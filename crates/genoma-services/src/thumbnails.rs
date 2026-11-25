//! Small pictures of files (`@genoma/thumbnails`), in the cache every
//! freedesktop program shares (`~/.cache/thumbnails`): what Nautilus made
//! is used here, and what is made here by Nautilus.
//!
//! PNG, JPEG, WebP, GIF and SVG are made in the process; the rest by the
//! thumbnailers installed (`/usr/share/thumbnailers`), each shut in a
//! sandbox of its own where bubblewrap is there: a folder only looked at
//! runs a video decoder over files from anywhere.

use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    fs::{self, File},
    io::{BufReader, BufWriter},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, UNIX_EPOCH},
};

use md5::{Digest, Md5};

use crate::{apps::data_dirs, directory::remote, mime};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Size {
    Normal,
    Large,
    XLarge,
    XxLarge,
}

impl Size {
    pub const ALL: [Size; 4] = [Size::Normal, Size::Large, Size::XLarge, Size::XxLarge];

    pub fn from_name(name: &str) -> Option<Size> {
        Size::ALL.into_iter().find(|s| s.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Size::Normal => "normal",
            Size::Large => "large",
            Size::XLarge => "x-large",
            Size::XxLarge => "xx-large",
        }
    }

    /// The longest side, pixels.
    pub fn pixels(self) -> u32 {
        match self {
            Size::Normal => 128,
            Size::Large => 256,
            Size::XLarge => 512,
            Size::XxLarge => 1024,
        }
    }
}

/// What came of asking.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The thumbnail, a PNG; whether it was made just now.
    Ready { file: PathBuf, made: bool },
    /// The file has none and will not have one as it is.
    None,
    /// Written to a moment ago, likely still: worth asking again after this.
    Later(Duration),
}

/// A file this young may be half of itself.
const YOUNG: Duration = Duration::from_secs(2);
/// The longest a thumbnailer may take.
const LIMIT: Duration = Duration::from_secs(30);
/// The most memory one picture may take to decode.
const MEMORY: u64 = 512 << 20;

const PROGRAM: &str = "genoma";

/// The cache, and what makes what it lacks.
#[derive(Clone, Debug)]
pub struct Thumbnailer {
    cache: PathBuf,
    /// Types to the command that makes theirs.
    commands: Arc<HashMap<String, String>>,
    /// Bubblewrap, where it is.
    sandbox: Option<PathBuf>,
}

impl Thumbnailer {
    pub fn new(cache: PathBuf, commands: HashMap<String, String>) -> Thumbnailer {
        Thumbnailer { cache, commands: Arc::new(commands), sandbox: in_path("bwrap") }
    }

    pub fn system() -> Thumbnailer {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
        Thumbnailer::new(cache.join("thumbnails"), installed(&data_dirs()))
    }

    /// Whether files of this type can have one.
    pub fn can(&self, mime: &str) -> bool {
        own(mime) || self.commands.contains_key(mime)
    }

