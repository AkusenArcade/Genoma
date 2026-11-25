//! Operations on files (`@genoma/files`): copy, move, link, delete, trash
//! and back, as jobs that say how far they are, wait when told, stop when
//! told, and ask when only a person can say.
//!
//! What they keep to:
//!
//! - Nothing is overwritten without an answer. What a file is put in place
//!   by refuses a name already taken, so one that appears meanwhile is asked
//!   about too.
//! - A file appears whole under its name or not at all: its data goes into
//!   one with no name (`O_TMPFILE`), or a hidden one where a filesystem has
//!   none of those, put in place once complete.
//! - Across filesystems a source is removed only when everything copied of
//!   it is on the disk, and only if it is still what was copied.
//! - Symbolic links are copied as links. Devices, sockets and pipes are left
//!   out and said to be.
//!
//! Everything here blocks: it runs on a thread of its own.

use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata},
    io::{self, ErrorKind},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

use rustix::fs::{AtFlags, CWD, Mode, OFlags, RenameFlags, SeekFrom, Timespec, Timestamps};

use crate::trash;

// ---- what is asked for ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Task {
    /// Into the folder `to`, each under its own name.
    Copy { from: Vec<PathBuf>, to: PathBuf },
    Move { from: Vec<PathBuf>, to: PathBuf },
    /// Symbolic links to them, in `to`.
    Link { from: Vec<PathBuf>, to: PathBuf },
    /// For good.
    Delete { paths: Vec<PathBuf> },
    Trash { paths: Vec<PathBuf> },
    /// What the trash holds back where it was.
    Restore { items: Vec<trash::Item> },
    /// What the trash holds, for good.
    Erase { items: Vec<trash::Item> },
}

impl Task {
    pub fn name(&self) -> &'static str {
        match self {
            Task::Copy { .. } => "copy",
            Task::Move { .. } => "move",
            Task::Link { .. } => "link",
            Task::Delete { .. } => "delete",
            Task::Trash { .. } => "trash",
            Task::Restore { .. } => "restore",
            Task::Erase { .. } => "erase",
        }
    }

    pub fn to(&self) -> Option<&Path> {
        match self {
            Task::Copy { to, .. } | Task::Move { to, .. } | Task::Link { to, .. } => Some(to),
            _ => None,
        }
    }

    pub fn count(&self) -> usize {
        match self {
            Task::Copy { from, .. } | Task::Move { from, .. } | Task::Link { from, .. } => from.len(),
            Task::Delete { paths } | Task::Trash { paths } => paths.len(),
            Task::Restore { items } | Task::Erase { items } => items.len(),
        }
    }

    /// Whether it only changes names: done at once, it need not wait behind
    /// a copy of an hour.
    pub fn quick(&self) -> bool {
        match self {
            Task::Link { .. } | Task::Trash { .. } => true,
            Task::Restore { .. } => true,
            Task::Move { from, to } => {
                let device = |p: &Path| fs::symlink_metadata(p).map(|m| m.dev()).ok();
                let there = fs::metadata(to).map(|m| m.dev()).ok();
                from.iter().all(|f| device(f) == there)
            }
            _ => false,
        }
    }
}

/// What one side of a conflict is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Seen {
    pub dir: bool,
    pub size: u64,
    pub modified: f64,
}

impl Seen {
    fn of(m: &Metadata) -> Seen {
        Seen { dir: m.is_dir(), size: if m.is_dir() { 0 } else { m.len() }, modified: m.mtime() as f64 + m.mtime_nsec() as f64 / 1e9 }
    }
}

/// What only a person can say.
#[derive(Clone, Debug, PartialEq)]
pub enum Question {
    /// The name is taken. `merge`: both are folders; `replace`: both are
    /// not, and the one there would go.
    Exists { from: PathBuf, to: PathBuf, source: Seen, target: Seen, merge: bool, replace: bool },
    /// It could not be done.
    Failed { path: PathBuf, error: String },
    /// More than fits where it goes.
    Space { to: PathBuf, needed: u64, available: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The one there goes, in favour of this one.
    Replace,
    /// This folder's entries go into the one there.
    Merge,
    /// Under another name.
    KeepBoth,
    /// Left as it is; with `Space`, on anyway.
    Skip,
    Retry,
    Cancel,
}

impl Answer {
    pub fn from_name(name: &str) -> Option<Answer> {
        Some(match name {
            "replace" => Answer::Replace,
            "merge" => Answer::Merge,
            "keep_both" => Answer::KeepBoth,
            "skip" => Answer::Skip,
            "retry" => Answer::Retry,
            "cancel" => Answer::Cancel,
            _ => return None,
        })
    }
}

impl Question {
    pub fn answers(&self) -> Vec<&'static str> {
        match self {
            Question::Exists { merge: true, .. } => vec!["merge", "keep_both", "skip", "cancel"],
            Question::Exists { replace: true, .. } => vec!["replace", "keep_both", "skip", "cancel"],
            Question::Exists { .. } => vec!["keep_both", "skip", "cancel"],
            Question::Failed { .. } => vec!["retry", "skip", "cancel"],
            Question::Space { .. } => vec!["retry", "skip", "cancel"],
        }
    }

    fn takes(&self, answer: Answer) -> bool {
        self.answers().iter().any(|a| Answer::from_name(a) == Some(answer))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Waiting,
    /// Counting what there is.
    Preparing,
    Running,
    Paused,
    Asking,
    /// Waiting for the disk to have it all.
    Finishing,
    Done,
    Cancelled,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::Preparing => "preparing",
            State::Running => "running",
            State::Paused => "paused",
            State::Asking => "asking",
            State::Finishing => "finishing",
            State::Done => "done",
            State::Cancelled => "cancelled",
        }
    }
}

/// How far a job is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub state: State,
    pub total_bytes: u64,
    pub bytes: u64,
    pub total_files: u64,
    pub files: u64,
    /// What is being worked on.
    pub current: PathBuf,
    /// Bytes a second, lately.
    pub rate: f64,
    pub question: Option<Question>,
    /// Where each thing asked for is now.
    pub made: Vec<PathBuf>,
    /// What was left out, by a person's answer or for what it is.
    pub skipped: Vec<PathBuf>,
    /// What a copy could not keep: "permissions", "times", "attributes";
    /// "changes" made to a file while it was read.
    pub lost: Vec<&'static str>,
}

// ---- the reins -----------------------------------------------------------------

#[derive(Default)]
struct Told {
    paused: bool,
    cancelled: bool,
    answer: Option<(Answer, bool)>,
    /// A test's: cancelled at this many checks from now.
    left: Option<u64>,
}

/// What holds a job: from any thread.
#[derive(Default)]
pub struct Control {
    told: Mutex<Told>,
    wake: Condvar,
}

/// Cancelled: everything unwinds, what is half made is taken away.
#[derive(Debug, PartialEq)]
pub struct Stopped;

impl Control {
    fn told(&self) -> std::sync::MutexGuard<'_, Told> {
        self.told.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn pause(&self, on: bool) {
        self.told().paused = on;
        self.wake.notify_all();
    }

    pub fn cancel(&self) {
        self.told().cancelled = true;
        self.wake.notify_all();
        // Also one that waits its turn.
        let _turns = TURNS.lock().unwrap_or_else(|e| e.into_inner());
        NEXT.notify_all();
    }

    pub fn answer(&self, answer: Answer, for_all: bool) {
        self.told().answer = Some((answer, for_all));
        self.wake.notify_all();
    }

    fn cancelled(&self) -> bool {
        self.told().cancelled
    }
}

// ---- in turn --------------------------------------------------------------------

/// Who waits to copy, the first first: two copies at once make a disk's
/// head run between them.
static TURNS: Mutex<std::collections::VecDeque<u64>> = Mutex::new(std::collections::VecDeque::new());
static NEXT: Condvar = Condvar::new();

/// A place in the line, given up with it.
struct Turn(u64);

impl Turn {
    fn take() -> Turn {
        static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        TURNS.lock().unwrap_or_else(|e| e.into_inner()).push_back(n);
        Turn(n)
    }

    /// Waits until it is first: false when cancelled before.
    fn come(&self, control: &Control) -> bool {
        let mut turns = TURNS.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if control.cancelled() {
                return false;
            }
            if turns.front() == Some(&self.0) {
                return true;
            }
            turns = NEXT.wait(turns).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        TURNS.lock().unwrap_or_else(|e| e.into_inner()).retain(|t| *t != self.0);
        NEXT.notify_all();
    }
}

/// [`run`], once the jobs asked for before it are done; one that only
/// changes names does not wait.
pub fn run_in_turn(task: &Task, control: &Control, report: &dyn Fn(&Progress)) -> Progress {
    if task.quick() {
        return run(task, control, report);
    }
    let turn = Turn::take();
    if !turn.come(control) {
        return Progress { state: State::Cancelled, ..Default::default() };
    }
    run(task, control, report)
}

// ---- a job ----------------------------------------------------------------------

/// The least between two reports of progress.
const BEAT: Duration = if cfg!(test) { Duration::ZERO } else { Duration::from_millis(100) };
/// What is copied between two looks at the reins.
const CHUNK: u64 = if cfg!(test) { 64 << 10 } else { 4 << 20 };

type Step<T> = Result<T, Stopped>;

/// What was said "for all".
#[derive(Default)]
struct Policy {
    files: Option<Answer>,
    folders: Option<Answer>,
    others: Option<Answer>,
    failures: bool,
}

/// A source copied across, to be removed once the copy is safe.
struct Moved {
    path: PathBuf,
    was: (u64, u64, i64, i64, u64),
    dir: bool,
}

fn identity(m: &Metadata) -> (u64, u64, i64, i64, u64) {
    (m.dev(), m.ino(), m.mtime(), m.mtime_nsec(), m.len())
}

pub struct Run<'a> {
    control: &'a Control,
    report: &'a dyn Fn(&Progress),
    pub progress: Progress,
    policy: Policy,
    last: Option<Instant>,
    /// The rate's last sample: when, and how many bytes.
    sample: Option<(Instant, u64)>,
    lost: HashSet<&'static str>,
    /// Going across filesystems: written out as it goes.
    across: bool,
}

