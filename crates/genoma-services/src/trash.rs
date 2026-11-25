//! The trash (`@genoma/trash`), as the freedesktop specification has it
//! and every file manager shares it: the home's own
//! (`$XDG_DATA_HOME/Trash`), and one on each other disk
//! (`.Trash/<uid>` where an administrator made it, else `.Trash-<uid>`),
//! so nothing thrown away is ever copied.

use std::{
    ffi::{OsStr, OsString},
    fs, io,
    io::Write,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
};

use crate::files::rename_new;

/// One thing in the trash.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Where it was.
    pub path: PathBuf,
    /// Where it is.
    pub file: PathBuf,
    /// What the trash knows of it.
    pub info: PathBuf,
    /// When it was thrown away, as it is written: "2026-11-25T01:30:00",
    /// the clock's time then.
    pub deleted: String,
}

/// A trash folder, and what its paths are relative to.
#[derive(Clone, Debug, PartialEq)]
struct Can {
    dir: PathBuf,
    /// The top of its disk; none for the home's, whose paths are whole.
    top: Option<PathBuf>,
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn home_trash() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("Trash")
}

/// `path` made absolute, its folder's links followed, its own name kept:
/// a link is thrown away, not what it points at.
fn whole(path: &Path) -> io::Result<PathBuf> {
    let name = match path.components().next_back() {
        Some(Component::Normal(n)) => n,
        _ => return Err(io::Error::other("it cannot go to the trash")),
    };
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok(fs::canonicalize(dir)?.join(name))
}

/// The nearest folder there is, at or above `path`.
fn existing(path: &Path) -> &Path {
    path.ancestors().find(|p| p.exists()).unwrap_or(Path::new("/"))
}

/// The top folder of the filesystem `path` is on.
fn top_of(path: &Path) -> io::Result<PathBuf> {
    let device = fs::symlink_metadata(path)?.dev();
    let mut top = path.to_owned();
    while let Some(up) = top.parent() {
        if fs::symlink_metadata(up)?.dev() != device {
            break;
        }
        top = up.to_owned();
    }
    Ok(top)
}

/// A folder only its owner sees, made if it is not there: none when what
/// is there is a link, or somebody else's.
fn private(dir: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.uid() != uid() {
        return Err(io::Error::other("the trash there is not ours"));
    }
    Ok(())
}

/// The trash `path`, a whole one, goes to.
fn can_for(path: &Path, home: &Path) -> io::Result<Can> {
    // Its folder's filesystem, not its own: a mount point cannot go.
    let device = fs::symlink_metadata(path)?.dev();
    let parent = path.parent().unwrap_or(Path::new("/"));
    if fs::symlink_metadata(parent)?.dev() != device {
        return Err(io::Error::other("a mounted disk cannot go to the trash"));
    }
    if fs::metadata(existing(home))?.dev() == device {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(home)?;
        return Ok(Can { dir: home.to_owned(), top: None });
    }
    let top = top_of(parent)?;
    // The administrator's, safe only when nobody can swap it.
    let shared = top.join(".Trash");
    if let Ok(m) = fs::symlink_metadata(&shared)
        && m.is_dir()
        && m.mode() & libc::S_ISVTX != 0
    {
        let dir = shared.join(uid().to_string());
        if private(&dir).is_ok() {
            return Ok(Can { dir, top: Some(top) });
        }
    }
    let dir = top.join(format!(".Trash-{}", uid()));
    private(&dir).map_err(|e| io::Error::other(format!("no trash on this disk: {}", crate::files::why(&e))))?;
    Ok(Can { dir, top: Some(top) })
}

/// Thrown away.
pub fn put(path: &Path) -> io::Result<Item> {
    put_in(path, &home_trash())
}

