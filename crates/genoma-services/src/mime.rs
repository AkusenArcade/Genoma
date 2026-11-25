//! What a file is, and what opens it (`@genoma/mime`): the freedesktop
//! shared MIME database (names first, then the content's magic for what a
//! name leaves open), the icon each type takes, and the applications
//! `mimeapps.list` and the desktop entries associate with it.
//!
//! The database is immutable once read and shared between threads: a
//! folder's listing types its entries off the loop. It is read again when
//! one of its files changes.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use crate::apps::{App, Apps, data_dirs, languages};

pub const DIRECTORY: &str = "inode/directory";
pub const UNKNOWN: &str = "application/octet-stream";
const EMPTY: &str = "application/x-zerosize";
const TEXT: &str = "text/plain";

/// The most read of a file for its magic. Rules that look further (a few
/// disk images) go unmatched rather than read a quarter of a megabyte of
/// every file without a name that says it.
const SNIFF: usize = 16 << 10;

#[derive(Clone, Debug, PartialEq)]
struct Glob {
    weight: u32,
    mime: Arc<str>,
    /// Matched only in the case it is written in (`*.C` against `*.c`).
    exact: bool,
}

/// One magic rule's line, with the lines under it.
#[derive(Debug)]
struct Magic {
    offset: usize,
    range: usize,
    value: Vec<u8>,
    mask: Option<Vec<u8>>,
    under: Vec<Magic>,
}

#[derive(Debug)]
struct MagicRule {
    priority: u32,
    mime: Arc<str>,
    any: Vec<Magic>,
}

/// The shared MIME database, as read.
#[derive(Debug, Default)]
pub struct Database {
    dirs: Vec<PathBuf>,
    /// Whole names (`Makefile`).
    names: HashMap<String, Vec<Glob>>,
    /// What follows the star of `*.tar.gz`, `*~`.
    endings: HashMap<String, Vec<Glob>>,
    /// Which bytes an ending begins with, so a name is looked up only
    /// where one could start.
    starts: HashSet<u8>,
    longest: usize,
    /// Everything else, matched one by one.
    patterns: Vec<(String, Glob)>,
    magic: Vec<MagicRule>,
    aliases: HashMap<String, String>,
    parents: HashMap<String, Vec<String>>,
    icons: HashMap<String, String>,
    generic: HashMap<String, String>,
}

/// What a name says of a file's type.
#[derive(Clone, Debug, PartialEq)]
pub enum Named {
    /// One type.
    One(Arc<str>),
    /// Several of the same weight, the first to be taken if the content
    /// does not choose.
    Several(Vec<Arc<str>>),
    Nothing,
}

