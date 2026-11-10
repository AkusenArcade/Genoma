//! The applications the menu offers (`@genoma/apps`): their desktop entries
//! read in the user's language, found by what is typed, ranked by how often
//! each is started, started themselves, and recognised by the name a window
//! or a process gives.

use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use genoma_core::reactive::Signal;

/// One application, from its `.desktop` file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct App {
    /// The file name without `.desktop`: `org.gnome.Nautilus`.
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub icon: String,
    pub exec: String,
    pub path: String,
    pub terminal: bool,
    pub startup_class: String,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    /// What it offers beside starting (`[Desktop Action …]`): a new window,
    /// a private one.
    pub actions: Vec<Action>,
    pub file: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub exec: String,
}

/// How often and how lately an application was started from here.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub count: u32,
    /// Seconds since the epoch.
    pub last: u64,
}

pub struct Apps {
    /// Every application, by name; read on first asking (`all`), so an
    /// interface that never asks (a lock screen) reads nothing.
    list: Signal<Vec<App>>,
    read: std::cell::Cell<bool>,
    usage: RefCell<HashMap<String, Usage>>,
    usage_file: Option<PathBuf>,
}

impl Default for Apps {
    fn default() -> Self {
        Apps::new()
    }
}

impl Apps {
    pub fn new() -> Apps {
        let usage_file = state_dir().map(|d| d.join("genoma").join("apps.json"));
        let usage = usage_file.as_deref().map(read_usage).unwrap_or_default();
        Apps { list: Signal::new(Vec::new()), read: std::cell::Cell::new(false), usage: RefCell::new(usage), usage_file }
    }

    /// Every application, by name, read the first time it is asked for. A
    /// binding that reads it follows `refresh`.
    pub fn all(&self) -> Vec<App> {
        if !self.read.replace(true) {
            self.list.set(read_all(&data_dirs(), &languages()));
        }
        self.list.get()
    }

    /// Read again: an application installed or removed since.
    pub fn refresh(&self) {
        self.read.set(true);
        let now = read_all(&data_dirs(), &languages());
        if self.list.get_untracked().as_ref() != Some(&now) {
            self.list.set(now);
        }
    }

    pub fn usage(&self, id: &str) -> Usage {
        self.usage.borrow().get(id).copied().unwrap_or_default()
    }