/// Does `task`, reporting on it; its last progress, `Done` or `Cancelled`.
pub fn run(task: &Task, control: &Control, report: &dyn Fn(&Progress)) -> Progress {
    let mut r = Run {
        control,
        report,
        progress: Progress::default(),
        policy: Policy::default(),
        last: None,
        sample: None,
        lost: HashSet::new(),
        across: false,
    };
    let done = match task {
        Task::Copy { from, to } => r.transfer(from, to, false),
        Task::Move { from, to } => r.transfer(from, to, true),
        Task::Link { from, to } => r.link(from, to),
        Task::Delete { paths } => r.delete(paths),
        Task::Trash { paths } => r.trash(paths),
        Task::Restore { items } => r.restore(items),
        Task::Erase { items } => r.erase(items),
    };
    r.progress.state = if done.is_ok() { State::Done } else { State::Cancelled };
    r.progress.question = None;
    r.progress.lost = {
        let mut lost: Vec<_> = r.lost.iter().copied().collect();
        lost.sort_unstable();
        lost
    };
    r.progress
}

impl Run<'_> {
    /// Where it stops when told, waits when told, and says how far it is.
    fn check(&mut self) -> Step<()> {
        let mut told = self.control.told();
        if let Some(left) = told.left.as_mut() {
            if *left == 0 {
                told.cancelled = true;
            } else {
                *left -= 1;
            }
        }
        if told.paused && !told.cancelled {
            let state = std::mem::replace(&mut self.progress.state, State::Paused);
            self.progress.rate = 0.0;
            (self.report)(&self.progress);
            while told.paused && !told.cancelled {
                told = self.control.wake.wait(told).unwrap_or_else(|e| e.into_inner());
            }
            self.progress.state = state;
            self.sample = None;
            self.last = None;
        }
        if told.cancelled {
            return Err(Stopped);
        }
        drop(told);
        if self.last.is_none_or(|t| t.elapsed() >= BEAT) {
            self.say();
        }
        Ok(())
    }

    fn say(&mut self) {
        let now = Instant::now();
        self.last = Some(now);
        match self.sample {
            Some((then, bytes)) if now.duration_since(then) >= Duration::from_millis(500) => {
                let rate = self.progress.bytes.saturating_sub(bytes) as f64 / now.duration_since(then).as_secs_f64();
                // The last second or so, not the last half of one.
                self.progress.rate = if self.progress.rate == 0.0 { rate } else { self.progress.rate * 0.6 + rate * 0.4 };
                self.sample = Some((now, self.progress.bytes));
            }
            None => self.sample = Some((now, self.progress.bytes)),
            _ => {}
        }
        (self.report)(&self.progress);
    }

    /// Waits for an answer, unless one was given for all of its kind.
    fn ask(&mut self, question: Question) -> Step<Answer> {
        let given = match &question {
            Question::Exists { merge: true, .. } => self.policy.folders,
            Question::Exists { replace: true, .. } => self.policy.files,
            Question::Exists { .. } => self.policy.others,
            Question::Failed { .. } => self.policy.failures.then_some(Answer::Skip),
            Question::Space { .. } => None,
        };
        if let Some(answer) = given {
            return Ok(answer);
        }
        // What was answered before this was asked answers something else.
        self.control.told().answer = None;
        let state = std::mem::replace(&mut self.progress.state, State::Asking);
        self.progress.question = Some(question.clone());
        self.progress.rate = 0.0;
        (self.report)(&self.progress);
        let mut told = self.control.told();
        let (answer, for_all) = loop {
            if told.cancelled {
                return Err(Stopped);
            }
            match told.answer.take() {
                Some((a, all)) if question.takes(a) => break (a, all),
                _ => told = self.control.wake.wait(told).unwrap_or_else(|e| e.into_inner()),
            }
        };
        drop(told);
        self.progress.state = state;
        self.progress.question = None;
        self.sample = None;
        if answer == Answer::Cancel {
            self.control.cancel();
            return Err(Stopped);
        }
        if for_all {
            match &question {
                Question::Exists { merge: true, .. } => self.policy.folders = Some(answer),
                Question::Exists { replace: true, .. } => self.policy.files = Some(answer),
                Question::Exists { .. } => self.policy.others = Some(answer),
                Question::Failed { .. } => self.policy.failures = answer == Answer::Skip,
                Question::Space { .. } => {}
            }
        }
        (self.report)(&self.progress);
        Ok(answer)
    }

    /// It failed: again (true), or left out (false).
    fn failed(&mut self, path: &Path, error: impl ToString) -> Step<bool> {
        if self.control.cancelled() {
            return Err(Stopped);
        }
        let again = self.ask(Question::Failed { path: path.to_owned(), error: error.to_string() })? == Answer::Retry;
        if !again {
            self.progress.skipped.push(path.to_owned());
        }
        Ok(again)
    }

    /// `f` until it works, or None when it is left out.
    fn trying<T>(&mut self, path: &Path, mut f: impl FnMut(&mut Self) -> Step<io::Result<T>>) -> Step<Option<T>> {
        loop {
            self.check()?;
            match f(self)? {
                Ok(v) => return Ok(Some(v)),
                Err(e) => {
                    if !self.failed(path, why(&e))? {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// Counts what is in `paths`, so there is a whole to be part of.
    fn measure(&mut self, paths: &[PathBuf]) -> Step<()> {
        self.progress.state = State::Preparing;
        let mut stack: Vec<PathBuf> = paths.to_vec();
        while let Some(path) = stack.pop() {
            self.check()?;
            let Ok(m) = fs::symlink_metadata(&path) else { continue };
            self.progress.total_files += 1;
            if m.is_dir() {
                stack.extend(fs::read_dir(&path).into_iter().flatten().flatten().map(|e| e.path()));
            } else if m.is_file() {
                self.progress.total_bytes += m.len();
            }
        }
        self.progress.state = State::Running;
        Ok(())
    }

    // ---- copy and move -----------------------------------------------------

    fn transfer(&mut self, from: &[PathBuf], to: &Path, moving: bool) -> Step<()> {
        let Some(there) = self.trying(to, |_| Ok(folder(to)))? else { return Ok(()) };
        // What only changes its name is not counted inside.
        let device = |p: &Path| fs::symlink_metadata(p).map(|m| m.dev()).ok();
        let far: Vec<PathBuf> = from.iter().filter(|f| !moving || device(f) != Some(there.dev())).cloned().collect();
        self.progress.total_files = (from.len() - far.len()) as u64;
        self.measure(&far)?;
        self.across = far.iter().any(|f| device(f) != Some(there.dev()));
        while self.across {
            let free = rustix::fs::statvfs(to).map(|s| s.f_bavail * s.f_frsize).unwrap_or(u64::MAX);
            if free >= self.progress.total_bytes {
                break;
            }
            let question = Question::Space { to: to.to_owned(), needed: self.progress.total_bytes, available: free };
            if self.ask(question)? == Answer::Skip {
                break;
            }
        }
        for source in from {
            self.check()?;
            self.one(source, to, moving)?;
        }
        if self.across && !moving {
            self.settle(to)?;
        }
        Ok(())
    }

    /// One of the things asked for, into `to`.
    fn one(&mut self, source: &Path, to: &Path, moving: bool) -> Step<()> {
        let Some(name) = source.file_name() else {
            self.failed(source, "it has no name")?;
            return Ok(());
        };
        let target = to.join(name);
        // Where things really are, links followed but for the last name.
        let real = |p: &Path| fs::canonicalize(p).ok();
        let parent = source.parent().and_then(|p| real(if p.as_os_str().is_empty() { Path::new(".") } else { p }));
        let at = real(to);
        let Some(m) = self.trying(source, |_| Ok(fs::symlink_metadata(source)))? else { return Ok(()) };
        // Into itself, however it is spelt: it would never end.
        if m.is_dir()
            && let (Some(own), Some(at)) = (real(source), &at)
            && at.starts_with(&own)
        {
            self.progress.skipped.push(source.to_owned());
            let question = Question::Failed { path: source.to_owned(), error: "a folder cannot go into itself".into() };
            self.ask(question)?;
            return Ok(());
        }
        if parent.is_some() && parent == at {
            // Moved where it is: there already. Copied where it is: a copy.
            if moving {
                self.progress.files += 1;
                self.progress.made.push(source.to_owned());
                return Ok(());
            }
            let target = free_name(&target);
            let (_, at) = self.copy(source, &target, &mut None)?;
            self.progress.made.extend(at);
            return Ok(());
        }
        if !moving {
            let (_, at) = self.copy(source, &target, &mut None)?;
            self.progress.made.extend(at);
            return Ok(());
        }
        if let Some(at) = self.rename(source, &target)? {
            self.progress.made.extend(at);
            return Ok(());
        }
        // Another filesystem: copied, safe on the disk, then removed.
        if self.progress.total_bytes == 0 && !self.across {
            self.measure(&[source.to_owned()])?;
        }
        self.across = true;
        let mut copied = Some(Vec::new());
        let (_, at) = self.copy(source, &target, &mut copied)?;
        let copied = copied.unwrap_or_default();
        if !copied.is_empty() && self.settle(to)? {
            self.remove_moved(copied)?;
        }
        self.progress.made.extend(at);
        Ok(())
    }

    /// Everything written to `to`'s filesystem on the disk: false when
    /// it cannot be known.
    fn settle(&mut self, to: &Path) -> Step<bool> {
        let state = std::mem::replace(&mut self.progress.state, State::Finishing);
        self.say();
        let done = self.trying(to, |_| Ok(File::open(to).and_then(|f| Ok(rustix::fs::syncfs(&f)?))))?;
        self.progress.state = state;
        Ok(done.is_some())
    }

    /// The sources of what was copied across, removed, the last first; one
    /// that changed since it was copied stays.
    fn remove_moved(&mut self, copied: Vec<Moved>) -> Step<()> {
        for m in copied.into_iter().rev() {
            // Not a check that cancels: the copy is whole, and half a
            // source is worse than the rest of it gone.
            let Ok(now) = fs::symlink_metadata(&m.path) else { continue };
            let removed = if m.dir {
                // Not empty: something in it was left out, or is new.
                match fs::remove_dir(&m.path) {
                    Err(e) if e.kind() == ErrorKind::DirectoryNotEmpty => Ok(()),
                    other => other,
                }
            } else if identity(&now) != m.was {
                Err(io::Error::other("it changed while it was moved: the copy is of how it was"))
            } else {
                fs::remove_file(&m.path)
            };
            if let Err(e) = removed {
                self.progress.skipped.push(m.path.clone());
                if self.control.cancelled() {
                    return Err(Stopped);
                }
                self.ask(Question::Failed { path: m.path, error: why(&e) })?;
            }
        }
        Ok(())
    }

    /// Moved by its name alone: where it is now (none when it was left
    /// out), or None when it is on another filesystem.
    fn rename(&mut self, from: &Path, to: &Path) -> Step<Option<Option<PathBuf>>> {
        let mut to = to.to_owned();
        let mut replace = false;
        loop {
            self.check()?;
            self.progress.current = from.to_owned();
            let done = if replace { fs::rename(from, &to) } else { rename_new(from, &to) };
            match done {
                Ok(()) => {
                    // Two names of one file: rename(2) leaves both.
                    if replace
                        && let (Ok(a), Ok(b)) = (fs::symlink_metadata(from), fs::symlink_metadata(&to))
                        && (a.dev(), a.ino()) == (b.dev(), b.ino())
                    {
                        let _ = fs::remove_file(from);
                    }
                    self.progress.files += 1;
                    return Ok(Some(Some(to)));
                }
                Err(e) if e.kind() == ErrorKind::CrossesDevices => return Ok(None),
                Err(e) if !replace && e.kind() == ErrorKind::AlreadyExists => match self.taken(from, &to)? {
                    Taken::Merge => return Ok(Some(self.merge(from, &to)?)),
                    Taken::Replace => replace = true,
                    Taken::Other(other) => to = other,
                    Taken::Skip => return Ok(Some(None)),
                    Taken::Again => {}
                },
                Err(e) => {
                    replace = false;
                    if !self.failed(from, why(&e))? {
                        return Ok(Some(None));
                    }
                }
            }
        }
    }

    /// A folder's entries moved into the one there, then the folder itself
    /// gone if nothing is left in it.
    fn merge(&mut self, from: &Path, to: &Path) -> Step<Option<PathBuf>> {
        let Some(entries) = self.trying(from, |_| Ok(entries(from)))? else { return Ok(None) };
        self.progress.total_files += entries.len() as u64;
        for name in entries {
            let (source, target) = (from.join(&name), to.join(&name));
            if self.rename(&source, &target)?.is_none() {
                // A mount inside it, or a subvolume.
                let mut copied = Some(Vec::new());
                self.measure(std::slice::from_ref(&source))?;
                self.across = true;
                self.copy(&source, &target, &mut copied)?;
                let copied = copied.unwrap_or_default();
                if !copied.is_empty() && self.settle(to)? {
                    self.remove_moved(copied)?;
                }
            }
        }
        match fs::remove_dir(from) {
            Err(e) if e.kind() != ErrorKind::DirectoryNotEmpty => {
                self.failed(from, why(&e))?;
            }
            _ => {}
        }
        self.progress.files += 1;
        Ok(Some(to.to_owned()))
    }

    /// The name `to` is taken: what then.
    fn taken(&mut self, from: &Path, to: &Path) -> Step<Taken> {
        let source = match fs::symlink_metadata(from) {
            Ok(m) => m,
            Err(e) => return Ok(if self.failed(from, why(&e))? { Taken::Again } else { Taken::Skip }),
        };
        let target = match fs::symlink_metadata(to) {
            Ok(m) => m,
            // Gone meanwhile.
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Taken::Again),
            Err(e) => return Ok(if self.failed(to, why(&e))? { Taken::Again } else { Taken::Skip }),
        };
        let question = Question::Exists {
            from: from.to_owned(),
            to: to.to_owned(),
            source: Seen::of(&source),
            target: Seen::of(&target),
            merge: source.is_dir() && target.is_dir(),
            replace: !source.is_dir() && !target.is_dir(),
        };
        Ok(match self.ask(question)? {
            Answer::Replace => Taken::Replace,
            Answer::Merge => Taken::Merge,
            Answer::KeepBoth => Taken::Other(free_name(to)),
            _ => {
                self.progress.skipped.push(from.to_owned());
                Taken::Skip
            }
        })
    }

    /// `from` copied as `to`: whether the whole of it was, and where it is.
    /// With `moved`, what of it was copied is added there.
    fn copy(&mut self, from: &Path, to: &Path, moved: &mut Option<Vec<Moved>>) -> Step<(bool, Option<PathBuf>)> {
        loop {
            self.check()?;
            self.progress.current = from.to_owned();
            let m = match fs::symlink_metadata(from) {
                Ok(m) => m,
                Err(e) => {
                    if self.failed(from, why(&e))? {
                        continue;
                    }
                    return Ok((false, None));
                }
            };
            let kind = m.file_type();
            if kind.is_dir() {
                return self.copy_folder(from, &m, to, moved);
            }
            if kind.is_file() || kind.is_symlink() {
                let before = self.progress.bytes;
                let done = self.copy_file(from, &m, to)?;
                // Whatever came of it, its part of the whole is behind.
                self.progress.bytes = before + if kind.is_file() { m.len() } else { 0 };
                self.progress.files += 1;
                if let (Some((_, read)), Some(list)) = (&done, moved) {
                    list.push(Moved { path: from.to_owned(), was: *read, dir: false });
                }
                return Ok((done.is_some(), done.map(|(at, _)| at)));
            }
            // A device, a socket, a pipe: not a thing to copy.
            debug_assert!(kind.is_fifo() || kind.is_socket() || kind.is_block_device() || kind.is_char_device());
            self.progress.skipped.push(from.to_owned());
            self.progress.files += 1;
            return Ok((false, None));
        }
    }

    fn copy_folder(&mut self, from: &Path, m: &Metadata, to: &Path, moved: &mut Option<Vec<Moved>>) -> Step<(bool, Option<PathBuf>)> {
        let mut to = to.to_owned();
        // Made here, its own until it is whole; or one that was there.
        let fresh = loop {
            self.check()?;
            match rustix::fs::mkdir(&to, Mode::RWXU).map_err(io::Error::from) {
                Ok(()) => break true,
                Err(e) if e.kind() == ErrorKind::AlreadyExists => match self.taken(from, &to)? {
                    Taken::Merge => break false,
                    Taken::Other(other) => to = other,
                    Taken::Again | Taken::Replace => {}
                    Taken::Skip => {
                        self.pass_over(from)?;
                        return Ok((false, None));
                    }
                },
                Err(e) => {
                    if !self.failed(&to, why(&e))? {
                        self.pass_over(from)?;
                        return Ok((false, None));
                    }
                }
            }
        };
        // Before its entries: removed after them, the last first.
        if let Some(list) = moved {
            list.push(Moved { path: from.to_owned(), was: identity(m), dir: true });
        }
        let mut whole = true;
        match self.trying(from, |_| Ok(entries(from)))? {
            Some(names) => {
                for name in names {
                    whole &= self.copy(&from.join(&name), &to.join(&name), moved)?.0;
                }
            }
            None => whole = false,
        }
        self.progress.files += 1;
        if fresh && let Ok(made) = File::open(&to) {
            self.keep(File::open(from).ok().as_ref(), m, made.as_fd());
        }
        Ok((whole, Some(to)))
    }

    /// `path` is left out: what is in it counts as behind.
    fn pass_over(&mut self, path: &Path) -> Step<()> {
        let (files, bytes) = (self.progress.total_files, self.progress.total_bytes);
        let state = self.progress.state;
        self.measure(&[path.to_owned()])?;
        self.progress.state = state;
        self.progress.files += self.progress.total_files - files;
        self.progress.bytes += self.progress.total_bytes - bytes;
        self.progress.total_files = files;
        self.progress.total_bytes = bytes;
        Ok(())
    }

    /// One file or link, copied as `to`: where it is, and what the source
    /// was when it was read; None when it was left out.
    #[allow(clippy::type_complexity)]
    fn copy_file(&mut self, from: &Path, m: &Metadata, to: &Path) -> Step<Option<(PathBuf, (u64, u64, i64, i64, u64))>> {
        let mut to = to.to_owned();
        let mut replace = false;
        // Asked before the hour it takes, where the answer is known to be
        // needed; and again at the end, if the name was taken meanwhile.
        while !replace && fs::symlink_metadata(&to).is_ok() {
            match self.taken(from, &to)? {
                Taken::Replace => replace = true,
                Taken::Other(other) => to = other,
                Taken::Skip => return Ok(None),
                Taken::Merge | Taken::Again => {}
            }
        }
        let before = self.progress.bytes;
        'again: loop {
            self.check()?;
            self.progress.bytes = before;
            let staged = if m.file_type().is_symlink() { Staged::link(from, m, &to) } else { self.staged(from, &to)? };
            let (staged, read) = match staged {
                Ok(s) => s,
                Err(e) => {
                    if self.failed(from, why(&e))? {
                        continue;
                    }
                    return Ok(None);
                }
            };
            loop {
                match staged.place(&to, replace) {
                    Ok(()) => return Ok(Some((to, read))),
                    Err(e) if !replace && e.kind() == ErrorKind::AlreadyExists => match self.taken(from, &to)? {
                        Taken::Replace => replace = true,
                        Taken::Other(other) => to = other,
                        Taken::Skip => return Ok(None),
                        Taken::Merge | Taken::Again => {}
                    },
                    Err(e) => {
                        if self.failed(&to, why(&e))? {
                            continue 'again;
                        }
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// The data of `from`, whole, beside where it goes, under no name yet.
    #[allow(clippy::type_complexity)]
    fn staged(&mut self, from: &Path, to: &Path) -> Step<io::Result<(Staged, (u64, u64, i64, i64, u64))>> {
        let opened = fs::OpenOptions::new()
            .read(true)
            // Not a pipe put there since it was looked at, waited on forever.
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(from);
        let source = match opened {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let m = match source.metadata() {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Ok(Err(io::Error::other("it is no longer a file"))),
            Err(e) => return Ok(Err(e)),
        };
        let mut staged = match Staged::file(to) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let data = {
            let Some(out) = staged.data.take() else { return Ok(Err(io::Error::other("nothing to write to"))) };
            let copied = self.data(&source, &out, m.len())?;
            staged.data = Some(out);
            copied
        };
        if let Err(e) = data {
            return Ok(Err(e));
        }
        // Written to while it was read (a log): a copy of it all the same,
        // but not one to remove the source for.
        if source.metadata().is_ok_and(|now| identity(&now) != identity(&m)) {
            self.lost.insert("changes");
        }
        if let Some(out) = &staged.data {
            self.keep(Some(&source), &m, out.as_fd());
            // Some filesystems say only now that it did not fit.
            if self.across
                && let Err(e) = out.sync_data()
            {
                return Ok(Err(e));
            }
        }
        Ok(Ok((staged, identity(&m))))
    }

    /// The bytes of `source` into `out`: shared where the filesystem shares
    /// them, else copied by the kernel, else through here. Holes stay holes.
    fn data(&mut self, source: &File, out: &File, size: u64) -> Step<io::Result<()>> {
        if size == 0 {
            return Ok(Ok(()));
        }
        if rustix::fs::ioctl_ficlone(out, source).is_ok() {
            self.progress.bytes += size;
            return Ok(Ok(()));
        }
        let before = self.progress.bytes;
        let mut kernel = true;
        let mut at = 0u64;
        let mut buffer = Vec::new();
        while at < size {
            // The next of it that is not a hole.
            let data = match rustix::fs::seek(source, SeekFrom::Data(at)) {
                Ok(d) => d.min(size),
                Err(rustix::io::Errno::NXIO) => size,
                Err(_) => at,
            };
            let hole = match rustix::fs::seek(source, SeekFrom::Hole(data)) {
                Ok(h) if h > data => h.min(size),
                _ => size,
            };
            at = data;
            self.progress.bytes = before + at;
            while at < hole {
                self.check()?;
                let want = (hole - at).min(CHUNK);
                let copied = if kernel {
                    let (mut a, mut b) = (at, at);
                    match rustix::fs::copy_file_range(source, Some(&mut a), out, Some(&mut b), want as usize) {
                        Ok(0) => {
                            kernel = false;
                            continue;
                        }
                        Ok(n) => n as u64,
                        Err(
                            rustix::io::Errno::XDEV | rustix::io::Errno::NOSYS | rustix::io::Errno::INVAL | rustix::io::Errno::OPNOTSUPP | rustix::io::Errno::PERM,
                        ) => {
                            kernel = false;
                            continue;
                        }
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(e) => return Ok(Err(e.into())),
                    }
                } else {
                    buffer.resize(1 << 20, 0);
                    match through(source, out, at, want, &mut buffer) {
                        Ok(0) => return Ok(Err(io::Error::other("it became shorter while it was copied"))),
                        Ok(n) => n,
                        Err(e) => return Ok(Err(e)),
                    }
                };
                if self.across {
                    write_behind(out, at, copied);
                }
                at += copied;
                self.progress.bytes = before + at;
            }
        }
        // A hole at the end is length and nothing else.
        Ok(rustix::fs::ftruncate(out, size).map_err(Into::into))
    }

    /// Permissions, times and attributes as they were, as far as they go.
    fn keep(&mut self, source: Option<&File>, m: &Metadata, to: BorrowedFd<'_>) {
        if let Some(source) = source
            && attributes(source, to).is_err()
        {
            self.lost.insert("attributes");
        }
        // Owned by whoever copies: with another owner, set-uid would be a
        // different thing than it was.
        let own = fs::metadata(format!("/proc/self/fd/{}", to.as_raw_fd())).is_ok_and(|n| (n.uid(), n.gid()) == (m.uid(), m.gid()));
        let mode = m.mode() & if own { 0o7777 } else { 0o777 };
        if rustix::fs::fchmod(to, Mode::from_raw_mode(mode)).is_err() {
            self.lost.insert("permissions");
        }
        if rustix::fs::futimens(to, &times(m)).is_err() {
            self.lost.insert("times");
        }
    }

    // ---- the others ----------------------------------------------------------

    fn link(&mut self, from: &[PathBuf], to: &Path) -> Step<()> {
        self.progress.state = State::Running;
        self.progress.total_files = from.len() as u64;
        for source in from {
            let Some(name) = source.file_name() else { continue };
            let mut target = to.join(name);
            // Beside itself, under its own name, it can only be another.
            if fs::symlink_metadata(&target).is_ok() {
                target = free_name(&target);
            }
            let made = self.trying(source, |_| match std::os::unix::fs::symlink(source, &target) {
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    target = free_name(&target);
                    Ok(std::os::unix::fs::symlink(source, &target))
                }
                other => Ok(other),
            })?;
            if made.is_some() {
                self.progress.made.push(target);
            }
            self.progress.files += 1;
        }
        Ok(())
    }

    fn delete(&mut self, paths: &[PathBuf]) -> Step<()> {
        self.measure(paths)?;
        for path in paths {
            self.erase_one(path)?;
        }
        Ok(())
    }

    /// `path` gone for good: whether all of it is.
    fn erase_one(&mut self, path: &Path) -> Step<bool> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            self.failed(path, "it cannot be deleted")?;
            return Ok(false);
        };
        let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
        let open = |_: &mut Self| Ok(rustix::fs::open(parent, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).map_err(io::Error::from));
        let Some(dir) = self.trying(path, open)? else { return Ok(false) };
        self.erase_at(dir.as_fd(), name, path)
    }

    /// By names within open folders, never by a path: what a folder was
    /// swapped for meanwhile is not followed.
    fn erase_at(&mut self, dir: BorrowedFd<'_>, name: &OsStr, path: &Path) -> Step<bool> {
        loop {
            self.check()?;
            self.progress.current = path.to_owned();
            let stat = match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(s) => s,
                Err(rustix::io::Errno::NOENT) => return Ok(true),
                Err(e) => {
                    if self.failed(path, why(&e.into()))? {
                        continue;
                    }
                    return Ok(false);
                }
            };
            let is_dir = rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir();
            let flags = if is_dir { AtFlags::REMOVEDIR } else { AtFlags::empty() };
            let mut whole = true;
            if is_dir {
                let opened = rustix::fs::openat(
                    dir,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                );
                let names = match opened.and_then(|d| Ok((names_in(d.as_fd())?, d))) {
                    Ok(found) => Some(found),
                    // An empty one need not be read to be removed.
                    Err(e) => match rustix::fs::unlinkat(dir, name, flags) {
                        Ok(()) => {
                            self.progress.files += 1;
                            return Ok(true);
                        }
                        Err(_) => {
                            if self.failed(path, why(&e.into()))? {
                                continue;
                            }
                            return Ok(false);
                        }
                    },
                };
                if let Some((names, inside)) = names {
                    for n in names {
                        whole &= self.erase_at(inside.as_fd(), &n, &path.join(&n))?;
                    }
                }
                // Something of it stays by an answer, and so does it.
                if !whole {
                    return Ok(false);
                }
            }
            match rustix::fs::unlinkat(dir, name, flags) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {
                    if !is_dir {
                        self.progress.bytes += stat.st_size.max(0) as u64;
                    }
                    self.progress.files += 1;
                    return Ok(true);
                }
                Err(e) => {
                    if !self.failed(path, why(&e.into()))? {
                        return Ok(false);
                    }
                }
            }
        }
    }

    fn trash(&mut self, paths: &[PathBuf]) -> Step<()> {
        self.progress.state = State::Running;
        self.progress.total_files = paths.len() as u64;
        for path in paths {
            self.progress.current = path.to_owned();
            if let Some(item) = self.trying(path, |_| Ok(trash::put(path)))? {
                self.progress.made.push(item.file);
            }
            self.progress.files += 1;
        }
        Ok(())
    }

    fn restore(&mut self, items: &[trash::Item]) -> Step<()> {
        self.progress.state = State::Running;
        self.progress.total_files = items.len() as u64;
        for item in items {
            self.check()?;
            let folder = item.path.parent().unwrap_or(Path::new("/")).to_owned();
            // The folder it was in may have gone with it.
            if self.trying(&folder, |_| Ok(fs::create_dir_all(&folder)))?.is_none() {
                self.progress.files += 1;
                continue;
            }
            let at = match self.rename(&item.file, &item.path)? {
                Some(at) => at,
                // Its folder is on another filesystem now.
                None => {
                    self.across = true;
                    self.measure(std::slice::from_ref(&item.file))?;
                    let mut copied = Some(Vec::new());
                    let (_, at) = self.copy(&item.file, &item.path, &mut copied)?;
                    let copied = copied.unwrap_or_default();
                    if !copied.is_empty() && self.settle(&folder)? {
                        self.remove_moved(copied)?;
                    }
                    at
                }
            };
            // All of it back: the trash no longer knows of it.
            if fs::symlink_metadata(&item.file).is_err() {
                let _ = fs::remove_file(&item.info);
            }
            self.progress.made.extend(at);
        }
        Ok(())
    }

    fn erase(&mut self, items: &[trash::Item]) -> Step<()> {
        let files: Vec<PathBuf> = items.iter().map(|i| i.file.clone()).collect();
        self.measure(&files)?;
        for item in items {
            // What it was goes last: cancelled before, it can be restored
            // still, whatever is left of it.
            if self.erase_one(&item.file)? {
                let _ = fs::remove_file(&item.info);
            }
        }
        Ok(())
    }
}

/// What is done about a name that is taken.
enum Taken {
    Replace,
    Merge,
    Other(PathBuf),
    Skip,
    /// No longer taken, as far as is known.
    Again,
}

// ---- a file before it has its name ----------------------------------------------

/// Complete beside where it goes, and gone if it is never put there.
struct Staged {
    /// A file's data; none for a link.
    data: Option<File>,
    /// Its name until then, where it must have one.
    beside: Option<PathBuf>,
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(beside) = &self.beside {
            let _ = fs::remove_file(beside);
        }
    }
}

/// A name of our own in `to`'s folder.
fn beside(to: &Path) -> PathBuf {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    to.with_file_name(format!(".genoma-{}-{n}-{at:x}.part", std::process::id()))
}

impl Staged {
    fn file(to: &Path) -> io::Result<Staged> {
        let dir = to.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let flags = OFlags::TMPFILE | OFlags::RDWR | OFlags::CLOEXEC;
        if let Ok(fd) = rustix::fs::open(dir, flags, Mode::RUSR | Mode::WUSR) {
            return Ok(Staged { data: Some(File::from(fd)), beside: None });
        }
        let name = beside(to);
        let file = fs::OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&name)?;
        Ok(Staged { data: Some(file), beside: Some(name) })
    }

    #[allow(clippy::type_complexity)]
    fn link(from: &Path, m: &Metadata, to: &Path) -> io::Result<(Staged, (u64, u64, i64, i64, u64))> {
        let name = beside(to);
        std::os::unix::fs::symlink(fs::read_link(from)?, &name)?;
        let _ = rustix::fs::utimensat(CWD, &name, &times(m), AtFlags::SYMLINK_NOFOLLOW);
        Ok((Staged { data: None, beside: Some(name) }, identity(m)))
    }

    /// Given the name `to`; `replace` the one there, else refused if it is
    /// taken.
    fn place(&self, to: &Path, replace: bool) -> io::Result<()> {
        match (&self.beside, &self.data) {
            (Some(name), _) if replace => fs::rename(name, to),
            (Some(name), _) => rename_new(name, to),
            (None, Some(data)) if replace => {
                // A file with no name cannot take another's: by a name of
                // its own first.
                let name = beside(to);
                name_it(data, &name)?;
                fs::rename(&name, to).inspect_err(|_| {
                    let _ = fs::remove_file(&name);
                })
            }
            (None, Some(data)) => name_it(data, to),
            (None, None) => Err(io::Error::other("nothing to put in place")),
        }
    }
}

/// A file opened with no name (`O_TMPFILE`) given one, if it is free.
fn name_it(data: &File, to: &Path) -> io::Result<()> {
    let own = format!("/proc/self/fd/{}", data.as_raw_fd());
    Ok(rustix::fs::linkat(CWD, own, CWD, to, AtFlags::SYMLINK_FOLLOW)?)
}

/// rename(2), refused when `to` is taken.
pub(crate) fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::io::Errno;
    match rustix::fs::renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        // A filesystem that cannot refuse: looked at first.
        Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => match fs::symlink_metadata(to) {
            Ok(_) => Err(Errno::EXIST.into()),
            Err(e) if e.kind() == ErrorKind::NotFound => fs::rename(from, to),
            Err(e) => Err(e),
        },
        Err(e) => Err(e.into()),
    }
}