    /// The thumbnail of `path`: the cached one when it is the file's as it
    /// is now, else made.
    pub fn of(&self, path: &Path, size: Size, mime: Option<&str>) -> Outcome {
        // The one name the file has: a thumbnail belongs to it.
        let Ok(path) = fs::canonicalize(path) else { return Outcome::None };
        let Ok(meta) = fs::metadata(&path) else { return Outcome::None };
        // A thumbnail is its own.
        if !meta.is_file() || path.starts_with(&self.cache) {
            return Outcome::None;
        }
        let Ok(modified) = meta.modified() else { return Outcome::None };
        let seconds = modified.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let uri = file_uri(&path);
        let name = cached_name(&uri);
        // The size asked for, or a larger one there already.
        for s in Size::ALL.into_iter().filter(|s| *s >= size) {
            let file = self.cache.join(s.name()).join(&name);
            if current(&file, &uri, seconds) {
                return Outcome::Ready { file, made: false };
            }
        }
        let failed = self.cache.join("fail").join(PROGRAM).join(&name);
        if current(&failed, &uri, seconds) || remote(&path) {
            return Outcome::None;
        }
        let mime = mime.map_or_else(|| mime::database().of(&path).to_string(), str::to_owned);
        if !self.can(&mime) {
            return Outcome::None;
        }
        // A clock set back must not make every file young for ever.
        if let Ok(age) = modified.elapsed()
            && age < YOUNG
        {
            return Outcome::Later(YOUNG - age);
        }
        let image = if own(&mime) {
            // A decoder that breaks breaks this one picture.
            std::panic::catch_unwind(|| decode(&path, &mime, size)).unwrap_or(None)
        } else {
            self.commands.get(&mime).and_then(|c| self.run(c, &path, size))
        };
        let text = [("Thumb::URI", uri.as_str()), ("Thumb::MTime", &seconds.to_string()), ("Thumb::Size", &meta.len().to_string()), ("Thumb::Mimetype", &mime), ("Software", PROGRAM)];
        // The file changed while its picture was made: of neither.
        if fs::metadata(&path).and_then(|m| m.modified()).ok() != Some(modified) {
            return Outcome::Later(YOUNG);
        }
        match image {
            Some(image) => {
                let file = self.cache.join(size.name()).join(&name);
                match write(&file, &image, &text) {
                    Ok(()) => Outcome::Ready { file, made: true },
                    Err(e) => {
                        log::warn!("thumbnail {}: {e}", file.display());
                        Outcome::None
                    }
                }
            }
            None => {
                log::debug!("thumbnail of {} ({mime}): none could be made", path.display());
                if let Err(e) = write(&failed, &image::RgbaImage::new(1, 1), &text) {
                    log::debug!("thumbnail {}: {e}", failed.display());
                }
                Outcome::None
            }
        }
    }

    /// The thumbnailer `exec` run over `path`.
    fn run(&self, exec: &str, path: &Path, size: Size) -> Option<image::RgbaImage> {
        static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("genoma-thumbnail-{}-{n}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
        let made = self.run_in(&dir, exec, path, size);
        let _ = fs::remove_dir_all(&dir);
        made
    }

    fn run_in(&self, dir: &Path, exec: &str, path: &Path, size: Size) -> Option<image::RgbaImage> {
        let out = dir.join("out.png");
        // In the sandbox the file is the only one there is, under a name
        // with its ending (some decoders go by it) and nothing else of it.
        let inside = match path.extension() {
            Some(e) => {
                let mut name = OsString::from("/input/file.");
                name.push(e);
                PathBuf::from(name)
            }
            None => PathBuf::from("/input/file"),
        };
        let seen = if self.sandbox.is_some() { &inside } else { path };
        let words = command(exec, seen, &out, size);
        let mut run = match &self.sandbox {
            Some(bwrap) => {
                let mut c = Command::new(bwrap);
                c.args(["--unshare-all", "--die-with-parent", "--new-session", "--clearenv"]);
                c.args(["--setenv", "PATH", "/usr/local/bin:/usr/bin:/bin", "--setenv", "HOME", "/tmp", "--setenv", "GIO_USE_VFS", "local"]);
                c.args(["--ro-bind", "/usr", "/usr"]);
                for etc in ["/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/alternatives", "/etc/fonts", "/etc/glycin-loaders"] {
                    c.args(["--ro-bind-try", etc, etc]);
                }
                for (at, to) in [("/bin", "usr/bin"), ("/sbin", "usr/bin"), ("/lib", "usr/lib"), ("/lib64", "usr/lib64")] {
                    if Path::new(at).is_symlink() {
                        c.args(["--symlink", &fs::read_link(at).map_or_else(|_| to.to_owned(), |t| t.to_string_lossy().into_owned()), at]);
                    } else if Path::new(at).is_dir() {
                        c.args(["--ro-bind", at, at]);
                    }
                }
                c.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
                c.arg("--ro-bind").arg(path).arg(&inside);
                c.arg("--bind").arg(dir).arg(dir);
                c.args(["--chdir", "/"]);
                c.args(&words);
                c
            }
            None => {
                let mut c = Command::new(words.first()?);
                c.args(&words[1..]);
                c
            }
        };
        run.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
        let mut child = match run.spawn() {
            Ok(c) => c,
            Err(e) => {
                log::debug!("thumbnailer {exec}: {e}");
                return None;
            }
        };
        let started = Instant::now();
        // Short at first: most take a few hundredths of a second.
        let mut pause = Duration::from_millis(4);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() > LIMIT => {
                    // Whatever it started with it.
                    if let Some(group) = i32::try_from(child.id()).ok().and_then(rustix::process::Pid::from_raw) {
                        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                    }
                    let _ = child.wait();
                    log::warn!("thumbnailer {exec}: stopped after {} s", LIMIT.as_secs());
                    return None;
                }
                Ok(None) => std::thread::sleep(pause),
                Err(_) => return None,
            }
            pause = (pause * 2).min(Duration::from_millis(50));
        }
        let image = limited(image::ImageReader::open(&out).ok()?).decode().ok()?;
        Some(fit(image, size.pixels()))
    }
}