fn put_in(path: &Path, home: &Path) -> io::Result<Item> {
    let path = whole(path)?;
    fs::symlink_metadata(&path)?;
    let can = can_for(&path, home)?;
    // The trash itself, or what holds it.
    if can.dir.starts_with(&path) || path.starts_with(&can.dir) {
        return Err(io::Error::other("it is the trash"));
    }
    let (files, infos) = (can.dir.join("files"), can.dir.join("info"));
    private(&files)?;
    private(&infos)?;
    let written = match &can.top {
        Some(top) => path.strip_prefix(top).unwrap_or(&path),
        None => &path,
    };
    let deleted = jiff::Zoned::now().strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let text = format!("[Trash Info]\nPath={}\nDeletionDate={deleted}\n", escape(written));
    let name = path.file_name().unwrap_or_default();
    for n in 1..10_000 {
        let called = called(name, n);
        let mut info_name = called.clone();
        info_name.push(".trashinfo");
        let info = infos.join(info_name);
        // The name is taken by what is known of it, first: two programs
        // at once never take the same.
        let mut f = match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let file = files.join(&called);
        let moved = f.write_all(text.as_bytes()).and_then(|()| f.sync_data()).and_then(|()| rename_new(&path, &file));
        match moved {
            Ok(()) => return Ok(Item { path, file, info, deleted }),
            Err(e) => {
                let _ = fs::remove_file(&info);
                // A file nothing is known of: another name.
                if e.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(io::Error::other("no free name in the trash"))
}

/// What the `n`th of a name is called: "a.txt", "a.2.txt", within what a
/// name may be with ".trashinfo" after it.
fn called(name: &OsStr, n: usize) -> OsString {
    let bytes = name.as_bytes();
    let (stem, ending) = match bytes.iter().rposition(|b| *b == b'.').filter(|i| *i > 0) {
        Some(i) => bytes.split_at(i),
        None => (bytes, &b""[..]),
    };
    let number = if n == 1 { String::new() } else { format!(".{n}") };
    // An ending longer than the room is no ending.
    let (stem, ending) = if ending.len() > 100 { (bytes, &b""[..]) } else { (stem, ending) };
    let room = 255 - ".trashinfo".len() - number.len() - ending.len();
    let mut cut = stem.len().min(room);
    while cut < stem.len() && cut > 0 && stem[cut] & 0xc0 == 0x80 {
        cut -= 1;
    }
    OsString::from_vec([&stem[..cut], number.as_bytes(), ending].concat())
}

fn escape(path: &Path) -> String {
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unescape(text: &str) -> PathBuf {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%').then(|| bytes.get(i + 1..i + 3)).flatten();
        match hex.and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(OsString::from_vec(out))
}

/// Every trash of this user there is: the home's, and each disk's.
fn cans() -> Vec<Can> {
    let mut out = vec![Can { dir: home_trash(), top: None }];
    let mounts = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    out.extend(cans_on(&mounted(&mounts)));
    out
}

/// Where real disks are mounted: the kernel's own filesystems have no
/// trash, and a network that hangs must not hang the list.
fn mounted(mountinfo: &str) -> Vec<PathBuf> {
    const NONE: [&str; 22] = [
        "proc", "sysfs", "devtmpfs", "devpts", "cgroup", "cgroup2", "securityfs", "debugfs", "tracefs", "pstore", "bpf", "configfs",
        "fusectl", "hugetlbfs", "mqueue", "ramfs", "efivarfs", "autofs", "binfmt_misc", "nsfs", "squashfs", "overlay",
    ];
    const FAR: [&str; 7] = ["nfs", "nfs4", "cifs", "smb3", "afs", "ceph", "9p"];
    let mut out: Vec<PathBuf> = Vec::new();
    for line in mountinfo.lines() {
        let Some((before, after)) = line.split_once(" - ") else { continue };
        let kind = after.split(' ').next().unwrap_or_default();
        if NONE.contains(&kind) || FAR.contains(&kind) || (kind.starts_with("fuse") && kind != "fuseblk") {
            continue;
        }
        let Some(at) = before.split(' ').nth(4) else { continue };
        let at = PathBuf::from(OsString::from_vec(octal(at.as_bytes())));
        if !out.contains(&at) {
            out.push(at);
        }
    }
    out
}

/// The mounts' way with spaces: "\040".
fn octal(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let code = (text[i] == b'\\').then(|| text.get(i + 1..i + 4)).flatten();
        match code.and_then(|c| std::str::from_utf8(c).ok()).and_then(|c| u8::from_str_radix(c, 8).ok()) {
            Some(b) => {
                out.push(b);
                i += 4;
            }
            None => {
                out.push(text[i]);
                i += 1;
            }
        }
    }
    out
}

/// The folders whose changes are the trash's. What is known of a thing is
/// written before the thing comes: both.
pub fn watched() -> Vec<PathBuf> {
    cans().into_iter().flat_map(|c| [c.dir.join("info"), c.dir.join("files")]).collect()
}

fn cans_on(tops: &[PathBuf]) -> Vec<Can> {
    let mut out = Vec::new();
    for top in tops {
        for dir in [top.join(".Trash").join(uid().to_string()), top.join(format!(".Trash-{}", uid()))] {
            if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && m.uid() == uid()) {
                out.push(Can { dir, top: Some(top.clone()) });
            }
        }
    }
    out
}

/// What is in the trash, the latest first.
pub fn items() -> Vec<Item> {
    items_in(&cans())
}

fn items_in(cans: &[Can]) -> Vec<Item> {
    let mut out = Vec::new();
    for can in cans {
        for e in fs::read_dir(can.dir.join("info")).into_iter().flatten().flatten() {
            let name = e.file_name();
            let Some(called) = name.as_bytes().strip_suffix(b".trashinfo") else { continue };
            let file = can.dir.join("files").join(OsStr::from_bytes(called));
            // What is known of nothing: left by a program that died.
            if fs::symlink_metadata(&file).is_err() {
                continue;
            }
            let Ok(text) = fs::read_to_string(e.path()) else { continue };
            let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::trim);
            let Some(path) = field("Path").map(unescape) else { continue };
            let path = match &can.top {
                // Within its disk and nowhere else, whatever is written.
                Some(top) if path.is_relative() && !path.components().any(|c| c == Component::ParentDir) => top.join(path),
                Some(_) => continue,
                None if path.is_absolute() => path,
                None => continue,
            };
            out.push(Item { path, file, info: e.path(), deleted: field("DeletionDate").unwrap_or_default().to_owned() });
        }
    }
    out.sort_by(|a, b| b.deleted.cmp(&a.deleted).then_with(|| a.path.cmp(&b.path)));
    out
}

/// When an item was thrown away, seconds since 1970: what is written is
/// the clock of this place.
pub fn seconds(deleted: &str) -> f64 {
    let at = deleted.parse::<jiff::civil::DateTime>().ok().and_then(|d| d.to_zoned(jiff::tz::TimeZone::system()).ok());
    at.map_or(0.0, |z| z.timestamp().as_second() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("genoma-trash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn what_is_thrown_away_is_known_and_told_apart() {
        let dir = scratch("home");
        let home = dir.join("Trash");
        fs::create_dir(dir.join("d")).unwrap();
        for _ in 0..2 {
            fs::write(dir.join("d/è 100%.tar.gz"), "x").unwrap();
            put_in(&dir.join("d/è 100%.tar.gz"), &home).unwrap();
        }
        put_in(&dir.join("d"), &home).unwrap();
        let items = items_in(&[Can { dir: home.clone(), top: None }]);
        assert_eq!(items.len(), 3);
        let mut names: Vec<String> = items.iter().map(|i| i.file.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["d", "è 100%.tar.2.gz", "è 100%.tar.gz"]);
        assert!(items.iter().filter(|i| i.path == dir.join("d/è 100%.tar.gz")).count() == 2);
        assert!(!dir.join("d").exists());
        let text = fs::read_to_string(home.join("info/è 100%.tar.gz.trashinfo")).unwrap();
        assert!(text.contains("/d/%C3%A8%20100%25.tar.gz\n"), "{text}");
    }

    #[test]
    fn a_link_goes_and_not_what_it_points_at() {
        let dir = scratch("link");
        fs::write(dir.join("real"), "x").unwrap();
        std::os::unix::fs::symlink("real", dir.join("link")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.join("lost")).unwrap();
        for name in ["link", "lost"] {
            put_in(&dir.join(name), &dir.join("Trash")).unwrap();
        }
        assert!(dir.join("real").exists());
        assert!(fs::symlink_metadata(dir.join("link")).is_err());
    }

    #[test]
    fn the_trash_does_not_go_into_itself() {
        let dir = scratch("itself");
        let home = dir.join("share/Trash");
        fs::write(dir.join("a"), "").unwrap();
        let item = put_in(&dir.join("a"), &home).unwrap();
        for path in [home.clone(), dir.join("share"), item.file.clone()] {
            assert!(put_in(&path, &home).is_err(), "{}", path.display());
        }
        assert!(item.file.exists());
        assert!(put_in(&dir.join("none"), &home).is_err());
        assert!(put_in(Path::new("/"), &home).is_err());
        assert!(put_in(Path::new(".."), &home).is_err());
        assert_eq!(fs::read_dir(home.join("info")).unwrap().count(), 1, "nothing known of what did not go");
    }

    #[test]
    fn another_disk_has_its_own() {
        // Two filesystems, where the machine has them (/dev/shm is a
        // tmpfs of its own).
        let shm = PathBuf::from("/dev/shm").join(format!("genoma-trash-{}", std::process::id()));
        let dir = scratch("disk");
        if fs::create_dir_all(&shm).is_err() || fs::metadata(&shm).unwrap().dev() == fs::metadata(&dir).unwrap().dev() {
            return;
        }
        fs::write(shm.join("file"), "x").unwrap();
        let item = put_in(&shm.join("file"), &dir.join("Trash")).unwrap();
        let can = PathBuf::from(format!("/dev/shm/.Trash-{}", uid()));
        assert!(item.file.starts_with(&can));
        let items = items_in(&cans_on(&[PathBuf::from("/dev/shm")]));
        let found = items.iter().find(|i| i.file == item.file).unwrap();
        assert_eq!(found.path, shm.join("file"));
        let text = fs::read_to_string(&item.info).unwrap();
        assert!(text.contains("\nPath=genoma-trash-"), "relative to its disk: {text}");
        fs::remove_file(&item.file).unwrap();
        fs::remove_file(&item.info).unwrap();
        let _ = fs::remove_dir(can.join("files"));
        let _ = fs::remove_dir(can.join("info"));
        let _ = fs::remove_dir(&can);
        fs::remove_dir_all(&shm).unwrap();
    }

    #[test]
    fn what_is_written_does_not_lead_out_of_the_disk() {
        let dir = scratch("out");
        let can = dir.join(format!(".Trash-{}", uid()));
        fs::create_dir_all(can.join("info")).unwrap();
        fs::create_dir_all(can.join("files")).unwrap();
        for (name, path) in [("a", "../../etc/passwd"), ("b", "/etc/passwd"), ("c", "fine/c")] {
            fs::write(can.join("files").join(name), "").unwrap();
            fs::write(can.join("info").join(format!("{name}.trashinfo")), format!("[Trash Info]\nPath={path}\nDeletionDate=2026-01-01T00:00:00\n")).unwrap();
        }
        let items = items_in(&cans_on(&[dir.clone()]));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, dir.join("fine/c"));
    }

    #[test]
    fn the_mounts_are_read() {
        let text = "36 35 98:0 / /mnt/a\\040b rw - ext4 /dev/sda1 rw\n37 35 0:5 / /proc rw - proc proc rw\n38 35 0:6 / /net rw - nfs4 a:/b rw\n39 35 8:2 / /ntfs rw - fuseblk /dev/sdb1 rw\n40 35 0:9 / /run/gvfs rw - fuse.gvfsd-fuse x rw\n";
        assert_eq!(mounted(text), [PathBuf::from("/mnt/a b"), PathBuf::from("/ntfs")]);
    }

    #[test]
    fn a_long_name_still_has_room_for_what_is_known_of_it() {
        let name = OsString::from("è".repeat(127));
        for n in [1, 2, 9999] {
            let c = called(&name, n);
            assert!(c.len() + ".trashinfo".len() <= 255 && c.to_str().is_some());
        }
    }
}