// ---- parts -------------------------------------------------------------------------

fn folder(path: &Path) -> io::Result<Metadata> {
    let m = fs::metadata(path)?;
    if m.is_dir() { Ok(m) } else { Err(io::Error::from(ErrorKind::NotADirectory)) }
}

fn entries(dir: &Path) -> io::Result<Vec<OsString>> {
    fs::read_dir(dir)?.map(|e| e.map(|e| e.file_name())).collect()
}

/// The names in an open folder, its own two left out.
fn names_in(dir: BorrowedFd<'_>) -> Result<Vec<OsString>, rustix::io::Errno> {
    let mut out = Vec::new();
    for e in rustix::fs::Dir::read_from(dir)? {
        let name = e?.file_name().to_bytes().to_vec();
        if name != b"." && name != b".." {
            out.push(OsString::from_vec(name));
        }
    }
    Ok(out)
}

/// Why, in words.
pub(crate) fn why(e: &io::Error) -> String {
    match e.kind() {
        ErrorKind::NotFound => "not found".into(),
        ErrorKind::PermissionDenied => "permission denied".into(),
        ErrorKind::StorageFull => "the disk is full".into(),
        ErrorKind::QuotaExceeded => "the quota is used up".into(),
        ErrorKind::ReadOnlyFilesystem => "the disk is read-only".into(),
        ErrorKind::AlreadyExists => "the name is taken".into(),
        ErrorKind::NotADirectory => "not a folder".into(),
        ErrorKind::FileTooLarge => "too large for the disk it goes to".into(),
        ErrorKind::InvalidFilename => "the name is not one the disk takes".into(),
        _ => {
            // "Input/output error (os error 5)": the number says nothing.
            let text = e.to_string();
            text.split(" (os error").next().unwrap_or(&text).to_owned()
        }
    }
}