    /// The applications that match `query`, best first; with nothing typed,
    /// the most used first, then by name. `limit` 0 is all of them.
    pub fn search(&self, query: &str, limit: usize) -> Vec<App> {
        let list = self.all();
        let usage = self.usage.borrow();
        let now = now();
        let mut scored: Vec<(f64, &App)> = list
            .iter()
            .filter_map(|app| {
                let found = score(app, query)?;
                let u = usage.get(&app.id).copied().unwrap_or_default();
                // Frequent and recent ones rise, never above a better match:
                // the boost is at most a third of a match step.
                let days = (now.saturating_sub(u.last)) as f64 / 86_400.0;
                let boost = (u.count.min(50) as f64).ln_1p() * 2.0 + if u.count > 0 { 3.0 / (1.0 + days) } else { 0.0 };
                Some((found + boost, app))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
        let take = if limit == 0 { scored.len() } else { limit };
        scored.into_iter().take(take).map(|(_, a)| a.clone()).collect()
    }

    /// The application a window's app_id or class, or a process's name,
    /// belongs to: the id, a reverse-DNS id's last part, the window class,
    /// the name; then, for a name of four letters or more, one that contains
    /// it or that it contains.
    pub fn find(&self, name: &str) -> Option<App> {
        let list = self.all();
        find_in(&list, name).cloned()
    }

    /// Starts `id` (or its action `action`), with `files` for its %f/%F/%u/%U,
    /// detached from the shell, and counts it. With an activation `token`
    /// (`g.activation_token`), its window is given the focus.
    pub fn launch(&self, id: &str, action: Option<&str>, files: &[String], token: Option<&str>) -> Result<(), String> {
        let list = genoma_core::reactive::untracked(|| self.all());
        let app = list.iter().find(|a| a.id == id).ok_or_else(|| format!("no application {id}"))?;
        let exec = match action {
            Some(a) => &app.actions.iter().find(|x| x.id == a).ok_or_else(|| format!("{id} has no action {a}"))?.exec,
            None => &app.exec,
        };
        let mut command = command_of(exec, app, files);
        if command.is_empty() {
            return Err(format!("{id}: nothing to run"));
        }
        if app.terminal {
            let mut wrapped = vec!["sh".to_owned(), "-c".to_owned(), IN_TERMINAL.to_owned(), "terminal".to_owned()];
            wrapped.append(&mut command);
            command = wrapped;
        }
        // In a session of its own (setsid -f): not the shell's child, so it
        // outlives a reload and is never left a zombie.
        let mut process = std::process::Command::new("setsid");
        process.arg("-f").args(&command).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if !app.path.is_empty() {
            process.current_dir(&app.path);
        }
        // What the shell was started with is its own, not the application's.
        process.env_remove("GENOMA_IPC").env_remove("GENOMA_LOG").env_remove("DESKTOP_STARTUP_ID").env_remove("XDG_ACTIVATION_TOKEN");
        // Its own token, under both names: Wayland's and the older one
        // GTK 3 and others still read.
        if let Some(token) = token {
            process.env("XDG_ACTIVATION_TOKEN", token).env("DESKTOP_STARTUP_ID", token);
        }
        let mut child = process.spawn().map_err(|e| format!("{id}: {e}"))?;
        // setsid -f returns at once, its child forked away.
        let _ = child.wait();
        self.count(id);
        Ok(())
    }

    fn count(&self, id: &str) {
        {
            let mut usage = self.usage.borrow_mut();
            let u = usage.entry(id.to_owned()).or_default();
            u.count += 1;
            u.last = now();
        }
        if let Some(file) = &self.usage_file {
            write_usage(file, &self.usage.borrow());
        }
    }
}

/// The desktop's terminal, asked the way the freedesktop proposal asks it
/// (`xdg-terminal-exec`), else $TERMINAL, else the first of the usual ones;
/// each takes the command after `-e`.
const IN_TERMINAL: &str = "if command -v xdg-terminal-exec >/dev/null 2>&1; then exec xdg-terminal-exec \"$@\"; fi; \
    for t in \"$TERMINAL\" kitty alacritty foot ghostty wezterm konsole gnome-terminal xterm; do \
    [ -n \"$t\" ] && command -v \"$t\" >/dev/null 2>&1 && exec \"$t\" -e \"$@\"; done; \
    echo \"no terminal for $1\" >&2; exit 1";

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
}

fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs = vec![std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(&home).join(".local/share"))];
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs
}

/// The user's languages, most specific first: `it_IT.UTF-8` gives
/// `it_IT`, `it`.
fn languages() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty())).unwrap_or_default();
    let base = raw.split(['.', '@']).next().unwrap_or_default();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let mut out = vec![base.to_owned()];
    if let Some((lang, _)) = base.split_once('_') {
        out.push(lang.to_owned());
    }
    out
}

fn read_usage(file: &Path) -> HashMap<String, Usage> {
    let Ok(text) = fs::read_to_string(file) else { return HashMap::new() };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text) else { return HashMap::new() };
    map.into_iter()
        .map(|(id, v)| {
            let count = v.get("count").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
            let last = v.get("last").and_then(serde_json::Value::as_u64).unwrap_or(0);
            (id, Usage { count, last })
        })
        .collect()
}

fn write_usage(file: &Path, usage: &HashMap<String, Usage>) {
    let map: serde_json::Map<String, serde_json::Value> =
        usage.iter().map(|(id, u)| (id.clone(), serde_json::json!({ "count": u.count, "last": u.last }))).collect();
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // Through a file beside it and a rename: a reader never sees half.
    let next = file.with_extension("json.new");
    if fs::write(&next, serde_json::Value::Object(map).to_string()).is_ok() {
        let _ = fs::rename(&next, file);
    }
}

