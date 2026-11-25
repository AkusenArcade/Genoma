//! A folder as a file manager sees it (`g.directory`): every entry with its
//! size, time and type, sorted and filtered. Everything here runs off the
//! loop: a folder of fifty thousand files takes a tenth of a second to
//! read, one on a disk asleep or across a network as long as it likes.
//!
//! A [`Listing`] is immutable. Each job makes the next from the one before,
//! so a change of a few names costs those names, and a new order no read.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::mime::{Database, Named, UNKNOWN, special};

/// How long files are opened for their types before what was found is shown.
const TELLING: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// As the system has it, byte for byte. What was found below the folder
    /// is called by its way from it: "a/b.txt".
    pub name: OsString,
    pub dir: bool,
    /// A symbolic link; what it points at decides the rest.
    pub link: bool,
    /// A link to nothing.
    pub broken: bool,
    pub executable: bool,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: f64,
    pub mime: Arc<str>,
    /// The name left the type open: the content has yet to say.
    unsure: bool,
    /// What sorts it among names.
    key: Box<[u8]>,
}

impl Entry {
    /// Itself, or something on the way to it.
    pub fn hidden(&self) -> bool {
        self.name.as_bytes().split(|b| *b == b'/').any(|part| part.first() == Some(&b'.'))
    }

    /// Its own name, and the way to it from the folder when it is below.
    pub fn leaf(&self) -> (&OsStr, Option<&OsStr>) {
        let bytes = self.name.as_bytes();
        match bytes.iter().rposition(|b| *b == b'/') {
            Some(at) => (OsStr::from_bytes(&bytes[at + 1..]), Some(OsStr::from_bytes(&bytes[..at]))),
            None => (&self.name, None),
        }
    }

    fn matches(&self, filter: &str) -> bool {
        folded(&self.leaf().0.to_string_lossy()).contains(filter)
    }

    /// The entry `name` of `dir` as it is now, or none.
    fn read(dir: &Path, name: &OsStr, db: &Database) -> Option<Entry> {
        let path = dir.join(name);
        let own = fs::symlink_metadata(&path).ok()?;
        let link = own.is_symlink();
        // Through a link, as a file manager shows it.
        let target = if link { fs::metadata(&path).ok() } else { None };
        let broken = link && target.is_none();
        let meta = target.unwrap_or(own);
        let text = Path::new(name).file_name().unwrap_or(name).to_string_lossy();
        let (mime, unsure) = if broken {
            ("inode/symlink".into(), false)
        } else if let Some(kind) = special(&meta.file_type()) {
            (kind.into(), false)
        } else {
            match db.by_name(&text) {
                Named::One(m) => (m, false),
                Named::Several(types) => (types[0].clone(), true),
                Named::Nothing => (UNKNOWN.into(), true),
            }
        };
        Some(Entry {
            name: name.to_owned(),
            dir: meta.is_dir(),
            link,
            broken,
            executable: meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            size: if meta.is_dir() || broken { 0 } else { meta.len() },
            modified: meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_secs_f64()),
            mime,
            unsure,
            key: natural(&text).into(),
        })
    }
}

/// `text` without case or accents: what a filter matches, what sorts first.
pub fn folded(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    text.nfkd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

/// A key that sorts names as people do: without case or accents, a run of
/// digits by its value ("2" before "10"), a hidden file as if it were not.
fn natural(name: &str) -> Vec<u8> {
    let folded = folded(name);
    let text = folded.trim_start_matches('.').as_bytes();
    let mut key = Vec::with_capacity(text.len() + 4);
    let mut i = 0;
    while i < text.len() {
        if !text[i].is_ascii_digit() {
            key.push(text[i]);
            i += 1;
            continue;
        }
        let end = text[i..].iter().position(|b| !b.is_ascii_digit()).map_or(text.len(), |n| i + n);
        let zeros = text[i..end].iter().take_while(|b| **b == b'0').count().min(end - i - 1);
        let digits = &text[i + zeros..end];
        // Where a digit sorts, then how many there are, then which.
        key.push(b'0');
        key.push(digits.len().min(255) as u8);
        key.extend_from_slice(digits);
        i = end;
    }
    key
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    Name,
    Size,
    Modified,
    Type,
}

impl By {
    pub fn from_name(name: &str) -> Option<By> {
        Some(match name {
            "name" => By::Name,
            "size" => By::Size,
            "modified" => By::Modified,
            "type" => By::Type,
            _ => return None,
        })
    }
}

/// What is shown of a folder, and in what order.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub hidden: bool,
    pub by: By,
    pub descending: bool,
    pub folders_first: bool,
    /// Only names containing it, whatever the case and the accents.
    pub filter: String,
    /// With a filter, what is in the folders below too.
    pub deep: bool,
}