fn times(m: &Metadata) -> Timestamps {
    Timestamps {
        last_access: Timespec { tv_sec: m.atime(), tv_nsec: m.atime_nsec() as _ },
        last_modification: Timespec { tv_sec: m.mtime(), tv_nsec: m.mtime_nsec() as _ },
    }
}

/// `want` bytes at `at`, through here.
fn through(source: &File, out: &File, at: u64, want: u64, buffer: &mut [u8]) -> io::Result<u64> {
    use std::os::unix::fs::FileExt;
    let want = (want as usize).min(buffer.len());
    let n = loop {
        match source.read_at(&mut buffer[..want], at) {
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            other => break other?,
        }
    };
    out.write_all_at(&buffer[..n], at)?;
    Ok(n as u64)
}

/// Sent to the disk as it is written, and waited for a step behind: what is
/// said to be done is, and a stick does not take minutes after the end.
fn write_behind(out: &File, at: u64, length: u64) {
    let fd = out.as_raw_fd();
    unsafe {
        libc::sync_file_range(fd, at as i64, length as i64, libc::SYNC_FILE_RANGE_WRITE);
        if at >= CHUNK {
            libc::sync_file_range(
                fd,
                (at - CHUNK) as i64,
                CHUNK as i64,
                libc::SYNC_FILE_RANGE_WAIT_BEFORE | libc::SYNC_FILE_RANGE_WRITE | libc::SYNC_FILE_RANGE_WAIT_AFTER,
            );
        }
    }
}