impl Database {
    pub fn read(dirs: &[PathBuf]) -> Database {
        let mut db = Database { dirs: dirs.to_vec(), ..Default::default() };
        // The least important directory first: a later one replaces.
        for dir in dirs.iter().rev() {
            let dir = dir.join("mime");
            if let Ok(text) = fs::read_to_string(dir.join("globs2")) {
                db.globs(&text);
            }
            let pairs = |name: &str, sep: char| -> Vec<(String, String)> {
                fs::read_to_string(dir.join(name))
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.starts_with('#'))
                    .filter_map(|l| l.split_once(sep).map(|(a, b)| (a.trim().to_owned(), b.trim().to_owned())))
                    .collect()
            };
            db.aliases.extend(pairs("aliases", ' '));
            for (child, parent) in pairs("subclasses", ' ') {
                let all = db.parents.entry(child).or_default();
                if !all.contains(&parent) {
                    all.push(parent);
                }
            }
            db.icons.extend(pairs("icons", ':'));
            db.generic.extend(pairs("generic-icons", ':'));
            if let Ok(bytes) = fs::read(dir.join("magic")) {
                db.magic(&bytes);
            }
        }
        db.magic.sort_by_key(|r| std::cmp::Reverse(r.priority));
        db.starts = db.endings.keys().filter_map(|k| k.bytes().next()).collect();
        db.longest = db.endings.keys().map(String::len).max().unwrap_or(0);
        db
    }

    /// `globs2`: `weight:type:pattern[:flags]` a line.
    fn globs(&mut self, text: &str) {
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let mut parts = line.splitn(4, ':');
            let (Some(weight), Some(mime), Some(pattern)) = (parts.next(), parts.next(), parts.next()) else { continue };
            if pattern == "__NOGLOBS__" {
                // What the directories before said of this type is undone.
                let other = |g: &Glob| &*g.mime != mime;
                self.names.values_mut().for_each(|v| v.retain(other));
                self.endings.values_mut().for_each(|v| v.retain(other));
                self.patterns.retain(|(_, g)| other(g));
                continue;
            }
            let exact = parts.next().is_some_and(|f| f.split(',').any(|f| f == "cs"));
            let glob = Glob { weight: weight.parse().unwrap_or(50), mime: mime.into(), exact };
            let wild = |p: &str| p.contains(['*', '?', '[']);
            let key = |p: &str| if exact { p.to_owned() } else { p.to_lowercase() };
            let into = match pattern.strip_prefix('*') {
                Some(ending) if !ending.is_empty() && !wild(ending) => self.endings.entry(key(ending)).or_default(),
                _ if !wild(pattern) => self.names.entry(key(pattern)).or_default(),
                _ => {
                    self.patterns.push((key(pattern), glob));
                    continue;
                }
            };
            if !into.contains(&glob) {
                into.push(glob);
            }
        }
    }

    /// The `magic` file: `[priority:type]` sections of rules, each
    /// `indent>offset=<length><value>[&mask][~word][+range]`.
    fn magic(&mut self, bytes: &[u8]) {
        let Some(mut at) = bytes.strip_prefix(b"MIME-Magic\0\n") else { return };
        while at.first() == Some(&b'[') {
            let Some(end) = at.iter().position(|&b| b == b'\n') else { return };
            let head = String::from_utf8_lossy(&at[1..end]).trim_end_matches(']').to_owned();
            at = &at[end + 1..];
            let mut lines: Vec<(usize, Magic)> = Vec::new();
            while let Some(&b) = at.first()
                && b != b'['
            {
                let Some((indent, rule, rest)) = magic_line(at) else { return };
                lines.push((indent, rule));
                at = rest;
            }
            let Some((priority, mime)) = head.split_once(':') else { continue };
            let mut lines = lines.into_iter().peekable();
            let any = nested(&mut lines, 0);
            self.magic.push(MagicRule { priority: priority.parse().unwrap_or(50), mime: mime.into(), any });
        }
    }

    /// What `name` says, without looking at the file: the heaviest glob's
    /// type, the longest of equals; then, for the content to choose among,
    /// the other types of globs as long.
    pub fn by_name(&self, name: &str) -> Named {
        let lower = name.to_lowercase();
        let mut found: Vec<(u32, usize, Arc<str>)> = Vec::new();
        self.matches(name, true, &mut found);
        self.matches(&lower, false, &mut found);
        found.sort_by_key(|(weight, length, _)| std::cmp::Reverse((*weight, *length)));
        let Some(longest) = found.first().map(|(_, l, _)| *l) else { return Named::Nothing };
        let mut types: Vec<Arc<str>> = Vec::new();
        for (_, length, mime) in found {
            if length == longest && !types.contains(&mime) {
                types.push(mime);
            }
        }
        if types.len() == 1 { Named::One(types.remove(0)) } else { Named::Several(types) }
    }

    /// Every glob `name` matches: weight, the pattern's length, its type.
    /// The exact ones are kept as written, the others in lower case, and
    /// so is the name they are looked up by.
    fn matches(&self, name: &str, exact: bool, found: &mut Vec<(u32, usize, Arc<str>)>) {
        let mut take = |globs: &[Glob], length: usize| {
            found.extend(globs.iter().filter(|g| g.exact == exact).map(|g| (g.weight, length, g.mime.clone())));
        };
        if let Some(globs) = self.names.get(name) {
            take(globs, name.len());
        }
        let from = name.len().saturating_sub(self.longest);
        for i in from..name.len() {
            // Not from the first byte: `*.txt` is not what `.txt` is.
            if i == 0 || !self.starts.contains(&name.as_bytes()[i]) || !name.is_char_boundary(i) {
                continue;
            }
            if let Some(globs) = self.endings.get(&name[i..]) {
                take(globs, name.len() - i);
            }
        }
        for (pattern, glob) in &self.patterns {
            if glob.exact == exact && wildcard(pattern.as_bytes(), name.as_bytes()) {
                take(std::slice::from_ref(glob), pattern.len());
            }
        }
    }

    /// The type whose magic `data`, a file's beginning, matches.
    pub fn by_content(&self, data: &[u8]) -> Option<Arc<str>> {
        self.magic.iter().find(|r| r.any.iter().any(|m| m.matches(data))).map(|r| r.mime.clone())
    }

    /// What `data`, a file's beginning, says: its magic, else text or bytes.
    fn sniffed(&self, data: &[u8]) -> Arc<str> {
        self.by_content(data).unwrap_or_else(|| if textual(data) { TEXT.into() } else { UNKNOWN.into() })
    }

    /// `name`'s type, the content read only when the name leaves it open.
    pub fn of(&self, path: &Path) -> Arc<str> {
        let Ok(meta) = fs::symlink_metadata(path) else { return UNKNOWN.into() };
        let meta = if meta.is_symlink() {
            match fs::metadata(path) {
                Ok(m) => m,
                Err(_) => return "inode/symlink".into(),
            }
        } else {
            meta
        };
        if let Some(special) = special(&meta.file_type()) {
            return special.into();
        }
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        self.resolve(self.by_name(&name), path, meta.len())
    }

    /// A regular file's type from what its name said.
    pub fn resolve(&self, named: Named, path: &Path, size: u64) -> Arc<str> {
        match named {
            Named::One(mime) => mime,
            Named::Several(types) => {
                let seen = beginning(path).map(|d| self.sniffed(&d));
                seen.and_then(|s| {
                    let same = types.iter().find(|t| **t == s);
                    same.or_else(|| types.iter().find(|t| s.as_ref() != UNKNOWN && self.is_a(t, &s))).or_else(|| types.iter().find(|t| self.is_a(&s, t))).cloned()
                })
                .unwrap_or_else(|| types[0].clone())
            }
            Named::Nothing if size == 0 => EMPTY.into(),
            Named::Nothing => match beginning(path) {
                None => UNKNOWN.into(),
                Some(data) => self.sniffed(&data),
            },
        }
    }

    /// The name a type goes by now (`application/x-pdf` → `application/pdf`).
    pub fn canonical<'a>(&'a self, mime: &'a str) -> &'a str {
        self.aliases.get(mime).map_or(mime, String::as_str)
    }

    /// What a type is a kind of, the nearest first: every text is
    /// `text/plain`, every file a stream of bytes.
    pub fn parents(&self, mime: &str) -> Vec<String> {
        let mime = self.canonical(mime);
        let mut out: Vec<String> =
            self.parents.get(mime).map(|p| p.iter().map(|p| self.canonical(p).to_owned()).collect()).unwrap_or_default();
        if out.is_empty() {
            if mime.starts_with("text/") && mime != TEXT {
                out.push(TEXT.into());
            } else if !mime.starts_with("inode/") && !mime.starts_with("x-scheme-handler/") && mime != UNKNOWN {
                out.push(UNKNOWN.into());
            }
        }
        out
    }

    /// The type itself, then what it is a kind of, breadth first, each once.
    pub fn lineage(&self, mime: &str) -> Vec<String> {
        let mut out = vec![self.canonical(mime).to_owned()];
        let mut i = 0;
        while i < out.len() {
            for p in self.parents(&out[i].clone()) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        // A stream of bytes says the least: last.
        if let Some(at) = out.iter().position(|m| m == UNKNOWN) {
            let m = out.remove(at);
            out.push(m);
        }
        out
    }

    pub fn is_a(&self, mime: &str, parent: &str) -> bool {
        let parent = self.canonical(parent);
        self.lineage(mime).iter().any(|m| m == parent)
    }

    /// The icon names to look for, the most precise first.
    pub fn icons(&self, mime: &str) -> Vec<String> {
        let mime = self.canonical(mime);
        let plain = mime.replace('/', "-");
        let mut out = Vec::new();
        out.extend(self.icons.get(mime).cloned());
        if !out.contains(&plain) {
            out.push(plain);
        }
        let generic = self
            .generic
            .get(mime)
            .cloned()
            .unwrap_or_else(|| format!("{}-x-generic", mime.split('/').next().unwrap_or("text")));
        if !out.contains(&generic) {
            out.push(generic);
        }
        out
    }

    /// What a type is called, in the first of `languages` the database
    /// has it in, else in English; the type itself when nobody says.
    pub fn description(&self, mime: &str, languages: &[String]) -> String {
        let mime = self.canonical(mime);
        for dir in &self.dirs {
            let Ok(text) = fs::read_to_string(dir.join("mime").join(format!("{mime}.xml"))) else { continue };
            let comments = comments(&text);
            let found = languages
                .iter()
                .find_map(|l| comments.iter().find(|(lang, _)| lang.as_deref() == Some(l)))
                .or_else(|| comments.iter().find(|(lang, _)| lang.is_none()));
            if let Some((_, c)) = found {
                return c.clone();
            }
        }
        mime.to_owned()
    }
}