impl Default for View {
    fn default() -> View {
        View { hidden: false, by: By::Name, descending: false, folders_first: true, filter: String::new(), deep: false }
    }
}

/// A folder as it was read.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub path: PathBuf,
    entries: Vec<Arc<Entry>>,
    /// What was found below it.
    found: Vec<Arc<Entry>>,
    /// What is shown of them, in order.
    pub shown: Vec<Arc<Entry>>,
    /// Why it could not be read.
    pub error: Option<String>,
}

/// Set ahead of `started`, a job under way is for nothing: it stops.
#[derive(Clone)]
pub struct Cancel {
    pub now: Arc<AtomicU64>,
    pub started: u64,
}

impl Cancel {
    fn cancelled(&self) -> bool {
        self.now.load(Ordering::Relaxed) != self.started
    }
}

impl Listing {
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    fn next(&self, entries: Vec<Arc<Entry>>, found: Vec<Arc<Entry>>, view: &View) -> Listing {
        let mut out = Listing { path: self.path.clone(), entries, found, shown: Vec::new(), error: self.error.clone() };
        out.arrange(view);
        out
    }

    /// The folder `path` read whole.
    pub fn read(path: &Path, view: &View, db: &Database, cancel: &Cancel) -> Listing {
        let mut out = Listing { path: path.to_owned(), ..Default::default() };
        match fs::read_dir(path) {
            Ok(read) => {
                for e in read {
                    if cancel.cancelled() {
                        return out;
                    }
                    match e {
                        Ok(e) => out.entries.extend(Entry::read(path, &e.file_name(), db).map(Arc::new)),
                        Err(e) => {
                            out.error = Some(reason(&e));
                            break;
                        }
                    }
                }
            }
            Err(e) => out.error = Some(reason(&e)),
        }
        out.arrange(view);
        out
    }

    /// The same folder with `names` read again: gone, new or changed. None
    /// when nothing is.
    pub fn updated(&self, names: &[OsString], view: &View, db: &Database) -> Option<Listing> {
        let at: HashMap<&OsStr, usize> = self.entries.iter().enumerate().map(|(i, e)| (e.name.as_os_str(), i)).collect();
        let mut next: Vec<Option<Arc<Entry>>> = self.entries.iter().cloned().map(Some).collect();
        let mut changed = false;
        for name in names {
            let mut now = Entry::read(&self.path, name, db);
            match at.get(name.as_os_str()).copied() {
                Some(i) => {
                    let had = next[i].as_deref();
                    // Told by its content already, and that the same: the
                    // type stays.
                    if let (Some(had), Some(now)) = (had, now.as_mut())
                        && !had.unsure
                        && now.unsure
                        && (had.size, had.modified) == (now.size, now.modified)
                    {
                        now.mime = had.mime.clone();
                        now.unsure = false;
                    }
                    if had != now.as_ref() {
                        next[i] = now.map(Arc::new);
                        changed = true;
                    }
                }
                None => {
                    if let Some(now) = now {
                        next.push(Some(Arc::new(now)));
                        changed = true;
                    }
                }
            }
        }
        // What was found in a folder that changed may have gone with it.
        let names: HashSet<&OsStr> = names.iter().map(|n| n.as_os_str()).collect();
        let first = |e: &Entry| Path::new(&e.name).iter().next().is_some_and(|n| names.contains(n));
        let found: Vec<Arc<Entry>> =
            self.found.iter().filter(|e| !first(e) || fs::symlink_metadata(self.path.join(&e.name)).is_ok()).cloned().collect();
        if !changed && found.len() == self.found.len() {
            return None;
        }
        Some(self.next(next.into_iter().flatten().collect(), found, view))
    }

    /// The same folder with what else was found below it.
    pub fn joined(&self, more: Vec<Arc<Entry>>, view: &View) -> Listing {
        let mut found = self.found.clone();
        {
            let had: HashSet<&OsStr> = self.found.iter().map(|e| e.name.as_os_str()).collect();
            found.extend(more.into_iter().filter(|e| !had.contains(e.name.as_os_str())));
        }
        // Several names looked for one after the other: the last one's only.
        if found.len() > MOST * 2 {
            let filter = folded(&view.filter);
            found.retain(|e| e.matches(&filter));
        }
        self.next(self.entries.clone(), found, view)
    }