/// Every application in `dirs`, the first directory's entry hiding a later
/// one of the same id, by name.
fn read_all(dirs: &[PathBuf], languages: &[String]) -> Vec<App> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(files) = fs::read_dir(dir.join("applications")) else { continue };
        let mut files: Vec<PathBuf> = files.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            if let Some(app) = parse(&id, &text, languages, &path) {
                out.push(app);
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// A desktop file's application, or none for one hidden, not shown, or
/// not an application.
fn parse(id: &str, text: &str, languages: &[String], file: &Path) -> Option<App> {
    let mut groups: Vec<(String, HashMap<String, String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            groups.push((name.to_owned(), HashMap::new()));
            continue;
        }
        if let (Some((_, fields)), Some((key, value))) = (groups.last_mut(), line.split_once('=')) {
            fields.entry(key.trim().to_owned()).or_insert_with(|| unescape(value.trim()));
        }
    }
    let entry = &groups.iter().find(|(n, _)| n == "Desktop Entry")?.1;
    let get = |fields: &HashMap<String, String>, k: &str| fields.get(k).cloned().unwrap_or_default();
    // A key in the user's language where the file has one.
    let local = |fields: &HashMap<String, String>, k: &str| {
        languages.iter().find_map(|l| fields.get(&format!("{k}[{l}]")).cloned()).unwrap_or_else(|| get(fields, k))
    };
    let yes = |k: &str| get(entry, k).eq_ignore_ascii_case("true");
    if get(entry, "Type") != "Application" || yes("Hidden") || yes("NoDisplay") {
        return None;
    }
    // Only where the desktop says it belongs.
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    let desktops: Vec<String> = desktop.split(':').filter(|d| !d.is_empty()).map(str::to_owned).collect();
    let listed = |k: &str| get(entry, k).split(';').filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect::<Vec<_>>();
    let only = listed("OnlyShowIn");
    if !only.is_empty() && !only.iter().any(|d| desktops.contains(d)) {
        return None;
    }
    if listed("NotShowIn").iter().any(|d| desktops.contains(d)) {
        return None;
    }
    let list = |k: &str| local(entry, k).split(';').filter(|s| !s.is_empty()).map(str::to_owned).collect();
    let actions = get(entry, "Actions")
        .split(';')
        .filter(|a| !a.is_empty())
        .filter_map(|a| {
            let fields = &groups.iter().find(|(n, _)| *n == format!("Desktop Action {a}"))?.1;
            Some(Action { id: a.to_owned(), name: local(fields, "Name"), icon: get(fields, "Icon"), exec: get(fields, "Exec") })
        })
        .collect();
    Some(App {
        id: id.to_owned(),
        name: local(entry, "Name"),
        generic_name: local(entry, "GenericName"),
        comment: local(entry, "Comment"),
        icon: get(entry, "Icon"),
        exec: get(entry, "Exec"),
        path: get(entry, "Path"),
        terminal: yes("Terminal"),
        startup_class: get(entry, "StartupWMClass"),
        keywords: list("Keywords"),
        categories: listed("Categories"),
        actions,
        file: file.to_owned(),
    })
}

/// A value's escapes: \s \n \t \r \\.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// `Exec=` as words: quoting undone, field codes given their values
/// (%f %F %u %U the files, %i the icon, %c the name, %k the file) or
/// dropped.
fn command_of(exec: &str, app: &App, files: &[String]) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            ' ' | '\t' if !quoted => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    let mut out = Vec::new();
    for w in words {
        match w.as_str() {
            "%f" | "%u" => out.extend(files.first().cloned()),
            "%F" | "%U" => out.extend(files.iter().cloned()),
            "%i" if !app.icon.is_empty() => {
                out.push("--icon".into());
                out.push(app.icon.clone());
            }
            "%c" => out.push(app.name.clone()),
            "%k" => out.push(app.file.display().to_string()),
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "%i" => {}
            _ => {
                // A code inside a word: %% is a percent, the others go.
                let mut kept = String::new();
                let mut it = w.chars().peekable();
                while let Some(c) = it.next() {
                    if c == '%' {
                        if it.next() == Some('%') {
                            kept.push('%');
                        }
                    } else {
                        kept.push(c);
                    }
                }
                out.push(kept);
            }
        }
    }
    out
}