/// The types pictured in the process.
fn own(mime: &str) -> bool {
    matches!(mime, "image/png" | "image/apng" | "image/jpeg" | "image/webp" | "image/gif" | "image/svg+xml" | "image/svg+xml-compressed")
}

fn limited<R: std::io::BufRead + std::io::Seek>(mut reader: image::ImageReader<R>) -> image::ImageReader<R> {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MEMORY);
    reader.limits(limits);
    reader
}

/// `path`'s picture within a square of the size asked for, turned as its
/// camera says.
fn decode(path: &Path, mime: &str, size: Size) -> Option<image::RgbaImage> {
    use image::ImageDecoder;
    if mime.starts_with("image/svg") {
        return svg(path, size.pixels());
    }
    let reader = limited(image::ImageReader::new(BufReader::new(File::open(path).ok()?))).with_guessed_format().ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let turned = decoder.orientation().ok()?;
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(turned);
    Some(fit(image, size.pixels()))
}

/// `image` within a square of `side`, never made larger.
fn fit(image: image::DynamicImage, side: u32) -> image::RgbaImage {
    let (w, h) = (image.width(), image.height());
    if w.max(h) <= side {
        return image.into_rgba8();
    }
    let scale = side as f64 / w.max(h) as f64;
    let to = |v: u32| ((v as f64 * scale).round() as u32).max(1);
    let image = image::DynamicImage::ImageRgba8(image.into_rgba8());
    let mut out = image::DynamicImage::ImageRgba8(image::RgbaImage::new(to(w), to(h)));
    let options = fast_image_resize::ResizeOptions::new()
        .resize_alg(fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::CatmullRom));
    match fast_image_resize::Resizer::new().resize(&image, &mut out, &options) {
        Ok(()) => out.into_rgba8(),
        Err(_) => image.thumbnail(side, side).into_rgba8(),
    }
}

fn svg(path: &Path, side: u32) -> Option<image::RgbaImage> {
    use resvg::{tiny_skia, usvg};
    // A drawing of many megabytes is a map or a mistake.
    if fs::metadata(path).ok()?.len() > 16 << 20 {
        return None;
    }
    let tree = usvg::Tree::from_data(&fs::read(path).ok()?, &usvg::Options::default()).ok()?;
    let scale = side as f32 / tree.size().width().max(tree.size().height());
    let to = |v: f32| ((v * scale).round() as u32).max(1);
    let mut map = tiny_skia::Pixmap::new(to(tree.size().width()), to(tree.size().height()))?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut map.as_mut());
    let (w, h) = (map.width(), map.height());
    let straight = map.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    });
    image::RgbaImage::from_raw(w, h, straight.collect())
}

/// Whether the thumbnail `file` is of `uri` as it was at `seconds`.
fn current(file: &Path, uri: &str, seconds: u64) -> bool {
    let Ok(f) = File::open(file) else { return false };
    let Ok(reader) = png::Decoder::new(BufReader::new(f)).read_info() else { return false };
    let info = reader.info();
    let text = |key: &str| {
        let latin = info.uncompressed_latin1_text.iter().find(|t| t.keyword == key).map(|t| t.text.clone());
        latin.or_else(|| info.utf8_text.iter().find(|t| t.keyword == key).and_then(|t| t.get_text().ok()))
    };
    // Some write the time with a fraction.
    let time = text("Thumb::MTime").and_then(|t| t.split('.').next().and_then(|s| s.parse::<u64>().ok()));
    time == Some(seconds) && text("Thumb::URI").is_none_or(|u| u == uri)
}