    /// The folder read whole, keeping what was found below it before: of
    /// that, what is still there.
    pub fn again(&self, view: &View, db: &Database, cancel: &Cancel) -> Listing {
        let mut out = Listing::read(&self.path, view, db, cancel);
        out.found = self.found.iter().filter_map(|e| Entry::read(&self.path, &e.name, db).map(Arc::new)).collect();
        out.arrange(view);
        out
    }

    /// The same entries in another view.
    pub fn arranged(&self, view: &View) -> Listing {
        // What was found is kept for what is typed next, until nothing is.
        let found = if view.deep && !view.filter.is_empty() { self.found.clone() } else { Vec::new() };
        self.next(self.entries.clone(), found, view)
    }

    /// Whether any entry's type waits on its content.
    pub fn unsure(&self) -> bool {
        self.entries.iter().chain(&self.found).any(|e| e.unsure)
    }

    /// The types the names left open, from what the files hold, as many as
    /// `TELLING` allows: what the person asks meanwhile goes before the
    /// rest. Not over a network, where each would be a download: the
    /// name's guess stays.
    pub fn told(&self, view: &View, db: &Database, cancel: &Cancel) -> Listing {
        let far = remote(&self.path);
        let (mut entries, mut found) = (self.entries.clone(), self.found.clone());
        let started = Instant::now();
        for e in entries.iter_mut().chain(&mut found) {
            if cancel.cancelled() || started.elapsed() > TELLING {
                break;
            }
            if !e.unsure {
                continue;
            }
            let mut next = (**e).clone();
            next.unsure = false;
            if !far {
                next.mime = db.resolve(db.by_name(&e.leaf().0.to_string_lossy()), &self.path.join(&e.name), e.size);
            }
            *e = Arc::new(next);
        }
        self.next(entries, found, view)
    }

    fn arrange(&mut self, view: &View) {
        let filter = folded(&view.filter);
        let mut shown: Vec<Arc<Entry>> = self
            .entries
            .iter()
            .filter(|e| (view.hidden || !e.hidden()) && (filter.is_empty() || e.matches(&filter)))
            .cloned()
            .collect();
        if view.deep && !filter.is_empty() {
            shown.extend(self.found.iter().filter(|e| (view.hidden || !e.hidden()) && e.matches(&filter)).cloned());
        }
        // Looked for, what is nearer is seen first: this folder's own, then
        // those whose name begins so, then by how deep they are.
        let depth = |e: &Entry| e.name.as_bytes().iter().filter(|b| **b == b'/').count();
        let begins = |e: &Entry| folded(&e.leaf().0.to_string_lossy()).starts_with(&filter);
        let far: HashMap<&OsStr, (bool, bool, usize)> = match view.deep && !filter.is_empty() {
            true => shown.iter().map(|e| (e.name.as_os_str(), (depth(e) > 0, view.by == By::Name && !begins(e), depth(e)))).collect(),
            false => HashMap::new(),
        };
        let mut order = shown.clone();
        order.sort_unstable_by(|a, b| {
            let near = match far.is_empty() {
                true => std::cmp::Ordering::Equal,
                false => {
                    let (a, b) = (far[a.name.as_os_str()], far[b.name.as_os_str()]);
                    a.0.cmp(&b.0).then(if a.0 { a.1.cmp(&b.1).then(if view.by == By::Name { a.2.cmp(&b.2) } else { std::cmp::Ordering::Equal }) } else { std::cmp::Ordering::Equal })
                }
            };
            if near.is_ne() {
                return near;
            }
            let folders = if view.folders_first { b.dir.cmp(&a.dir) } else { std::cmp::Ordering::Equal };
            folders.then_with(|| {
                let name = || a.key.cmp(&b.key).then_with(|| a.name.cmp(&b.name));
                let by = match view.by {
                    By::Name => std::cmp::Ordering::Equal,
                    By::Size => a.size.cmp(&b.size),
                    By::Modified => a.modified.total_cmp(&b.modified),
                    By::Type => a.mime.cmp(&b.mime),
                };
                // Equal by what is asked, the names go up in either order.
                match (view.by, view.descending) {
                    (By::Name, true) => name().reverse(),
                    (_, true) => by.reverse().then_with(name),
                    (_, false) => by.then_with(name),
                }
            })
        });
        self.shown = order;
    }
}