/// The type of what is neither a regular file nor a link.
pub fn special(kind: &fs::FileType) -> Option<&'static str> {
    if kind.is_dir() {
        Some(DIRECTORY)
    } else if kind.is_fifo() {
        Some("inode/fifo")
    } else if kind.is_socket() {
        Some("inode/socket")
    } else if kind.is_block_device() {
        Some("inode/blockdevice")
    } else if kind.is_char_device() {
        Some("inode/chardevice")
    } else {
        None
    }
}

fn beginning(path: &Path) -> Option<Vec<u8>> {
    let mut data = Vec::with_capacity(SNIFF);
    File::open(path).ok()?.take(SNIFF as u64).read_to_end(&mut data).ok()?;
    Some(data)
}

/// Text: UTF-8 (a character cut at the end aside) with no control
/// characters but the ones text has.
fn textual(data: &[u8]) -> bool {
    let valid = match std::str::from_utf8(data) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && data.len() - e.valid_up_to() < 4,
    };
    valid && !data.iter().any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c | 0x1b | 0x08) || b == 0x7f)
}

/// A shell glob against a whole name: `*`, `?`, `[a-z]`.
fn wildcard(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some(b'*') => (0..=name.len()).any(|i| wildcard(&pattern[1..], &name[i..])),
        Some(b'?') => !name.is_empty() && wildcard(&pattern[1..], &name[1..]),
        Some(b'[') => {
            let Some(end) = pattern.iter().skip(2).position(|&b| b == b']').map(|i| i + 2) else {
                return name.first() == Some(&b'[') && wildcard(&pattern[1..], &name[1..]);
            };
            let Some(&c) = name.first() else { return false };
            let (not, set) = match pattern[1] {
                b'!' | b'^' => (true, &pattern[2..end]),
                _ => (false, &pattern[1..end]),
            };
            let mut inside = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == b'-' {
                    inside |= (set[i]..=set[i + 2]).contains(&c);
                    i += 3;
                } else {
                    inside |= set[i] == c;
                    i += 1;
                }
            }
            inside != not && wildcard(&pattern[end + 1..], &name[1..])
        }
        Some(&p) => name.first() == Some(&p) && wildcard(&pattern[1..], &name[1..]),
    }
}