/// How well `app` answers `query`, or none: the name first, then its
/// words, then what else describes it, then letters in order.
fn score(app: &App, query: &str) -> Option<f64> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Some(0.0);
    }
    let name = app.name.to_lowercase();
    if name == q {
        return Some(120.0);
    }
    if name.starts_with(&q) {
        return Some(100.0);
    }
    if name.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) {
        return Some(80.0);
    }
    if name.contains(&q) {
        return Some(60.0);
    }
    let others: Vec<String> = [&app.generic_name, &app.id, &app.startup_class]
        .into_iter()
        .map(|s| s.to_lowercase())
        .chain(app.keywords.iter().map(|k| k.to_lowercase()))
        .chain(app.exec.split_whitespace().next().map(|e| e.rsplit('/').next().unwrap_or(e).to_lowercase()))
        .collect();
    if others.iter().any(|o| o.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q))) {
        return Some(40.0);
    }
    if others.iter().any(|o| o.contains(&q)) || app.comment.to_lowercase().contains(&q) {
        return Some(20.0);
    }
    // The letters in order in the name, from the start of one of its words:
    // "lbo" for LibreOffice, never "fire" inside "libreoffice writer".
    let first = q.chars().next()?;
    let starts = name.char_indices().filter(|(i, c)| *c == first && (*i == 0 || !name[..*i].ends_with(char::is_alphanumeric)));
    for (i, _) in starts {
        let mut letters = name[i..].chars();
        if q.chars().all(|c| letters.any(|n| n == c)) {
            return Some(10.0);
        }
    }
    None
}

/// Wrappers and packaging leave these behind.
fn normalise(name: &str) -> String {
    let mut out = name.to_lowercase();
    for suffix in [".exe", ".bin", ".sh", ".py"] {
        if let Some(s) = out.strip_suffix(suffix) {
            out = s.to_owned();
        }
    }
    for suffix in ["bin", "wrapped", "real", "desktop", "gtk", "qt", "electron"] {
        for sep in ['-', '_'] {
            if let Some(s) = out.strip_suffix(&format!("{sep}{suffix}")) {
                out = s.to_owned();
            }
        }
    }
    out
}