/// The most that is looked for below a folder: past it, what is typed says
/// too little.
pub const MOST: usize = 10_000;

/// What a search below a folder tells as it goes.
pub struct Found {
    pub entries: Vec<Arc<Entry>>,
    /// Nothing more is coming.
    pub done: bool,
    /// Stopped at [`MOST`].
    pub capped: bool,
}

/// How often what was found is told.
const BATCH: Duration = Duration::from_millis(120);
/// Reading folders is waiting for a disk: a few at a time.
const WALKERS: usize = 4;

/// The folders still to look in, nearest first, and how many are being.
struct Ahead {
    folders: VecDeque<OsString>,
    open: usize,
}

/// What is below `path` whose name contains `filter`, told a few at a time.
/// Links are not followed, hidden folders only with `hidden`, and no disk
/// that is a network's is gone into.
pub fn search(path: &Path, filter: &str, hidden: bool, db: &Database, cancel: &Cancel, tell: &(dyn Fn(Found) + Sync)) {
    let filter = folded(filter);
    if filter.is_empty() || remote(path) {
        tell(Found { entries: Vec::new(), done: true, capped: false });
        return;
    }
    let device = rustix::fs::stat(path).map(|s| s.st_dev).unwrap_or(0);
    let ahead = Mutex::new(Ahead { folders: VecDeque::from([OsString::new()]), open: 0 });
    let more = Condvar::new();
    let batch = Mutex::new((Vec::new(), Instant::now()));
    let count = AtomicUsize::new(0);
    let over = || cancel.cancelled() || count.load(Ordering::Relaxed) >= MOST;
    // What was found is told a few at a time, and before a long silence.
    let flush = |push: Option<Arc<Entry>>| {
        let mut b = batch.lock().unwrap();
        b.0.extend(push);
        if !b.0.is_empty() && b.1.elapsed() > BATCH {
            b.1 = Instant::now();
            let entries = std::mem::take(&mut b.0);
            drop(b);
            tell(Found { entries, done: false, capped: false });
        }
    };
    let walk = || {
        loop {
            flush(None);
            let folder = {
                let mut a = ahead.lock().unwrap();
                loop {
                    if over() {
                        more.notify_all();
                        return;
                    }
                    if let Some(f) = a.folders.pop_front() {
                        a.open += 1;
                        break f;
                    }
                    if a.open == 0 {
                        more.notify_all();
                        return;
                    }
                    // Woken now and then, for a search that is for nothing.
                    a = more.wait_timeout(a, Duration::from_millis(200)).unwrap().0;
                }
            };
            let mut below: Vec<OsString> = Vec::new();
            for e in fs::read_dir(path.join(&folder)).into_iter().flatten().flatten() {
                if over() {
                    break;
                }
                let name = e.file_name();
                if !hidden && name.as_bytes().first() == Some(&b'.') {
                    continue;
                }
                let way = Path::new(&folder).join(&name).into_os_string();
                // Its own are the listing's.
                if !folder.is_empty() && folded(&name.to_string_lossy()).contains(&filter)
                    && let Some(entry) = Entry::read(path, &way, db)
                {
                    count.fetch_add(1, Ordering::Relaxed);
                    flush(Some(Arc::new(entry)));
                }
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    // Another disk is gone into only when it is here.
                    use std::os::unix::fs::MetadataExt;
                    let at = path.join(&way);
                    if e.metadata().is_ok_and(|m| m.dev() == device) || !(remote(&at) || of_the_kernel(&at)) {
                        below.push(way);
                    }
                }
            }
            let mut a = ahead.lock().unwrap();
            a.open -= 1;
            a.folders.extend(below);
            more.notify_all();
        }
    };
    std::thread::scope(|s| {
        for _ in 1..WALKERS {
            s.spawn(walk);
        }
        walk();
    });
    let entries = std::mem::take(&mut batch.lock().unwrap().0);
    tell(Found { entries, done: true, capped: count.load(Ordering::Relaxed) >= MOST });
}

/// Why a folder could not be read, in words.
fn reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => "not found".into(),
        ErrorKind::PermissionDenied => "permission denied".into(),
        ErrorKind::NotADirectory => "not a folder".into(),
        _ => e.to_string(),
    }
}