impl Magic {
    fn matches(&self, data: &[u8]) -> bool {
        let here = (self.offset..self.offset + self.range.max(1)).any(|at| {
            data.get(at..at + self.value.len()).is_some_and(|seen| match &self.mask {
                None => seen == self.value,
                Some(mask) => seen.iter().zip(&self.value).zip(mask).all(|((s, v), m)| s & m == v & m),
            })
        });
        here && (self.under.is_empty() || self.under.iter().any(|m| m.matches(data)))
    }
}

/// One line of the magic file: its indent, the rule, and what follows it.
fn magic_line(at: &[u8]) -> Option<(usize, Magic, &[u8])> {
    let number = |at: &[u8]| -> (usize, usize) {
        let digits = at.iter().take_while(|b| b.is_ascii_digit()).count();
        (std::str::from_utf8(&at[..digits]).ok().and_then(|d| d.parse().ok()).unwrap_or(0), digits)
    };
    let (indent, used) = number(at);
    let at = at.get(used..)?.strip_prefix(b">")?;
    let (offset, used) = number(at);
    let at = at.get(used..)?.strip_prefix(b"=")?;
    let length = u16::from_be_bytes([*at.first()?, *at.get(1)?]) as usize;
    let mut value = at.get(2..2 + length)?.to_vec();
    let mut at = at.get(2 + length..)?;
    let mut mask = None;
    let (mut word, mut range) = (1, 1);
    loop {
        match at.first()? {
            b'&' => {
                mask = Some(at.get(1..1 + length)?.to_vec());
                at = at.get(1 + length..)?;
            }
            b'~' => {
                let (w, used) = number(&at[1..]);
                word = w;
                at = at.get(1 + used..)?;
            }
            b'+' => {
                let (r, used) = number(&at[1..]);
                range = r;
                at = at.get(1 + used..)?;
            }
            b'\n' => {
                at = &at[1..];
                break;
            }
            // Something newer than this: the rest of the line passed over.
            _ => {
                let end = at.iter().position(|&b| b == b'\n')?;
                at = &at[end + 1..];
                break;
            }
        }
    }
    // A value in the host's byte order is written big-endian.
    if cfg!(target_endian = "little") && word > 1 {
        for bytes in [Some(&mut value), mask.as_mut()].into_iter().flatten() {
            for w in bytes.chunks_exact_mut(word) {
                w.reverse();
            }
        }
    }
    Some((indent, Magic { offset, range, value, mask, under: Vec::new() }, at))
}