/// The attributes people and programs put on a file, and its access list.
fn attributes(from: &File, to: BorrowedFd<'_>) -> io::Result<()> {
    let mut names = vec![0u8; 64 << 10];
    let n = match rustix::fs::flistxattr(from, &mut names) {
        Ok(n) => n,
        // None there, none to lose.
        Err(rustix::io::Errno::OPNOTSUPP | rustix::io::Errno::NOSYS) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut failed = Ok(());
    for name in names[..n].split(|b| *b == 0).filter(|s| !s.is_empty()) {
        if !(name.starts_with(b"user.") || name.starts_with(b"system.posix_acl_")) {
            continue;
        }
        let name = OsStr::from_bytes(name);
        let mut value = vec![0u8; 64 << 10];
        let set = rustix::fs::fgetxattr(from, name, &mut value)
            .and_then(|n| rustix::fs::fsetxattr(to, name, &value[..n], rustix::fs::XattrFlags::empty()));
        if let Err(e) = set {
            failed = Err(e.into());
        }
    }
    failed
}

/// A name like `path`'s that is free: "a.txt" → "a (2).txt", "a (2).txt" →
/// "a (3).txt".
pub fn free_name(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().as_bytes().to_vec();
    // A folder's dots are part of its name.
    let dir = fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    let (stem, ending) = if dir { (name.as_slice(), &b""[..]) } else { split_ending(&name) };
    let (stem, first) = match numbered(stem) {
        Some((s, n)) => (s, n + 1),
        None => (stem, 2),
    };
    for n in first.. {
        let number = format!(" ({n})");
        // Within what a name may be, at the stem's expense.
        let room = 255usize.saturating_sub(number.len() + ending.len());
        let mut stem = stem;
        if stem.len() > room {
            let mut cut = room;
            while cut > 0 && stem[cut] & 0xc0 == 0x80 {
                cut -= 1;
            }
            stem = &stem[..cut];
        }
        let next = path.with_file_name(OsStr::from_bytes(&[stem, number.as_bytes(), ending].concat()));
        if fs::symlink_metadata(&next).is_err() {
            return next;
        }
    }
    path.to_owned()
}

/// "name (3)" → "name" and 3.
fn numbered(stem: &[u8]) -> Option<(&[u8], u64)> {
    let inside = stem.strip_suffix(b")")?;
    let open = inside.windows(2).rposition(|w| w == b" (")?;
    let digits = &inside[open + 2..];
    if digits.is_empty() || digits.len() > 9 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some((&stem[..open], std::str::from_utf8(digits).ok()?.parse().ok()?))
}

/// A name and its ending: "a.tar.gz" → "a", ".tar.gz"; ".bashrc" has none.
fn split_ending(name: &[u8]) -> (&[u8], &[u8]) {
    let Some(dot) = name.iter().rposition(|b| *b == b'.').filter(|i| *i > 0) else { return (name, b"") };
    let before = &name[..dot];
    match before.iter().rposition(|b| *b == b'.').filter(|i| *i > 0) {
        Some(tar) if before[tar..].eq_ignore_ascii_case(b".tar") => name.split_at(tar),
        _ => name.split_at(dot),
    }
}

// ---- at once ------------------------------------------------------------------------

/// Whether a folder can hold a thing of this name, in words when not.
pub fn check_name(name: &OsStr) -> Result<(), String> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err("a name cannot be empty".into());
    }
    if bytes == b"." || bytes == b".." || bytes.contains(&b'/') || bytes.contains(&0) {
        return Err("that cannot be a name".into());
    }
    if bytes.len() > 255 {
        return Err("the name is too long".into());
    }
    Ok(())
}

/// Whether moving all of `paths` into `folder` is only changing names: they
/// are on its disk.
pub fn same_disk(paths: &[PathBuf], folder: &Path) -> bool {
    let Ok(to) = fs::metadata(folder) else { return false };
    paths.iter().all(|p| fs::symlink_metadata(p).is_ok_and(|m| m.dev() == to.dev()))
}

/// `path` called `name`, where it is; never over another.
pub fn rename(path: &Path, name: &OsStr) -> Result<PathBuf, String> {
    check_name(name)?;
    let to = path.with_file_name(name);
    if to == path {
        return Ok(to);
    }
    match rename_new(path, &to) {
        Ok(()) => Ok(to),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            // On a disk that does not tell cases apart the one there is it.
            let one = |p: &Path| fs::symlink_metadata(p).map(|m| (m.dev(), m.ino(), m.nlink())).ok();
            let same = one(path).is_some_and(|a| a.2 == 1 && one(&to) == Some(a));
            if same { fs::rename(path, &to).map(|()| to).map_err(|e| why(&e)) } else { Err(why(&e)) }
        }
        Err(e) => Err(why(&e)),
    }
}

pub fn make_folder(dir: &Path, name: &OsStr) -> Result<PathBuf, String> {
    check_name(name)?;
    let path = dir.join(name);
    fs::create_dir(&path).map_err(|e| why(&e))?;
    Ok(path)
}

pub fn make_file(dir: &Path, name: &OsStr) -> Result<PathBuf, String> {
    check_name(name)?;
    let path = dir.join(name);
    fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| why(&e))?;
    Ok(path)
}

/// What is under some paths, for a properties panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measure {
    pub bytes: u64,
    /// On the disk: less where there are holes, more for small files.
    pub on_disk: u64,
    pub files: u64,
    pub folders: u64,
    /// Folders that could not be read.
    pub unread: u64,
}

/// Counts it, a file with several names once, until `go_on` says no; told
/// of as it grows.
pub fn measure(paths: &[PathBuf], mut go_on: impl FnMut(&Measure) -> bool) -> Option<Measure> {
    let mut out = Measure::default();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut stack: Vec<PathBuf> = paths.to_vec();
    let mut last = Instant::now();
    while let Some(path) = stack.pop() {
        let Ok(m) = fs::symlink_metadata(&path) else { continue };
        if m.nlink() > 1 && !m.is_dir() && !seen.insert((m.dev(), m.ino())) {
            continue;
        }
        out.on_disk += m.blocks() * 512;
        if m.is_dir() {
            out.folders += 1;
            match fs::read_dir(&path) {
                Ok(read) => stack.extend(read.flatten().map(|e| e.path())),
                Err(_) => out.unread += 1,
            }
        } else {
            out.files += 1;
            out.bytes += m.len();
        }
        if last.elapsed() >= BEAT {
            last = Instant::now();
            if !go_on(&out) {
                return None;
            }
        }
    }
    Some(out)
}