fn find_in<'a>(list: &'a [App], name: &str) -> Option<&'a App> {
    let wanted = name.to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    let exact = |w: &str| {
        list.iter()
            .find(|a| {
                let id = a.id.to_lowercase();
                id == w || id.ends_with(&format!(".{w}"))
            })
            .or_else(|| list.iter().find(|a| a.startup_class.to_lowercase() == w))
            .or_else(|| list.iter().find(|a| a.name.to_lowercase() == w))
    };
    if let Some(a) = exact(&wanted) {
        return Some(a);
    }
    let wanted = normalise(&wanted);
    if let Some(a) = exact(&wanted) {
        return Some(a);
    }
    // A cut executable is a prefix of the real thing, so the match goes
    // both ways, but never on a fragment short enough to match half the
    // menu.
    if wanted.chars().count() < 4 {
        return None;
    }
    list.iter().find(|a| {
        let (id, label, class) = (a.id.to_lowercase(), a.name.to_lowercase(), a.startup_class.to_lowercase());
        id.contains(&wanted) || class.contains(&wanted) || label.contains(&wanted) || (label.chars().count() >= 4 && wanted.contains(&label))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = "[Desktop Entry]\nType=Application\nName=Firefox\nName[it]=Firefox (it)\nGenericName=Web Browser\n\
        GenericName[it]=Browser web\nExec=/usr/lib/firefox/firefox %u\nIcon=firefox\nKeywords=internet;www;\n\
        StartupWMClass=firefox\nActions=new-window;private;\n\n[Desktop Action new-window]\nName=New Window\n\
        Name[it]=Nuova finestra\nExec=/usr/lib/firefox/firefox --new-window %u\n\n[Desktop Action private]\n\
        Name=New Private Window\nExec=/usr/lib/firefox/firefox --private-window %u\n";

    fn firefox() -> App {
        parse("firefox", FIREFOX, &["it_IT".into(), "it".into()], Path::new("/usr/share/applications/firefox.desktop")).unwrap()
    }

    #[test]
    fn an_entry_is_read_in_the_users_language_with_its_actions() {
        let app = firefox();
        assert_eq!((app.name.as_str(), app.generic_name.as_str()), ("Firefox (it)", "Browser web"));
        assert_eq!(app.actions.len(), 2);
        assert_eq!((app.actions[0].id.as_str(), app.actions[0].name.as_str()), ("new-window", "Nuova finestra"));
        assert!(parse("x", "[Desktop Entry]\nType=Application\nName=X\nNoDisplay=true\n", &[], Path::new("x")).is_none());
        assert!(parse("x", "[Desktop Entry]\nType=Link\nName=X\n", &[], Path::new("x")).is_none());
    }

    #[test]
    fn exec_gives_its_codes_their_values() {
        let app = firefox();
        let files = vec!["https://a".to_owned(), "https://b".to_owned()];
        assert_eq!(command_of(&app.exec, &app, &files), ["/usr/lib/firefox/firefox", "https://a"]);
        assert_eq!(command_of(&app.exec, &app, &[]), ["/usr/lib/firefox/firefox"]);
        assert_eq!(
            command_of("sh -c \"echo \\\"a b\\\" 100%%\" %F %i %c", &app, &files),
            ["sh", "-c", "echo \"a b\" 100%", "https://a", "https://b", "--icon", "firefox", "Firefox (it)"]
        );
    }

    #[test]
    fn a_search_ranks_the_name_before_what_describes_it() {
        let mut list = vec![firefox()];
        for (id, name, generic) in [("org.gnome.Nautilus", "Files", "File Manager"), ("fire-alarm", "Alarm", "Fire watch"), ("libreoffice-writer", "LibreOffice Writer", "")] {
            list.push(App { id: id.into(), name: name.into(), generic_name: generic.into(), ..Default::default() });
        }
        let apps = Apps { list: Signal::new(list), read: std::cell::Cell::new(true), usage: RefCell::default(), usage_file: None };
        let names = |q: &str| apps.search(q, 0).into_iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(names("fire"), ["firefox", "fire-alarm"], "the name, then the id and generic name");
        assert_eq!(names("writer"), ["libreoffice-writer"], "a word of the name");
        assert_eq!(names("lbo"), ["libreoffice-writer"], "letters in order");
        assert_eq!(names("web"), ["firefox"], "the generic name's word");
        // Used often, an application rises among equals.
        apps.usage.borrow_mut().insert("fire-alarm".into(), Usage { count: 30, last: now() });
        assert_eq!(names(""), ["fire-alarm", "org.gnome.Nautilus", "firefox", "libreoffice-writer"]);
        assert_eq!(names("fire"), ["firefox", "fire-alarm"], "never above a better match");
    }

    #[test]
    fn a_window_or_a_process_is_traced_to_its_application() {
        let mut list = vec![firefox()];
        list.push(App { id: "org.gnome.Nautilus".into(), name: "Files".into(), startup_class: "org.gnome.Nautilus".into(), ..Default::default() });
        list.push(App { id: "zen".into(), name: "Zen Browser".into(), ..Default::default() });
        assert_eq!(find_in(&list, "nautilus").map(|a| a.id.as_str()), Some("org.gnome.Nautilus"), "the reverse-DNS id's last part");
        assert_eq!(find_in(&list, "Firefox").map(|a| a.id.as_str()), Some("firefox"));
        assert_eq!(find_in(&list, "zen-bin").map(|a| a.id.as_str()), Some("zen"), "a wrapper's suffix");
        assert!(find_in(&list, "fi").is_none(), "too short to guess from");
    }
}