/// The rules at `indent`, each with the ones under it.
fn nested(lines: &mut std::iter::Peekable<std::vec::IntoIter<(usize, Magic)>>, indent: usize) -> Vec<Magic> {
    let mut out: Vec<Magic> = Vec::new();
    while let Some((at, _)) = lines.peek() {
        if *at < indent {
            break;
        }
        if *at > indent {
            let under = nested(lines, indent + 1);
            match out.last_mut() {
                Some(m) => m.under.extend(under),
                None => out.extend(under),
            }
            continue;
        }
        if let Some((_, m)) = lines.next() {
            out.push(m);
        }
    }
    out
}

/// The `<comment>` elements of a type's XML: language and text.
fn comments(xml: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<comment") {
        rest = &rest[i + "<comment".len()..];
        let Some(open) = rest.find('>') else { break };
        let Some(close) = rest.find("</comment>") else { break };
        if close < open {
            continue;
        }
        let lang = rest[..open].split("xml:lang=\"").nth(1).and_then(|l| l.split('"').next()).map(str::to_owned);
        let text = rest[open + 1..close]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        out.push((lang, text));
        rest = &rest[close..];
    }
    out
}

/// When the files the database is made of last changed.
fn stamp(dirs: &[PathBuf]) -> Vec<Option<SystemTime>> {
    dirs.iter().map(|d| fs::metadata(d.join("mime/globs2")).and_then(|m| m.modified()).ok()).collect()
}

/// The database, and what it was read from.
struct Loaded {
    stamp: Vec<Option<SystemTime>>,
    db: Arc<Database>,
}

static DATABASE: Mutex<Option<Loaded>> = Mutex::new(None);

/// The system's database: read the first time, and again once it changed.
pub fn database() -> Arc<Database> {
    let dirs = data_dirs();
    let now = stamp(&dirs);
    let mut read = DATABASE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = read.as_ref()
        && r.stamp == now
    {
        return r.db.clone();
    }
    let db = Arc::new(Database::read(&dirs));
    *read = Some(Loaded { stamp: now, db: db.clone() });
    db
}

// ---- what opens a type -------------------------------------------------------

/// A `mimeapps.list` or `mimeinfo.cache`: group, type, desktop ids.
#[derive(Debug, Default, PartialEq)]
struct Associations {
    default: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
    cache: HashMap<String, Vec<String>>,
}

impl Associations {
    fn parse(text: &str) -> Associations {
        let mut out = Associations::default();
        let mut group = None;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                group = match line {
                    "[Default Applications]" => Some(&mut out.default),
                    "[Added Associations]" => Some(&mut out.added),
                    "[Removed Associations]" => Some(&mut out.removed),
                    "[MIME Cache]" => Some(&mut out.cache),
                    _ => None,
                };
                continue;
            }
            if let (Some(into), Some((mime, ids))) = (group.as_mut(), line.split_once('=')) {
                let ids = ids.split(';').map(str::trim).filter(|s| !s.is_empty());
                into.entry(mime.trim().to_owned()).or_default().extend(ids.map(|id| id.strip_suffix(".desktop").unwrap_or(id).to_owned()));
            }
        }
        out
    }
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
}

