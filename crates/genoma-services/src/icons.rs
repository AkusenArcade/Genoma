//! An application's icon, from its app id: the desktop entry's `Icon=`, then
//! the icon theme and the themes it inherits, then hicolor and pixmaps, as
//! the freedesktop icon theme specification orders them. The theme is
//! `QS_ICON_THEME` when set (Bioma passes the desktop's to Quickshell that
//! way), else the desktop's own.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs = vec![
        std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(&home).join(".local/share")),
    ];
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs
}

fn theme() -> Option<String> {
    if let Ok(t) = std::env::var("QS_ICON_THEME") {
        return Some(t);
    }
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_owned())
}

/// The `Icon=` of the desktop entry whose name or StartupWMClass is `app_id`.
fn desktop_icon(app_id: &str) -> Option<String> {
    let lower = app_id.to_lowercase();
    let mut fallback = None;
    for dir in data_dirs() {
        let Ok(entries) = fs::read_dir(dir.join("applications")) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
            let icon = field("Icon=");
            if stem == lower || stem.ends_with(&format!(".{lower}")) {
                if let Some(icon) = icon {
                    return Some(icon.to_owned());
                }
            }
            if fallback.is_none() && field("StartupWMClass=").is_some_and(|c| c.to_lowercase() == lower) {
                fallback = icon.map(str::to_owned);
            }
        }
    }
    fallback
}

fn theme_dirs(name: &str) -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut bases = vec![PathBuf::from(&home).join(".icons")];
    bases.extend(data_dirs().into_iter().map(|d| d.join("icons")));
    bases.into_iter().map(|b| b.join(name)).filter(|p| p.is_dir()).collect()
}

/// The theme and everything it inherits, in order, ending with hicolor.
fn chain(start: &str) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    let mut queue = vec![start.to_owned()];
    let mut dirs = Vec::new();
    while let Some(name) = queue.pop() {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        for dir in theme_dirs(&name) {
            if let Ok(index) = fs::read_to_string(dir.join("index.theme")) {
                if let Some(inherits) = index.lines().find_map(|l| l.strip_prefix("Inherits=")) {
                    for parent in inherits.split(',').rev() {
                        queue.push(parent.trim().to_owned());
                    }
                }
            }
            dirs.push(dir);
        }
    }
    if !seen.iter().any(|s| s == "hicolor") {
        dirs.extend(theme_dirs("hicolor"));
    }
    dirs
}

const SIZES: &[&str] = &[
    "scalable/apps", "apps/scalable", "256x256/apps", "128x128/apps", "96x96/apps", "64x64/apps",
    "48x48/apps", "apps/64", "apps/48", "32x32/apps",
];

/// Theme by theme, and in each the name as written before its lower case:
/// a desktop entry asks for `Alacritty`, the chosen theme draws `alacritty`,
/// and hicolor's `Alacritty` further down the chain is not the one meant.
/// The unthemed pixmaps are the last resort, as Qt's lookup has it.
/// The subdirectories a theme declares (`Directories=` in its index.theme),
/// the scalable ones first and then from the largest size down; Bioma's
/// application sizes for a theme that declares none. A tray item's icon is
/// a status or a symbolic one as often as an application's.
fn subdirs(dir: &Path) -> Vec<String> {
    let Ok(index) = fs::read_to_string(dir.join("index.theme")) else {
        return SIZES.iter().map(|s| s.to_string()).collect();
    };
    let mut listed: Vec<String> = Vec::new();
    for key in ["Directories=", "ScaledDirectories="] {
        if let Some(line) = index.lines().find_map(|l| l.strip_prefix(key)) {
            listed.extend(line.split(',').map(|d| d.trim().to_owned()).filter(|d| !d.is_empty()));
        }
    }
    if listed.is_empty() {
        return SIZES.iter().map(|s| s.to_string()).collect();
    }
    let weight = |d: &str| -> i64 {
        if d.contains("scalable") {
            return i64::MAX;
        }
        d.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse::<i64>().ok()).max().unwrap_or(0)
    };
    // Applications first among equals: a name both an app and a status
    // icon (a tray's) is the application's.
    listed.sort_by_key(|d| (std::cmp::Reverse(weight(d)), !d.contains("apps")));
    listed.dedup();
    listed
}