/// Whether `path` is on a filesystem whose files are elsewhere.
pub fn remote(path: &Path) -> bool {
    const FUSE: u64 = 0x6573_5546;
    const NFS: u64 = 0x6969;
    const CIFS: u64 = 0xff53_4d42;
    const SMB2: u64 = 0xfe53_4d42;
    const AFS: u64 = 0x5346_414f;
    const CEPH: u64 = 0x00c3_6400;
    const V9FS: u64 = 0x0102_1997;
    let Ok(fs_) = rustix::fs::statfs(path) else { return false };
    match fs_.f_type as u64 & 0xffff_ffff {
        // A drive read through FUSE (NTFS, exFAT) is here all the same.
        FUSE => !on_a_drive(path),
        kind => [NFS, CIFS, SMB2, AFS, CEPH, V9FS].contains(&kind),
    }
}

/// Whether what is there is not files but the kernel speaking.
fn of_the_kernel(path: &Path) -> bool {
    const PROC: u64 = 0x9fa0;
    const SYSFS: u64 = 0x6265_6572;
    const CGROUP2: u64 = 0x6367_7270;
    const DEBUGFS: u64 = 0x6462_6720;
    const TRACEFS: u64 = 0x7472_6163;
    const DEVPTS: u64 = 0x1cd1;
    const MQUEUE: u64 = 0x1980_0202;
    let Ok(fs_) = rustix::fs::statfs(path) else { return true };
    let kind = fs_.f_type as u64 & 0xffff_ffff;
    [PROC, SYSFS, CGROUP2, DEBUGFS, TRACEFS, DEVPTS, MQUEUE].contains(&kind)
}