/// Where associations are said, the most important first.
fn association_files(config: &Path, data: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = vec![config.to_owned()];
    let system = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.extend(data.iter().map(|d| d.join("applications")));
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    let mut out = Vec::new();
    for dir in &dirs {
        for d in desktop.split(':').filter(|d| !d.is_empty()) {
            out.push(dir.join(format!("{d}-mimeapps.list")));
        }
        out.push(dir.join("mimeapps.list"));
    }
    out.extend(data.iter().map(|d| d.join("applications/mimeinfo.cache")));
    out
}

/// What the files say opens `types` (a type, then what it is a kind of):
/// ids, the one that opens it first, then the others in the order found.
fn openers(files: &[Associations], types: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for mime in types {
        let mut removed: HashSet<&str> = HashSet::new();
        let take = |id: &str, removed: &HashSet<&str>, out: &mut Vec<String>| {
            if !removed.contains(id) && !out.iter().any(|o| o == id) {
                out.push(id.to_owned());
            }
        };
        // The default first, then what is associated, the files in order.
        for f in files {
            for id in f.default.get(mime).into_iter().flatten() {
                take(id, &removed, &mut out);
            }
        }
        for f in files {
            for id in f.added.get(mime).into_iter().flatten() {
                take(id, &removed, &mut out);
            }
            // What this file takes away, the ones after it no longer give.
            removed.extend(f.removed.get(mime).into_iter().flatten().map(String::as_str));
            for id in f.cache.get(mime).into_iter().flatten() {
                take(id, &removed, &mut out);
            }
        }
    }
    out
}

/// A `mimeapps.list` with `id` made the default for `mime`, associated
/// with it and no longer removed from it; the rest as it was.
fn with_default(text: &str, mime: &str, id: &str) -> String {
    let file = format!("{id}.desktop");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut edit = |group: &str, change: &dyn Fn(Vec<String>) -> Vec<String>| {
        let start = lines.iter().position(|l| l.trim() == group);
        let Some(start) = start else {
            let ids = change(Vec::new());
            if !ids.is_empty() {
                if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(group.to_owned());
                lines.push(format!("{mime}={};", ids.join(";")));
            }
            return;
        };
        let end = lines[start + 1..].iter().position(|l| l.trim().starts_with('[')).map_or(lines.len(), |i| start + 1 + i);
        let at = lines[start + 1..end].iter().position(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == mime)).map(|i| start + 1 + i);
        let had: Vec<String> = at
            .and_then(|i| lines[i].split_once('=').map(|(_, v)| v.split(';').filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_owned()).collect()))
            .unwrap_or_default();
        let ids = change(had.clone());
        match at {
            // Already so: the line is left as it is written.
            Some(_) if ids == had => {}
            Some(i) if ids.is_empty() => {
                lines.remove(i);
            }
            Some(i) => lines[i] = format!("{mime}={};", ids.join(";")),
            None if ids.is_empty() => {}
            None => {
                // After the group's last line, before the blank ones.
                let mut to = end;
                while to > start + 1 && lines[to - 1].trim().is_empty() {
                    to -= 1;
                }
                lines.insert(to, format!("{mime}={};", ids.join(";")));
            }
        }
    };
    edit("[Default Applications]", &|_| vec![file.clone()]);
    edit("[Added Associations]", &|had| {
        let mut ids = vec![file.clone()];
        ids.extend(had.into_iter().filter(|x| *x != file));
        ids
    });
    edit("[Removed Associations]", &|had| had.into_iter().filter(|x| *x != file).collect());
    lines.join("\n") + "\n"
}

/// Every association file as it is now.
fn associations() -> Vec<Associations> {
    association_files(&config_home(), &data_dirs())
        .iter()
        .filter_map(|f| fs::read_to_string(f).ok())
        .map(|t| Associations::parse(&t))
        .collect()
}

/// The applications that open `mime`, the one that does by default first.
pub fn apps(apps: &Apps, mime: &str) -> Vec<App> {
    apps.entries(&openers(&associations(), &database().lineage(mime)))
}