/// Beside, then into place, and nobody else's to read: a private
/// picture's thumbnail is as private.
fn write(file: &Path, image: &image::RgbaImage, text: &[(&str, &str)]) -> Result<(), String> {
    let dir = file.parent().ok_or("no folder")?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| e.to_string())?;
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let beside = dir.join(format!(".genoma-{}-{n}.png", std::process::id()));
    let save = || -> Result<(), String> {
        let f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&beside).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(BufWriter::new(f), image.width(), image.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        for (key, value) in text {
            encoder.add_text_chunk((*key).to_owned(), (*value).to_owned()).map_err(|e| e.to_string())?;
        }
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(image.as_raw()).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        fs::rename(&beside, file).map_err(|e| e.to_string())
    };
    save().inspect_err(|_| {
        let _ = fs::remove_file(&beside);
    })
}

/// What a file's thumbnail is called.
fn cached_name(uri: &str) -> String {
    let hex: String = Md5::digest(uri.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    hex + ".png"
}

/// A path as a URI, escaped as GLib does: the cache goes by it, letter
/// for letter.
pub fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"!$&'()*+,-./:=@_~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The path a `file://` URI names, byte for byte; none for another's.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.trim().strip_prefix("file://")?;
    // file://host/path: the host, usually empty or localhost, goes.
    let path = &rest.as_bytes()[rest.find('/')?..];
    let mut out = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        let hex = (path[i] == b'%').then(|| path.get(i + 1..i + 3)).flatten();
        match hex.and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(path[i]);
                i += 1;
            }
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

/// A thumbnailer's `Exec` as words, its codes given their values.
fn command(exec: &str, path: &Path, out: &Path, size: Size) -> Vec<OsString> {
    let mut words = Vec::new();
    for word in exec.split_whitespace() {
        let mut to: Vec<u8> = Vec::new();
        let mut bytes = word.bytes();
        while let Some(b) = bytes.next() {
            if b != b'%' {
                to.push(b);
                continue;
            }
            match bytes.next() {
                Some(b'i') => to.extend_from_slice(path.as_os_str().as_bytes()),
                Some(b'u') => to.extend_from_slice(file_uri(path).as_bytes()),
                Some(b'o') => to.extend_from_slice(out.as_os_str().as_bytes()),
                Some(b's') => to.extend_from_slice(size.pixels().to_string().as_bytes()),
                Some(b'%') => to.push(b'%'),
                _ => {}
            }
        }
        words.push(OsString::from_vec(to));
    }
    words
}

fn in_path(program: &str) -> Option<PathBuf> {
    if program.starts_with('/') {
        return Some(PathBuf::from(program)).filter(|p| p.is_file());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.is_file())
}

/// The thumbnailers installed, by type: the first directory's wins.
fn installed(dirs: &[PathBuf]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for dir in dirs {
        let Ok(files) = fs::read_dir(dir.join("thumbnailers")) else { continue };
        let mut files: Vec<PathBuf> = files.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "thumbnailer")).collect();
        files.sort();
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else { continue };
            let field = |key: &str| text.lines().find_map(|l| l.trim().strip_prefix(key)?.trim_start().strip_prefix('=')).map(str::trim);
            let Some(exec) = field("Exec") else { continue };
            // Not installed, it makes nothing: another may.
            let program = field("TryExec").or_else(|| exec.split_whitespace().next()).unwrap_or_default();
            if in_path(program).is_none() {
                continue;
            }
            for mime in field("MimeType").unwrap_or_default().split(';').filter(|m| !m.is_empty()) {
                out.entry(mime.to_owned()).or_insert_with(|| exec.to_owned());
            }
        }
    }
    out
}

// ---- the queue ----------------------------------------------------------------

/// One thumbnail asked for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Wanted {
    pub path: PathBuf,
    pub size: Size,
}

struct Waiting {
    asked: Wanted,
    mime: Option<String>,
}

#[derive(Default)]
struct Line {
    waiting: VecDeque<Waiting>,
    threads: usize,
}

/// What is asked for, made a few at a time, the first asked first; what
/// nobody looks at any more is taken out before its turn.
pub struct Queue {
    line: Arc<Mutex<Line>>,
    thumbnailer: OnceLock<Thumbnailer>,
    done: Arc<dyn Fn(Wanted, Outcome) + Send + Sync>,
}