/// Whether a FUSE filesystem is over a block device: `fuseblk` in the
/// mounts, found by the device number.
fn on_a_drive(path: &Path) -> bool {
    let Ok(st) = rustix::fs::stat(path) else { return false };
    let device = format!("{}:{}", rustix::fs::major(st.st_dev), rustix::fs::minor(st.st_dev));
    let Ok(mounts) = fs::read_to_string("/proc/self/mountinfo") else { return false };
    mounts.lines().any(|l| {
        let kind = l.split(" - ").nth(1).and_then(|r| r.split(' ').next());
        l.split(' ').nth(2) == Some(&device) && kind == Some("fuseblk")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cancel() -> Cancel {
        Cancel { now: Arc::default(), started: 0 }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("genoma-directory-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(l: &Listing) -> Vec<String> {
        l.shown.iter().map(|e| e.name.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn names_sort_as_people_do() {
        let mut list = ["b", "file10", "file2", "File1", "à", "a", "z", "file02", "e2", "É1", ".hidden", "h", "file"];
        list.sort_by(|a, b| natural(a).cmp(&natural(b)).then(a.cmp(b)));
        assert_eq!(list, ["a", "à", "b", "É1", "e2", "file", "File1", "file02", "file2", "file10", "h", ".hidden", "z"]);
    }

    #[test]
    fn a_folder_is_read_sorted_and_filtered() {
        let dir = scratch("read");
        for (name, size) in [("Beta.txt", 3), ("alpha.txt", 1), ("gamma.txt", 2), (".dot", 9)] {
            fs::write(dir.join(name), "x".repeat(size)).unwrap();
        }
        fs::create_dir(dir.join("zoo")).unwrap();
        std::os::unix::fs::symlink("zoo", dir.join("link")).unwrap();
        std::os::unix::fs::symlink("gone", dir.join("lost")).unwrap();
        let db = Database::default();
        let mut view = View::default();
        let l = Listing::read(&dir, &view, &db, &cancel());
        assert_eq!(names(&l), ["link", "zoo", "alpha.txt", "Beta.txt", "gamma.txt", "lost"]);
        assert!(l.shown[0].link && l.shown[0].dir && !l.shown[0].broken);
        assert!(l.shown[5].broken && &*l.shown[5].mime == "inode/symlink");
        assert_eq!(l.total(), 7);
        view.by = By::Size;
        view.descending = true;
        view.folders_first = false;
        view.hidden = true;
        assert_eq!(names(&l.arranged(&view))[..4], [".dot", "Beta.txt", "gamma.txt", "alpha.txt"]);
        view.filter = "ALPH".into();
        assert_eq!(names(&l.arranged(&view)), ["alpha.txt"]);
    }

    #[test]
    fn only_what_changed_is_read_again() {
        let dir = scratch("update");
        fs::write(dir.join("a"), "1").unwrap();
        fs::write(dir.join("b"), "1").unwrap();
        let (db, view) = (Database::default(), View::default());
        let l = Listing::read(&dir, &view, &db, &cancel()).told(&view, &db, &cancel());
        assert!(!l.unsure());
        assert_eq!(&*l.shown[0].mime, "text/plain");
        assert!(l.updated(&["a".into(), "nothing".into()], &view, &db).is_none(), "nothing changed");
        fs::remove_file(dir.join("a")).unwrap();
        fs::write(dir.join("b"), "longer").unwrap();
        fs::write(dir.join("c"), "new").unwrap();
        let n = l.updated(&["a".into(), "b".into(), "c".into()], &view, &db).unwrap();
        assert_eq!(names(&n), ["b", "c"]);
        assert_eq!(n.shown[0].size, 6);
        assert!(n.unsure(), "written since: told again");
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        let dir = scratch("error");
        fs::write(dir.join("file"), "").unwrap();
        let (db, view) = (Database::default(), View::default());
        let why = |p: &Path| Listing::read(p, &view, &db, &cancel()).error;
        assert_eq!(why(&dir.join("none")).as_deref(), Some("not found"));
        assert_eq!(why(&dir.join("file")).as_deref(), Some("not a folder"));
        assert_eq!(why(&dir), None);
    }

    #[test]
    fn what_is_below_is_found_nearest_first() {
        let dir = scratch("deep");
        for f in ["Report.txt", "other.txt", "a/my report.txt", "a/b/report 2.txt", "a/b/c/reports.md", ".hide/report.txt"] {
            fs::create_dir_all(dir.join(f).parent().unwrap()).unwrap();
            fs::write(dir.join(f), "x").unwrap();
        }
        fs::create_dir(dir.join("a/Report folder")).unwrap();
        std::os::unix::fs::symlink("..", dir.join("a/loop")).unwrap();
        let db = Database::default();
        let mut view = View { filter: "REPORT".into(), deep: true, ..Default::default() };
        let look = |hidden: bool| {
            let all = Mutex::new((Vec::new(), 0));
            search(&dir, "REPORT", hidden, &db, &cancel(), &|f: Found| {
                let mut a = all.lock().unwrap();
                a.0.extend(f.entries);
                a.1 += f.done as usize;
            });
            let (found, done) = all.into_inner().unwrap();
            assert_eq!(done, 1);
            found
        };
        let l = Listing::read(&dir, &view, &db, &cancel());
        assert_eq!(names(&l), ["Report.txt"]);
        let found = look(false);
        assert_eq!(found.len(), 4);
        let l = l.joined(found, &view);
        assert_eq!(names(&l), ["Report.txt", "a/Report folder", "a/b/report 2.txt", "a/b/c/reports.md", "a/my report.txt"]);
        let (leaf, within) = l.shown[2].leaf();
        assert_eq!((leaf, within), (OsStr::new("report 2.txt"), Some(OsStr::new("a/b"))));
        // Found twice is found once; what is hidden, only when asked for.
        let l = l.joined(look(true), &view);
        assert_eq!(l.shown.len(), 5);
        view.hidden = true;
        assert_eq!(l.arranged(&view).shown.len(), 6);
        // Another name keeps what still answers to it; none, nothing.
        view.filter = "reports".into();
        assert_eq!(names(&l.arranged(&view)), ["a/b/c/reports.md"]);
        // What went is forgotten.
        view.filter = "report".into();
        fs::remove_dir_all(dir.join("a/b")).unwrap();
        assert_eq!(l.updated(&["a".into()], &view, &db).unwrap().shown.len(), 4);
        assert_eq!(l.again(&view, &db, &cancel()).shown.len(), 4);
        view.filter.clear();
        let l = l.arranged(&view);
        assert!(l.found.is_empty() && l.shown.len() == 4);
    }

    #[test]
    fn a_name_that_is_not_text_stays_itself() {
        let dir = scratch("bytes");
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        fs::write(dir.join(name), "").unwrap();
        let (db, view) = (Database::default(), View::default());
        let l = Listing::read(&dir, &view, &db, &cancel());
        assert_eq!(l.shown[0].name, name);
    }
}