#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        os::unix::fs::{PermissionsExt, symlink},
    };

    use super::*;

    /// A folder of this test's own on a filesystem.
    fn scratch_in(base: &Path, name: &str) -> PathBuf {
        static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = base.join(format!("genoma-files-{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        scratch_in(&std::env::temp_dir(), name)
    }

    /// Another filesystem than `scratch`'s, where there is one.
    fn elsewhere(name: &str) -> Option<PathBuf> {
        let base = Path::new("/dev/shm");
        let here = fs::metadata(std::env::temp_dir()).unwrap().dev();
        (fs::metadata(base).is_ok_and(|m| m.dev() != here)).then(|| scratch_in(base, name))
    }

    struct Gone(Vec<PathBuf>);

    impl Drop for Gone {
        fn drop(&mut self) {
            for p in &self.0 {
                let _ = fs::remove_dir_all(p);
            }
        }
    }

    /// Runs it, each question answered by `answer`; the questions asked.
    fn answered(task: &Task, control: &Control, answer: impl Fn(&Question) -> (Answer, bool)) -> (Progress, Vec<Question>) {
        let asked = RefCell::new(Vec::new());
        let report = |p: &Progress| {
            if p.state == State::Asking
                && let Some(q) = &p.question
            {
                asked.borrow_mut().push(q.clone());
                let (a, all) = answer(q);
                control.answer(a, all);
            }
        };
        let done = run(task, control, &report);
        (done, asked.into_inner())
    }

    fn quietly(task: &Task) -> Progress {
        let (done, asked) = answered(task, &Control::default(), |q| panic!("asked {q:?}"));
        assert!(asked.is_empty());
        done
    }

    /// Everything under `root`: what each is, and what it holds.
    fn tree(root: &Path) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        let mut stack = vec![root.to_owned()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap().flatten() {
                let path = e.path();
                let m = fs::symlink_metadata(&path).unwrap();
                let what = if m.is_dir() {
                    stack.push(path.clone());
                    "folder".into()
                } else if m.is_symlink() {
                    format!("link to {}", fs::read_link(&path).unwrap().display())
                } else if m.is_file() {
                    {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    fs::read(&path).ok().hash(&mut h);
                    format!("file {:x}", h.finish())
                }
                } else {
                    "special".into()
                };
                out.insert(path.strip_prefix(root).unwrap().to_owned(), what);
            }
        }
        out
    }

    fn leftovers(root: &Path) -> usize {
        tree(root).keys().filter(|p| p.to_string_lossy().contains(".genoma-")).count()
    }

    fn sample(dir: &Path) {
        fs::create_dir_all(dir.join("sub/deep")).unwrap();
        fs::write(dir.join("a.txt"), "hello").unwrap();
        fs::write(dir.join("empty"), "").unwrap();
        fs::write(dir.join("sub/big"), vec![7u8; 150 << 10]).unwrap();
        fs::write(dir.join("sub/deep/run.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(dir.join("sub/deep/run.sh"), fs::Permissions::from_mode(0o750)).unwrap();
        symlink("a.txt", dir.join("link")).unwrap();
        symlink("/nowhere/at/all", dir.join("sub/lost")).unwrap();
        fs::write(dir.join(OsStr::from_bytes(b"caf\xe9")), "latin").unwrap();
        // A hole, then data, then a hole.
        let f = File::create(dir.join("sub/holes")).unwrap();
        use std::os::unix::fs::FileExt;
        f.write_all_at(b"middle", 1 << 20).unwrap();
        f.set_len(3 << 20).unwrap();
        for p in ["sub/deep/run.sh", "sub/deep", "sub"] {
            let f = File::open(dir.join(p)).unwrap();
            f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000)).unwrap();
        }
    }

    #[test]
    fn a_tree_is_copied_as_it_is() {
        for far in [false, true] {
            let a = scratch("copy");
            let b = match far {
                false => scratch("copied"),
                true => match elsewhere("copied") {
                    Some(b) => b,
                    None => continue,
                },
            };
            let _gone = Gone(vec![a.clone(), b.clone()]);
            sample(&a.join("in"));
            let before = tree(&a);
            let done = quietly(&Task::Copy { from: vec![a.join("in")], to: b.clone() });
            assert_eq!(done.state, State::Done);
            assert_eq!(done.made, [b.join("in")]);
            assert_eq!((done.files, done.bytes), (done.total_files, done.total_bytes), "the whole of it is behind");
            assert_eq!(tree(&b), before, "far {far}");
            assert_eq!(tree(&a), before, "the source is as it was");
            let m = fs::metadata(b.join("in/sub/deep/run.sh")).unwrap();
            assert_eq!(m.mode() & 0o777, 0o750);
            for p in ["in/sub/deep/run.sh", "in/sub/deep", "in/sub"] {
                assert_eq!(fs::metadata(b.join(p)).unwrap().mtime(), 1_000_000_000, "{p}");
            }
            let holes = fs::metadata(b.join("in/sub/holes")).unwrap();
            assert!(holes.blocks() * 512 < 1 << 20, "its holes stay holes: {}", holes.blocks() * 512);
            assert!(done.lost.is_empty(), "{:?}", done.lost);
        }
    }

    #[test]
    fn a_move_leaves_nothing_behind() {
        for far in [false, true] {
            let a = scratch("move");
            let b = match far {
                false => scratch("moved"),
                true => match elsewhere("moved") {
                    Some(b) => b,
                    None => continue,
                },
            };
            let _gone = Gone(vec![a.clone(), b.clone()]);
            sample(&a.join("in"));
            let before = tree(&a);
            let done = quietly(&Task::Move { from: vec![a.join("in")], to: b.clone() });
            assert_eq!(done.state, State::Done);
            assert_eq!(tree(&b), before);
            assert!(tree(&a).is_empty(), "{:?}", tree(&a));
        }
    }

    #[test]
    fn a_name_that_is_taken_is_asked_about() {
        let dir = scratch("taken");
        let _gone = Gone(vec![dir.clone()]);
        let (a, b) = (dir.join("a"), dir.join("b"));
        for moving in [false, true] {
            for answer in [Answer::Replace, Answer::KeepBoth, Answer::Skip, Answer::Cancel] {
                for d in [&a, &b] {
                    let _ = fs::remove_dir_all(d);
                    fs::create_dir(d).unwrap();
                }
                fs::write(a.join("f.txt"), "new").unwrap();
                fs::write(b.join("f.txt"), "old").unwrap();
                let (from, to) = (vec![a.join("f.txt")], b.clone());
                let task = if moving { Task::Move { from, to } } else { Task::Copy { from, to } };
                let (done, asked) = answered(&task, &Control::default(), |_| (answer, false));
                assert!(matches!(&asked[..], [Question::Exists { replace: true, merge: false, .. }]), "{asked:?}");
                let read = |p: PathBuf| fs::read_to_string(p).ok();
                let (there, other) = (read(b.join("f.txt")), read(b.join("f (2).txt")));
                match answer {
                    Answer::Replace => assert_eq!((there.as_deref(), other), (Some("new"), None)),
                    Answer::KeepBoth => assert_eq!((there.as_deref(), other.as_deref()), (Some("old"), Some("new"))),
                    _ => assert_eq!((there.as_deref(), other), (Some("old"), None)),
                }
                let went = matches!(answer, Answer::Replace | Answer::KeepBoth);
                assert_eq!(a.join("f.txt").exists(), !(moving && went));
                assert_eq!(done.state, if answer == Answer::Cancel { State::Cancelled } else { State::Done });
                assert_eq!(leftovers(&dir), 0);
            }
        }
    }

    #[test]
    fn folders_merge_only_when_told_and_for_all() {
        for moving in [false, true] {
            let dir = scratch("merge");
            let _gone = Gone(vec![dir.clone()]);
            for (p, text) in [("a/d/x/1", "one"), ("a/d/2", "two"), ("a/d/same", "new"), ("b/d/x/3", "three"), ("b/d/same", "old")] {
                fs::create_dir_all(dir.join(p).parent().unwrap()).unwrap();
                fs::write(dir.join(p), text).unwrap();
            }
            let (from, to) = (vec![dir.join("a/d")], dir.join("b"));
            let task = if moving { Task::Move { from, to } } else { Task::Copy { from, to } };
            let (done, asked) = answered(&task, &Control::default(), |q| match q {
                Question::Exists { merge: true, .. } => (Answer::Merge, true),
                _ => (Answer::Skip, false),
            });
            assert_eq!(done.state, State::Done);
            let folders = asked.iter().filter(|q| matches!(q, Question::Exists { merge: true, .. })).count();
            assert_eq!((folders, asked.len()), (1, 2), "d, then same; x by the answer for all: {asked:?}");
            let b = tree(&dir.join("b"));
            for p in ["d/x/1", "d/x/3", "d/2", "d/same"] {
                assert!(b.contains_key(Path::new(p)), "{p}");
            }
            assert_eq!(fs::read_to_string(dir.join("b/d/same")).unwrap(), "old");
            let left: Vec<PathBuf> = tree(&dir.join("a")).into_keys().collect();
            match moving {
                true => assert_eq!(left, [PathBuf::from("d"), PathBuf::from("d/same")], "what was skipped stays"),
                false => assert_eq!(left.len(), 5),
            }
        }
    }

    #[test]
    fn a_file_never_takes_a_folders_place() {
        let dir = scratch("kinds");
        let _gone = Gone(vec![dir.clone()]);
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b/x")).unwrap();
        fs::write(dir.join("a/x"), "file").unwrap();
        fs::write(dir.join("b/x/kept"), "kept").unwrap();
        let task = Task::Copy { from: vec![dir.join("a/x")], to: dir.join("b") };
        // Replace is not an answer to this: the first that is, is taken.
        let control = Control::default();
        let asked = RefCell::new(0);
        let done = run(&task, &control, &|p| {
            if p.state == State::Asking {
                *asked.borrow_mut() += 1;
                assert_eq!(p.question.as_ref().unwrap().answers(), ["keep_both", "skip", "cancel"]);
                control.answer(Answer::KeepBoth, false);
            }
        });
        assert_eq!(done.made, [dir.join("b/x (2)")]);
        assert_eq!(fs::read_to_string(dir.join("b/x/kept")).unwrap(), "kept");
    }

    #[test]
    fn a_copy_where_it_is_is_another_and_a_move_is_nothing() {
        let dir = scratch("same");
        let _gone = Gone(vec![dir.clone()]);
        fs::write(dir.join("f.tar.gz"), "x").unwrap();
        fs::create_dir(dir.join("d.d")).unwrap();
        symlink(&dir, dir.join("loop")).unwrap();
        // By another path to the same folder too.
        let done = quietly(&Task::Copy { from: vec![dir.join("f.tar.gz"), dir.join("d.d")], to: dir.join("loop") });
        assert_eq!(done.made, [dir.join("loop/f (2).tar.gz"), dir.join("loop/d.d (2)")]);
        let before = tree(&dir);
        quietly(&Task::Move { from: vec![dir.join("f.tar.gz")], to: dir.join("loop") });
        assert_eq!(tree(&dir), before);
    }

    #[test]
    fn a_folder_does_not_go_into_itself() {
        let dir = scratch("itself");
        let _gone = Gone(vec![dir.clone()]);
        sample(&dir.join("in"));
        symlink(dir.join("in/sub"), dir.join("way")).unwrap();
        let before = tree(&dir);
        for to in ["in", "in/sub/deep", "way"] {
            for moving in [false, true] {
                let (from, to) = (vec![dir.join("in")], dir.join(to));
                let task = if moving { Task::Move { from, to } } else { Task::Copy { from, to } };
                let (_, asked) = answered(&task, &Control::default(), |_| (Answer::Skip, false));
                assert!(matches!(&asked[..], [Question::Failed { .. }]), "{asked:?}");
                assert_eq!(tree(&dir), before);
            }
        }
    }

    #[test]
    fn a_name_taken_while_it_is_copied_is_asked_about() {
        let dir = scratch("race");
        let _gone = Gone(vec![dir.clone()]);
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();
        fs::write(dir.join("a/f"), vec![1u8; 1 << 20]).unwrap();
        // Read and written through here: not shared, all at once.
        let control = Control::default();
        let asked = RefCell::new(0);
        let raced = RefCell::new(false);
        let done = run(&Task::Copy { from: vec![dir.join("a/f")], to: dir.join("b") }, &control, &|p| {
            // After it was looked for, before it is put in place.
            if p.state == State::Running && p.bytes > 0 && p.files == 0 {
                *raced.borrow_mut() = true;
                fs::write(dir.join("b/f"), "somebody else's").unwrap();
            }
            if p.state == State::Asking {
                *asked.borrow_mut() += 1;
                control.answer(Answer::Skip, false);
            }
        });
        assert_eq!(done.state, State::Done);
        // On a disk that shares the data there is no between.
        if !*raced.borrow() {
            return;
        }
        assert_eq!(*asked.borrow(), 1);
        assert_eq!(fs::read_to_string(dir.join("b/f")).unwrap(), "somebody else's");
        assert_eq!(leftovers(&dir), 0);
    }

    #[test]
    fn what_is_put_in_place_refuses_a_name_taken() {
        let dir = scratch("place");
        let _gone = Gone(vec![dir.clone()]);
        fs::write(dir.join("there"), "old").unwrap();
        for hidden in [false, true] {
            let to = dir.join("there");
            let mut staged = Staged::file(&to).unwrap();
            if hidden {
                let name = beside(&to);
                staged = Staged { data: Some(File::create_new(&name).unwrap()), beside: Some(name) };
            }
            use std::io::Write;
            staged.data.as_mut().unwrap().write_all(b"new").unwrap();
            assert_eq!(staged.place(&to, false).unwrap_err().kind(), ErrorKind::AlreadyExists);
            assert_eq!(fs::read_to_string(&to).unwrap(), "old");
            staged.place(&to, true).unwrap();
            assert_eq!(fs::read_to_string(&to).unwrap(), "new");
            fs::write(&to, "old").unwrap();
            drop(staged);
            assert_eq!(leftovers(&dir), 0);
        }
        // Replacing a link replaces the link.
        symlink("there", dir.join("way")).unwrap();
        let staged = Staged::file(&dir.join("way")).unwrap();
        staged.place(&dir.join("way"), true).unwrap();
        assert_eq!(fs::read_to_string(dir.join("there")).unwrap(), "old");
    }

    #[test]
    fn a_source_changed_since_it_was_copied_stays() {
        let (dir, Some(far)) = (scratch("changed"), elsewhere("changed")) else { return };
        let _gone = Gone(vec![dir.clone(), far.clone()]);
        fs::write(dir.join("f"), "first").unwrap();
        fs::write(dir.join("g"), "other").unwrap();
        let control = Control::default();
        let asked = RefCell::new(0);
        let done = run(&Task::Move { from: vec![dir.join("f"), dir.join("g")], to: far.clone() }, &control, &|p| {
            if p.state == State::Finishing && dir.join("f").exists() && !dir.join("done").exists() {
                fs::write(dir.join("f"), "second, longer").unwrap();
                fs::write(dir.join("done"), "").unwrap();
            }
            if p.state == State::Asking {
                *asked.borrow_mut() += 1;
                control.answer(Answer::Skip, false);
            }
        });
        assert_eq!(*asked.borrow(), 1);
        assert_eq!(fs::read_to_string(dir.join("f")).unwrap(), "second, longer");
        assert_eq!(fs::read_to_string(far.join("f")).unwrap(), "first");
        assert!(!dir.join("g").exists() && far.join("g").exists());
        assert_eq!(done.skipped, [dir.join("f")]);
    }

    #[test]
    fn what_cannot_be_read_is_left_and_told() {
        if rustix::process::getuid().is_root() {
            return;
        }
        let (dir, Some(far)) = (scratch("locked"), elsewhere("locked")) else { return };
        let _gone = Gone(vec![dir.clone(), far.clone()]);
        sample(&dir.join("in"));
        fs::set_permissions(dir.join("in/a.txt"), fs::Permissions::from_mode(0)).unwrap();
        let (done, asked) = answered(&Task::Move { from: vec![dir.join("in")], to: far.clone() }, &Control::default(), |_| (Answer::Skip, false));
        assert!(matches!(&asked[..], [Question::Failed { error, .. }] if error == "permission denied"), "{asked:?}");
        assert_eq!(done.state, State::Done);
        let left: Vec<PathBuf> = tree(&dir).into_keys().filter(|p| !p.ends_with("a.txt")).collect();
        assert_eq!(left, [PathBuf::from("in")], "the rest moved");
        assert!(dir.join("in/a.txt").exists());
        fs::set_permissions(dir.join("in/a.txt"), fs::Permissions::from_mode(0o600)).unwrap();
        // And then, again, it is.
        let (done, _) = answered(&Task::Move { from: vec![dir.join("in")], to: far.clone() }, &Control::default(), |_| (Answer::Merge, true));
        assert_eq!(done.state, State::Done);
        assert!(tree(&dir).is_empty());
    }

    #[test]
    fn pipes_and_sockets_are_left_out() {
        let dir = scratch("special");
        let _gone = Gone(vec![dir.clone()]);
        fs::create_dir_all(dir.join("in")).unwrap();
        fs::create_dir_all(dir.join("out")).unwrap();
        rustix::fs::mknodat(CWD, dir.join("in/pipe"), rustix::fs::FileType::Fifo, Mode::RWXU, 0).unwrap();
        fs::write(dir.join("in/file"), "x").unwrap();
        let done = quietly(&Task::Copy { from: vec![dir.join("in")], to: dir.join("out") });
        assert_eq!(done.skipped, [dir.join("in/pipe")]);
        assert!(dir.join("out/in/file").exists() && !dir.join("out/in/pipe").exists());
    }

    #[test]
    fn deleting_follows_no_link() {
        let dir = scratch("delete");
        let _gone = Gone(vec![dir.clone()]);
        sample(&dir.join("gone"));
        sample(&dir.join("kept"));
        symlink(dir.join("kept"), dir.join("gone/way")).unwrap();
        symlink(dir.join("kept"), dir.join("way")).unwrap();
        let before = tree(&dir.join("kept"));
        let done = quietly(&Task::Delete { paths: vec![dir.join("gone"), dir.join("way"), dir.join("never")] });
        assert_eq!(done.state, State::Done);
        assert_eq!(tree(&dir.join("kept")), before);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let (_, asked) = answered(&Task::Delete { paths: vec![PathBuf::from("/")] }, &Control::default(), |_| (Answer::Skip, false));
        assert!(matches!(&asked[..], [Question::Failed { .. }]));
    }

    #[test]
    fn paused_it_waits_and_goes_on() {
        let dir = scratch("pause");
        let _gone = Gone(vec![dir.clone()]);
        sample(&dir.join("in"));
        fs::create_dir(dir.join("out")).unwrap();
        let control = std::sync::Arc::new(Control::default());
        control.pause(true);
        let seen = Mutex::new(Vec::new());
        let done = std::thread::scope(|s| {
            let c = control.clone();
            s.spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                c.pause(false);
            });
            run(&Task::Copy { from: vec![dir.join("in")], to: dir.join("out") }, &control, &|p| seen.lock().unwrap().push(p.state))
        });
        assert_eq!(done.state, State::Done);
        assert!(seen.lock().unwrap().contains(&State::Paused));
        assert_eq!(tree(&dir.join("out/in")), tree(&dir.join("in")));
    }

    /// Cancelled at every place it can be, whatever it was doing: nothing
    /// is lost, nothing is half there, nothing of ours is left.
    #[test]
    fn cancelled_anywhere_nothing_is_lost() {
        for (what, far) in [("copy", false), ("copy", true), ("move", true), ("move", false), ("delete", false)] {
            for at in 0.. {
                let a = scratch("cancel");
                let b = match far {
                    false => scratch("cancelled"),
                    true => match elsewhere("cancelled") {
                        Some(b) => b,
                        None => break,
                    },
                };
                let _gone = Gone(vec![a.clone(), b.clone()]);
                sample(&a.join("in"));
                fs::write(a.join("other"), "other").unwrap();
                let before = tree(&a);
                let from = vec![a.join("in"), a.join("other")];
                let task = match what {
                    "copy" => Task::Copy { from, to: b.clone() },
                    "move" => Task::Move { from, to: b.clone() },
                    _ => Task::Delete { paths: from },
                };
                let control = Control::default();
                control.told().left = Some(at);
                let (done, _) = answered(&task, &control, |q| panic!("asked {q:?}"));
                let (left, there) = (tree(&a), tree(&b));
                assert_eq!(leftovers(&a) + leftovers(&b), 0, "{what} {at}");
                for (p, was) in left.iter().chain(&there) {
                    assert_eq!(before.get(p), Some(was), "{what} {at}: {} is whole", p.display());
                }
                if what != "delete" {
                    for (p, was) in &before {
                        assert!(left.get(p) == Some(was) || there.get(p) == Some(was), "{what} {at}: {} is lost", p.display());
                    }
                }
                if what == "copy" {
                    assert_eq!(left, before);
                }
                if done.state == State::Done {
                    assert!(at > 2, "{what}: it looks at the reins");
                    break;
                }
            }
        }
    }

    /// A random thing, the same for the same seed.
    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n
        }
    }

    /// A random tree, names few enough to collide, every file unlike any
    /// other.
    fn grow(dice: &mut Dice, dir: &Path, depth: u64, count: &mut u64) {
        for name in ["a", "b", "c.txt", "d.tar.gz"] {
            let path = dir.join(name);
            match dice.roll(5) {
                0 if depth > 0 => {
                    fs::create_dir(&path).unwrap();
                    grow(dice, &path, depth - 1, count);
                }
                1 | 2 => {
                    *count += 1;
                    fs::write(&path, format!("{count}:").repeat(dice.roll(50) as usize + 1)).unwrap();
                }
                3 if dice.roll(2) == 0 => symlink("b", &path).unwrap(),
                _ => {}
            }
        }
    }

    fn contents(root: &Path) -> BTreeSet<String> {
        tree(root).into_values().filter(|w| w.starts_with("file")).collect()
    }

    /// Any trees, any answers, any moment to stop: what was not answered
    /// "replace" is somewhere still, and whole.
    #[test]
    fn whatever_is_answered_only_what_was_replaced_is_gone() {
        for seed in 0..600u64 {
            let mut dice = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) + 1);
            let far = seed % 3 == 0;
            let a = scratch("dice");
            let b = match far {
                false => scratch("diced"),
                true => elsewhere("diced").unwrap_or_else(|| scratch("diced")),
            };
            let _gone = Gone(vec![a.clone(), b.clone()]);
            let mut count = 0;
            grow(&mut dice, &a, 3, &mut count);
            grow(&mut dice, &b, 3, &mut count);
            let source = tree(&a);
            let all: BTreeSet<String> = contents(&a).union(&contents(&b)).cloned().collect();
            let from: Vec<PathBuf> = fs::read_dir(&a).unwrap().flatten().map(|e| e.path()).collect();
            let moving = dice.roll(2) == 0;
            let may_replace = dice.roll(2) == 0;
            let task = if moving { Task::Move { from, to: b.clone() } } else { Task::Copy { from, to: b.clone() } };
            let control = Control::default();
            if dice.roll(3) == 0 {
                control.told().left = Some(dice.roll(40));
            }
            let replaced = RefCell::new(BTreeSet::new());
            let dice = RefCell::new(dice);
            let (_, _) = answered(&task, &control, |q| {
                let mut dice = dice.borrow_mut();
                let answers: Vec<Answer> = q.answers().iter().filter_map(|a| Answer::from_name(a)).filter(|a| *a != Answer::Retry && (may_replace || *a != Answer::Replace)).collect();
                // Cancel is rare, or there is little to see.
                let mut answer = answers[dice.roll(answers.len() as u64) as usize];
                if answer == Answer::Cancel && dice.roll(4) > 0 {
                    answer = Answer::Skip;
                }
                let for_all = answer != Answer::Replace && dice.roll(4) == 0;
                if answer == Answer::Replace
                    && let Question::Exists { to, .. } = q
                    && let Ok(m) = fs::symlink_metadata(to)
                    && m.is_file()
                {
                    let one = tree(to.parent().unwrap());
                    replaced.borrow_mut().insert(one[Path::new(to.file_name().unwrap())].clone());
                }
                (answer, for_all)
            });
            let now: BTreeSet<String> = contents(&a).union(&contents(&b)).cloned().collect();
            for c in &now {
                assert!(all.contains(c), "seed {seed}: something half made: {c}");
            }
            for c in &all {
                assert!(now.contains(c) || replaced.borrow().contains(c), "seed {seed} moving {moving} far {far}: {c} is lost");
            }
            if !moving {
                assert_eq!(tree(&a), source, "seed {seed}: a copy leaves the source as it was");
            }
            assert_eq!(leftovers(&a) + leftovers(&b), 0, "seed {seed}");
        }
    }

    /// Run where a small disk can be made:
    /// `unshare -Urm sh -c 'mount -t tmpfs -o size=4m tmpfs /mnt && GENOMA_SMALL_DISK=/mnt cargo test -p genoma-services full -- --ignored'`
    #[test]
    #[ignore]
    fn a_full_disk_loses_nothing() {
        let Some(small) = std::env::var_os("GENOMA_SMALL_DISK").map(PathBuf::from) else { return };
        for moving in [false, true] {
            let a = scratch("full");
            let b = scratch_in(&small, "full");
            let _gone = Gone(vec![a.clone(), b.clone()]);
            fs::write(a.join("small"), vec![1u8; 100 << 10]).unwrap();
            fs::write(a.join("large"), vec![2u8; 9 << 20]).unwrap();
            fs::write(a.join("zlast"), vec![3u8; 100 << 10]).unwrap();
            let before = tree(&a);
            let from = ["small", "large", "zlast"].map(|n| a.join(n)).to_vec();
            let task = if moving { Task::Move { from, to: b.clone() } } else { Task::Copy { from, to: b.clone() } };
            let (done, asked) = answered(&task, &Control::default(), |_| (Answer::Skip, false));
            assert!(matches!(&asked[0], Question::Space { .. }), "{asked:?}");
            assert!(matches!(&asked[1], Question::Failed { error, .. } if error == "the disk is full"), "{asked:?}");
            assert_eq!(done.state, State::Done);
            assert_eq!(leftovers(&b), 0);
            assert!(!b.join("large").exists() && b.join("zlast").exists());
            assert!(a.join("large").exists());
            assert_eq!(a.join("small").exists(), !moving);
            for (p, was) in &before {
                assert!(tree(&a).get(p) == Some(was) || tree(&b).get(p) == Some(was));
            }
        }
    }

    #[test]
    fn jobs_wait_their_turn_and_can_be_cancelled_in_it() {
        let dir = scratch("turns");
        let _gone = Gone(vec![dir.clone()]);
        fs::create_dir(dir.join("out")).unwrap();
        for n in 0..4 {
            fs::write(dir.join(n.to_string()), "x").unwrap();
        }
        let copy = |n: usize| Task::Copy { from: vec![dir.join(n.to_string())], to: dir.join("out") };
        let controls: Vec<Control> = (0..4).map(|_| Control::default()).collect();
        controls[0].pause(true);
        let order = Mutex::new(Vec::new());
        fs::create_dir(dir.join("links")).unwrap();
        std::thread::scope(|s| {
            for n in 0..4 {
                let (task, control, order) = (copy(n), &controls[n], &order);
                s.spawn(move || {
                    let done = run_in_turn(&task, control, &|_| {});
                    order.lock().unwrap().push((n, done.state));
                });
                std::thread::sleep(Duration::from_millis(30));
            }
            controls[2].cancel();
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(*order.lock().unwrap(), [(2, State::Cancelled)]);
            // What only changes a name goes by.
            let quick = Task::Link { from: vec![dir.join("0")], to: dir.join("links") };
            assert_eq!(run_in_turn(&quick, &Control::default(), &|_| {}).state, State::Done);
            controls[0].pause(false);
        });
        assert_eq!(order.lock().unwrap()[1..], [(0, State::Done), (1, State::Done), (3, State::Done)]);
    }

    #[test]
    fn a_free_name_is_the_next_one() {
        let dir = scratch("free");
        let _gone = Gone(vec![dir.clone()]);
        let next = |name: &str| free_name(&dir.join(name)).file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(next("a.txt"), "a (2).txt");
        fs::write(dir.join("a (2).txt"), "").unwrap();
        assert_eq!(next("a.txt"), "a (3).txt");
        assert_eq!(next("a (2).txt"), "a (3).txt");
        assert_eq!(next("a.tar.gz"), "a (2).tar.gz");
        assert_eq!(next(".bashrc"), ".bashrc (2)");
        assert_eq!(next("noending"), "noending (2)");
        assert_eq!(next("(2)"), "(2) (2)");
        fs::create_dir(dir.join("v1.2")).unwrap();
        assert_eq!(next("v1.2"), "v1.2 (2)");
        let long = next(&format!("{}.txt", "è".repeat(125)));
        assert!(long.len() <= 255 && long.ends_with(" (2).txt"), "{long}");
    }

    #[test]
    fn a_new_name_is_never_anothers() {
        let dir = scratch("rename");
        let _gone = Gone(vec![dir.clone()]);
        fs::write(dir.join("a"), "a").unwrap();
        fs::write(dir.join("b"), "b").unwrap();
        assert_eq!(rename(&dir.join("a"), OsStr::new("b")).unwrap_err(), "the name is taken");
        assert_eq!(fs::read_to_string(dir.join("b")).unwrap(), "b");
        for bad in ["", ".", "..", "x/y", &"n".repeat(256)] {
            assert!(rename(&dir.join("a"), OsStr::new(bad)).is_err(), "{bad}");
        }
        // Two names of one file are two names.
        fs::hard_link(dir.join("a"), dir.join("c")).unwrap();
        assert!(rename(&dir.join("a"), OsStr::new("c")).is_err());
        assert_eq!(rename(&dir.join("a"), OsStr::new("d")).unwrap(), dir.join("d"));
        assert_eq!(make_folder(&dir, OsStr::new("b")).unwrap_err(), "the name is taken");
        assert!(make_file(&dir, OsStr::new("new")).is_ok() && make_file(&dir, OsStr::new("new")).is_err());
    }

    #[test]
    fn a_folder_is_measured() {
        let dir = scratch("measure");
        let _gone = Gone(vec![dir.clone()]);
        sample(&dir.join("in"));
        fs::hard_link(dir.join("in/sub/big"), dir.join("in/again")).unwrap();
        let m = measure(&[dir.join("in")], |_| true).unwrap();
        assert_eq!((m.folders, m.files), (3, 8));
        assert_eq!(m.bytes, 5 + 5 + (150 << 10) + 10 + (3 << 20) + "a.txt".len() as u64 + "/nowhere/at/all".len() as u64);
    }
}