/// Makes `id` what opens `mime`, in the user's own `mimeapps.list`.
pub fn set_default(apps: &Apps, mime: &str, id: &str) -> Result<(), String> {
    apps.entry(id).ok_or_else(|| format!("no application {id}"))?;
    let path = config_home().join("mimeapps.list");
    // Through a link (a dotfiles repository's), the file it names.
    let path = fs::canonicalize(&path).unwrap_or(path);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let next = with_default(&text, database().canonical(mime), id);
    if next == text {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let beside = path.with_extension("list.genoma");
    fs::write(&beside, next).and_then(|()| fs::rename(&beside, &path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// What a type is called, in the user's language.
pub fn description(mime: &str) -> String {
    database().description(mime, &languages())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("genoma-mime-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mime/image")).unwrap();
        dir
    }

    fn small(name: &str) -> (PathBuf, Database) {
        let dir = scratch(name);
        fs::write(
            dir.join("mime/globs2"),
            "# a comment\n80:text/special:*.special\n50:image/png:*.png\n50:text/x-c++src:*.C:cs\n50:text/x-csrc:*.c:cs\n\
             50:application/gzip:*.gz\n50:application/x-compressed-tar:*.tar.gz\n50:text/x-makefile:Makefile\n\
             50:text/x-readme:README*\n50:application/x-a:*.amb\n50:image/png:*.amb\n50:application/x-trash:*~\n",
        )
        .unwrap();
        fs::write(dir.join("mime/aliases"), "image/x-png image/png\n").unwrap();
        fs::write(dir.join("mime/subclasses"), "text/x-c++src text/x-csrc\n").unwrap();
        fs::write(dir.join("mime/generic-icons"), "application/gzip:package-x-generic\n").unwrap();
        // "\x89PNG" at 0; and one nested under an offset range: "AB" in the
        // first four bytes, with "Z" at 6.
        let mut magic = b"MIME-Magic\0\n[50:image/png]\n>0=\0\x04\x89PNG\n[60:application/x-ab]\n>0=\0\x02AB+4\n1>6=\0\x01Z\n".to_vec();
        magic.extend_from_slice(b"[40:application/x-word]\n>0=\0\x02\x12\x34~2\n[40:application/x-mask]\n>0=\0\x01\x40&\xf0\n");
        fs::write(dir.join("mime/magic"), magic).unwrap();
        fs::write(
            dir.join("mime/image/png.xml"),
            "<mime-type type=\"image/png\"><comment>PNG image</comment><comment xml:lang=\"it\">Immagine PNG &amp; co</comment></mime-type>",
        )
        .unwrap();
        let db = Database::read(std::slice::from_ref(&dir));
        (dir, db)
    }

    fn one(db: &Database, name: &str) -> String {
        match db.by_name(name) {
            Named::One(m) => m.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn a_name_says_the_type() {
        let (_, db) = small("names");
        assert_eq!(one(&db, "a.png"), "image/png");
        assert_eq!(one(&db, "A.PNG"), "image/png");
        assert_eq!(one(&db, "x.C"), "text/x-c++src");
        assert_eq!(one(&db, "x.c"), "text/x-csrc");
        assert_eq!(one(&db, "a.tar.gz"), "application/x-compressed-tar", "the longest");
        assert_eq!(one(&db, "è.gz"), "application/gzip");
        assert_eq!(one(&db, "Makefile"), "text/x-makefile");
        assert_eq!(one(&db, "MAKEFILE"), "text/x-makefile", "not an exact name");
        assert_eq!(one(&db, "README.first"), "text/x-readme");
        assert_eq!(one(&db, "README.gz"), "text/x-readme", "the longer pattern, whatever its case");
        assert_eq!(one(&db, "notes~"), "application/x-trash");
        assert_eq!(db.by_name(".png"), Named::Nothing, "a hidden file with no ending");
        assert_eq!(db.by_name("thing"), Named::Nothing);
        assert!(matches!(db.by_name("a.amb"), Named::Several(_)));
    }

    #[test]
    fn the_content_says_what_the_name_does_not() {
        let (dir, db) = small("content");
        let file = |name: &str, data: &[u8]| {
            fs::write(dir.join(name), data).unwrap();
            db.of(&dir.join(name)).to_string()
        };
        assert_eq!(file("picture", b"\x89PNG and more"), "image/png");
        assert_eq!(file("b.amb", b"\x89PNG"), "image/png", "chosen among the name's");
        assert_eq!(file("c.amb", b"whatever"), "application/x-a", "the first when it does not choose");
        assert_eq!(file("ab", b"..AB..Z"), "application/x-ab");
        assert_eq!(file("ab2", b"..AB..Y"), "text/plain", "the rule under it must match too");
        assert_eq!(file("word", b"\x34\x12\x00"), "application/x-word", "in the host's order");
        assert_eq!(file("mask", b"\x4f\x00"), "application/x-mask");
        assert_eq!(file("text", "perché no\n".as_bytes()), "text/plain");
        assert_eq!(file("bytes", b"\x00\x01\x02"), UNKNOWN);
        assert_eq!(file("empty", b""), EMPTY);
        assert_eq!(&*db.of(&dir), DIRECTORY);
        std::os::unix::fs::symlink("nowhere", dir.join("broken")).unwrap();
        assert_eq!(&*db.of(&dir.join("broken")), "inode/symlink");
    }

    #[test]
    fn a_type_has_a_lineage_icons_and_a_name() {
        let (_, db) = small("lineage");
        assert_eq!(db.canonical("image/x-png"), "image/png");
        assert_eq!(db.lineage("text/x-c++src"), ["text/x-c++src", "text/x-csrc", "text/plain", UNKNOWN]);
        assert!(db.is_a("text/x-c++src", "text/plain"));
        assert!(!db.is_a(DIRECTORY, UNKNOWN));
        assert_eq!(db.icons("image/x-png"), ["image-png", "image-x-generic"]);
        assert_eq!(db.icons("application/gzip"), ["application-gzip", "package-x-generic"]);
        assert_eq!(db.description("image/png", &["it".into()]), "Immagine PNG & co");
        assert_eq!(db.description("image/png", &["de".into()]), "PNG image");
        assert_eq!(db.description("image/none", &[]), "image/none");
    }

    #[test]
    fn a_nearer_directory_undoes_a_farther_ones_globs() {
        let far = scratch("far");
        let near = scratch("near");
        fs::write(far.join("mime/globs2"), "50:a/x:*.x\n50:a/y:*.y\n").unwrap();
        fs::write(near.join("mime/globs2"), "50:a/x:__NOGLOBS__\n50:a/x:*.xx\n").unwrap();
        let db = Database::read(&[near, far]);
        assert_eq!(db.by_name("f.x"), Named::Nothing);
        assert_eq!(one(&db, "f.xx"), "a/x");
        assert_eq!(one(&db, "f.y"), "a/y");
    }

    #[test]
    fn what_opens_a_type() {
        let user = Associations::parse(
            "[Default Applications]\nimage/png=gone.desktop;viewer.desktop\n[Added Associations]\nimage/png=editor.desktop;\n[Removed Associations]\nimage/png=browser.desktop;\n",
        );
        let cache = Associations::parse("[MIME Cache]\nimage/png=browser.desktop;painter.desktop;\ntext/plain=pad.desktop;\n");
        let types = ["image/png".to_string(), "text/plain".to_string()];
        assert_eq!(openers(&[user, cache], &types), ["gone", "viewer", "editor", "painter", "pad"]);
    }

    #[test]
    fn making_a_default_leaves_the_rest_as_it_was() {
        let text = "[Default Applications]\ntext/html=zen.desktop\nimage/png=a.desktop\n\n[Removed Associations]\nimage/png=b.desktop;c.desktop;\n";
        let next = with_default(text, "image/png", "b");
        assert_eq!(
            next,
            "[Default Applications]\ntext/html=zen.desktop\nimage/png=b.desktop;\n\n[Removed Associations]\nimage/png=c.desktop;\n\n[Added Associations]\nimage/png=b.desktop;\n"
        );
        assert_eq!(with_default(&next, "image/png", "b"), next, "said twice, the same");
        assert_eq!(with_default("", "a/b", "x"), "[Default Applications]\na/b=x.desktop;\n\n[Added Associations]\na/b=x.desktop;\n");
    }
}