impl Queue {
    /// `done` is called on the thread that made it.
    pub fn new(done: impl Fn(Wanted, Outcome) + Send + Sync + 'static) -> Queue {
        Queue { line: Arc::default(), thumbnailer: OnceLock::new(), done: Arc::new(done) }
    }

    pub fn thumbnailer(&self) -> &Thumbnailer {
        self.thumbnailer.get_or_init(Thumbnailer::system)
    }

    pub fn ask(&self, asked: Wanted, mime: Option<String>) {
        let mut line = self.line.lock().unwrap_or_else(|e| e.into_inner());
        if line.waiting.iter().any(|w| w.asked == asked) {
            return;
        }
        line.waiting.push_back(Waiting { asked, mime });
        // Half the machine at most, and never more than four at once.
        let most = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4));
        if line.threads >= most {
            return;
        }
        line.threads += 1;
        let (shared, done, thumbnailer) = (self.line.clone(), self.done.clone(), self.thumbnailer().clone());
        let spawned = std::thread::Builder::new().name("genoma-thumbnail".into()).spawn(move || {
            // Behind whatever the person does meanwhile.
            unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::syscall(libc::SYS_gettid) as libc::id_t, 10) };
            loop {
                let next = {
                    let mut line = shared.lock().unwrap_or_else(|e| e.into_inner());
                    match line.waiting.pop_front() {
                        Some(w) => w,
                        None => {
                            line.threads -= 1;
                            return;
                        }
                    }
                };
                let outcome = thumbnailer.of(&next.asked.path, next.asked.size, next.mime.as_deref());
                done(next.asked, outcome);
            }
        });
        if spawned.is_err() {
            line.threads -= 1;
        }
    }

    /// Nobody looks at it: not made, unless it is being made.
    pub fn forget(&self, asked: &Wanted) {
        let mut line = self.line.lock().unwrap_or_else(|e| e.into_inner());
        line.waiting.retain(|w| w.asked != *asked);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("genoma-thumbnails-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    /// A file an hour old: not one being written.
    fn old(path: &Path) {
        let f = File::options().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)).unwrap();
    }

    fn picture(path: &Path, w: u32, h: u32) {
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 10, 10, 255])).save(path).unwrap();
        old(path);
    }

    #[test]
    fn a_picture_is_made_once_and_again_when_it_changes() {
        let dir = scratch("made");
        let t = Thumbnailer::new(dir.join("cache"), HashMap::new());
        let file = dir.join("a picture è.png");
        picture(&file, 400, 100);
        let Outcome::Ready { file: thumb, made: true } = t.of(&file, Size::Normal, Some("image/png")) else { panic!("not made") };
        assert_eq!(image::image_dimensions(&thumb).unwrap(), (128, 32));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&thumb).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(t.of(&file, Size::Normal, Some("image/png")), Outcome::Ready { file: thumb.clone(), made: false });
        // Another size is another file; a larger one serves a smaller.
        assert!(matches!(t.of(&file, Size::Large, Some("image/png")), Outcome::Ready { made: true, .. }));
        fs::remove_file(&thumb).unwrap();
        assert!(matches!(t.of(&file, Size::Normal, Some("image/png")), Outcome::Ready { made: false, .. }));
        let f = File::options().write(true).open(&file).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(60)).unwrap();
        assert!(matches!(t.of(&file, Size::Normal, Some("image/png")), Outcome::Ready { made: true, .. }));
    }

    #[test]
    fn what_cannot_be_pictured_is_not_tried_twice() {
        let dir = scratch("failed");
        let t = Thumbnailer::new(dir.join("cache"), HashMap::new());
        let file = dir.join("broken.png");
        fs::write(&file, "not a picture").unwrap();
        old(&file);
        assert_eq!(t.of(&file, Size::Normal, Some("image/png")), Outcome::None);
        let uri = file_uri(&file);
        let name = cached_name(&uri);
        assert!(dir.join("cache/fail/genoma").join(name).is_file());
        assert_eq!(t.of(&dir.join("none.png"), Size::Normal, Some("image/png")), Outcome::None);
        fs::write(dir.join("text.txt"), "words").unwrap();
        assert_eq!(t.of(&dir.join("text.txt"), Size::Normal, Some("text/plain")), Outcome::None);
    }

    #[test]
    fn a_file_being_written_waits() {
        let dir = scratch("young");
        let t = Thumbnailer::new(dir.join("cache"), HashMap::new());
        let file = dir.join("new.png");
        image::RgbaImage::new(4, 4).save(&file).unwrap();
        assert!(matches!(t.of(&file, Size::Normal, Some("image/png")), Outcome::Later(_)));
    }

    #[test]
    fn a_small_picture_is_not_made_larger_and_a_drawing_fills_the_size() {
        let dir = scratch("fit");
        let t = Thumbnailer::new(dir.join("cache"), HashMap::new());
        let file = dir.join("small.png");
        picture(&file, 20, 10);
        let Outcome::Ready { file: thumb, .. } = t.of(&file, Size::Large, None) else { panic!() };
        assert_eq!(image::image_dimensions(&thumb).unwrap(), (20, 10));
        let file = dir.join("drawing.svg");
        fs::write(&file, r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="#ff0000" fill-opacity="0.5"/></svg>"##).unwrap();
        old(&file);
        let Outcome::Ready { file: thumb, .. } = t.of(&file, Size::Large, Some("image/svg+xml")) else { panic!() };
        let image = image::open(&thumb).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (256, 128));
        assert_eq!(image.get_pixel(9, 9).0[0], 255, "not darkened by its transparency");
    }

    #[test]
    fn a_thumbnailer_is_run_and_stopped() {
        let dir = scratch("run");
        let commands = HashMap::from([("a/b".to_string(), "/usr/bin/cp %i %o".to_string())]);
        let mut t = Thumbnailer::new(dir.join("cache"), commands);
        let file = dir.join("it's 100%.png");
        picture(&file, 300, 300);
        for sandbox in [t.sandbox.clone(), None] {
            t.sandbox = sandbox;
            let _ = fs::remove_dir_all(dir.join("cache"));
            let Outcome::Ready { file: thumb, .. } = t.of(&file, Size::Normal, Some("a/b")) else { panic!("not made, sandbox {:?}", t.sandbox) };
            assert_eq!(image::image_dimensions(&thumb).unwrap(), (128, 128));
        }
    }

    #[test]
    fn a_path_is_escaped_as_glib_does() {
        assert_eq!(file_uri(Path::new("/a b/è;[1]#?%~(x)@='.png")), "file:///a%20b/%C3%A8%3B%5B1%5D%23%3F%25~(x)@='.png");
    }

    #[test]
    fn a_uri_goes_back_to_its_path() {
        use std::ffi::OsStr;
        for path in [OsStr::new("/a b/è;[1]#?%~(x)@='.png"), OsStr::from_bytes(b"/caf\xe9"), OsStr::new("/100%25")] {
            assert_eq!(uri_path(&file_uri(Path::new(path))).unwrap(), path);
        }
        assert_eq!(uri_path("file://localhost/a%2").unwrap(), Path::new("/a%2"));
        assert_eq!(uri_path("https://a/b"), None);
    }

    #[test]
    fn what_is_forgotten_is_not_made() {
        let q = Queue::new(|_, _| {});
        // No thread takes any: what waits is seen.
        q.line.lock().unwrap().threads = 99;
        let wanted = |n: usize| Wanted { path: PathBuf::from(format!("/nowhere/{n}")), size: Size::Normal };
        for n in [0, 1, 2, 1] {
            q.ask(wanted(n), None);
        }
        q.forget(&wanted(0));
        let waiting: Vec<Wanted> = q.line.lock().unwrap().waiting.iter().map(|w| w.asked.clone()).collect();
        assert_eq!(waiting, [wanted(1), wanted(2)], "each once, in the order asked");
    }

    #[test]
    fn what_is_asked_is_answered() {
        let (post, made) = std::sync::mpsc::channel();
        let post = Mutex::new(post);
        let q = Queue::new(move |w: Wanted, o| drop(post.lock().unwrap().send((w, o))));
        for n in 0..20 {
            q.ask(Wanted { path: PathBuf::from(format!("/nowhere/{n}")), size: Size::Normal }, None);
        }
        for _ in 0..20 {
            assert_eq!(made.recv_timeout(Duration::from_secs(5)).unwrap().1, Outcome::None);
        }
    }
}