fn in_theme(dirs: &[PathBuf], icon: &str) -> Option<PathBuf> {
    let lower = icon.to_lowercase();
    let names: &[&str] = if lower == icon { &[icon] } else { &[icon, &lower] };
    for dir in dirs {
        let sizes = subdirs(dir);
        for name in names {
            for size in &sizes {
                for ext in ["svg", "png"] {
                    let p = dir.join(size).join(format!("{name}.{ext}"));
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
    }
    for name in names {
        for ext in ["svg", "png"] {
            let p = Path::new("/usr/share/pixmaps").join(format!("{name}.{ext}"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

static CACHE: Mutex<Option<HashMap<String, Option<PathBuf>>>> = Mutex::new(None);

/// The icon file for an app id, or nothing: the cell then draws its own
/// fallback glyph, never another application's logo.
pub fn app_icon(app_id: &str) -> Option<PathBuf> {
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(found) = cache.get(app_id) {
        return found.clone();
    }
    let icon = desktop_icon(app_id).unwrap_or_else(|| app_id.to_owned());
    let found = if Path::new(&icon).is_absolute() {
        Path::new(&icon).exists().then(|| PathBuf::from(&icon))
    } else {
        let dirs = chain(&theme().unwrap_or_else(|| "hicolor".into()));
        in_theme(&dirs, &icon).or_else(|| in_theme(&dirs, &app_id.to_lowercase()))
    };
    cache.insert(app_id.to_owned(), found.clone());
    found
}

/// An icon by name (or path) through the theme, as `app_icon` finds an
/// application's: what Quickshell's `iconPath` answers.
pub fn icon_path(icon: &str) -> Option<PathBuf> {
    if Path::new(icon).is_absolute() {
        return Path::new(icon).exists().then(|| PathBuf::from(icon));
    }
    let key = format!("icon:{icon}");
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(found) = cache.get(&key) {
        return found.clone();
    }
    let dirs = chain(&theme().unwrap_or_else(|| "hicolor".into()));
    let found = in_theme(&dirs, icon);
    cache.insert(key, found.clone());
    found
}

/// One application's desktop entry, the `[Desktop Entry]` group.
#[derive(Clone, Debug, Default)]
pub struct DesktopEntry {
    /// The file name without `.desktop`: `org.gnome.Nautilus`.
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub icon: String,
    pub exec: String,
    pub startup_class: String,
    pub path: String,
    pub terminal: bool,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub file: PathBuf,
}

/// The applications the menu offers, in the order the data directories
/// rank them: an entry in the user's directory hides the system's of the
/// same id, and one that is `Hidden`, `NoDisplay` or not an application is
/// left out.
pub fn desktop_entries() -> Vec<DesktopEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in data_dirs() {
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
            let mut fields = HashMap::new();
            let mut in_group = false;
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_group = line == "[Desktop Entry]";
                    continue;
                }
                if !in_group {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    fields.entry(key.trim().to_owned()).or_insert_with(|| value.trim().to_owned());
                }
            }
            let get = |k: &str| fields.get(k).cloned().unwrap_or_default();
            let yes = |k: &str| get(k).eq_ignore_ascii_case("true");
            if get("Type") != "Application" || yes("Hidden") || yes("NoDisplay") {
                continue;
            }
            let list = |k: &str| get(k).split(';').filter(|s| !s.is_empty()).map(str::to_owned).collect();
            out.push(DesktopEntry {
                id,
                name: get("Name"),
                generic_name: get("GenericName"),
                comment: get("Comment"),
                icon: get("Icon"),
                exec: get("Exec"),
                startup_class: get("StartupWMClass"),
                path: get("Path"),
                terminal: yes("Terminal"),
                keywords: list("Keywords"),
                categories: list("Categories"),
                file: path,
            });
        }
    }
    out
}
