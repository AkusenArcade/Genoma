//! The building blocks a service written in Luau is made of (PRD §9.2),
//! Quickshell's `Quickshell.Io` counterparts: `g.timer`, `g.process`,
//! `g.file` and `g.socket`.
//!
//! None of them owns a thread. Each asks the runtime's loop, through
//! [`Host`], to wake it when a file descriptor is readable or an interval has
//! passed: a process's output and its exit (a pidfd), a file's directory
//! (inotify), a socket. Whatever the interface's top-level code creates
//! belongs to the load, and the next load stops it: a reload kills the old
//! processes and closes the old sockets.

use std::{
    cell::{Cell, RefCell},
    fs::File,
    io::{ErrorKind, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    time::Duration,
};

use calloop::RegistrationToken;
use genoma_core::reactive::{Signal, current_owner, on_cleanup, owned_by};
use mlua::{Function, IntoLuaMulti, LuaString, Table, UserData, UserDataFields, UserDataMethods};
use rustix::fs::inotify;

/// What the runtime's loop offers the script.
pub trait Host {
    /// Calls `tick` after `after`, and again after whatever it returns; `None`
    /// stops it.
    fn timer(&self, after: Duration, tick: Box<dyn FnMut() -> Option<Duration>>) -> Option<RegistrationToken>;
    /// Calls `ready` whenever `fd` is readable, until it returns false. The
    /// loop owns `fd` and closes it then.
    fn readable(&self, fd: OwnedFd, ready: Box<dyn FnMut(BorrowedFd<'_>) -> bool>) -> Option<RegistrationToken>;
    fn remove(&self, token: RegistrationToken);
    /// A channel into the loop: what is sent from the bus thread is handed
    /// to `deliver` on the main thread.
    fn channel(
        &self,
        deliver: Box<dyn FnMut(crate::dbus::Delivery)>,
    ) -> Option<calloop::channel::Sender<crate::dbus::Delivery>>;
    /// How wide `text` is set in `style` at `size`, logical units.
    fn measure(&self, text: &str, size: f32, style: &genoma_scene::TextStyle<'_>) -> f32;
    /// How tall `text` is, wrapped within `width`, logical units.
    fn measure_wrapped(
        &self,
        text: &str,
        size: f32,
        style: &genoma_scene::TextStyle<'_>,
        width: f32,
        runs: &[genoma_scene::TextRun],
    ) -> f32;
}

pub type HostRef = Rc<dyn Host>;

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// A callback, run with the owner that was current when its source was
/// made: what it starts (a process when another ends, a watcher in a
/// D-Bus reply) belongs to the same load and goes with it. Run with none,
/// it outlived every reload, and a reload at start left two of each.
pub(crate) fn call(f: &Function, what: &str, args: impl IntoLuaMulti, owner: Option<usize>) {
    // A callback of a load that has been replaced: its state is gone. What
    // a g.watch handler starts is detached, so its timers outlive the load
    // they belong to, and fire into nothing.
    if f.weak_lua().try_upgrade().is_none() {
        return;
    }
    if let Err(e) = owned_by(owner, || f.call::<()>(args)) {
        log::error!("{what}: {e}");
    }
}

/// `~/` is the home directory, as in Bioma's paths, and a leading `$NAME`
/// that variable (`$XDG_RUNTIME_DIR/…`, `$NIRI_SOCKET`): the sandbox gives
/// Luau no environment of its own.
pub(crate) fn expand(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest);
    }
    if let Some(rest) = path.strip_prefix('$') {
        let (name, tail) = rest.split_once('/').map_or((rest, ""), |(n, t)| (n, t));
        let base = PathBuf::from(std::env::var(name).unwrap_or_default());
        return if tail.is_empty() { base } else { base.join(tail) };
    }
    PathBuf::from(path)
}

/// The loop's sources a block holds, and what else stopping it means.
pub(crate) struct Stop {
    host: HostRef,
    tokens: RefCell<Vec<RegistrationToken>>,
    pub(crate) also: RefCell<Option<Box<dyn FnOnce()>>>,
    /// Who owned what made it: its callbacks run as that owner.
    pub(crate) owner: Option<usize>,
}

thread_local! {
    /// The load under way or last finished: who owns what is made where
    /// nothing else does.
    static LOAD_OWNER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Names the load's owner (see [`Stop::new`]).
pub(crate) fn set_load_owner(owner: Option<usize>) {
    LOAD_OWNER.with(|l| l.set(owner));
}

impl Stop {
    pub(crate) fn new(host: &HostRef) -> Rc<Stop> {
        // Made where nothing owns it (a g.watch's handler, a click, an IPC
        // verb), it is the load's: a file watched or a timer started there
        // was never stopped, and every reload left its sources open.
        let owner = current_owner().or_else(|| LOAD_OWNER.with(|l| l.get()));
        let stop = Rc::new(Stop { host: host.clone(), tokens: RefCell::default(), also: RefCell::default(), owner });
        // Owned by the load: the next one stops it.
        let s = stop.clone();
        if current_owner().is_some() {
            on_cleanup(move || s.stop());
        } else {
            genoma_core::reactive::owned_by(owner, || on_cleanup(move || s.stop()));
        }
        stop
    }

    pub(crate) fn hold(&self, token: Option<RegistrationToken>) {
        self.tokens.borrow_mut().extend(token);
    }

    pub(crate) fn stop(&self) {
        for token in self.tokens.borrow_mut().drain(..) {
            self.host.remove(token);
        }
        if let Some(also) = self.also.borrow_mut().take() {
            also();
        }
    }
}

/// Work that would hold the loop (a name looked up, a connection made, a
/// TLS or WebSocket handshake) done on a thread of its own: `done` gets the
/// result on the main thread, through a pipe the loop watches. Stopped
/// before then, the result is dropped with the thread's last reference.
pub(crate) fn off_loop<T: Send + 'static>(
    host: &HostRef,
    stop: &Stop,
    name: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(Result<T, String>) + 'static,
) -> mlua::Result<()> {
    let (reader, writer) = rustix::pipe::pipe().map_err(|e| err(format!("{name}: {e}")))?;
    let slot: std::sync::Arc<std::sync::Mutex<Option<Result<T, String>>>> = std::sync::Arc::default();
    let filled = slot.clone();
    std::thread::Builder::new()
        .name(format!("genoma-{name}"))
        .spawn(move || {
            let result = work();
            if let Ok(mut s) = filled.lock() {
                *s = Some(result);
            }
            let _ = rustix::io::write(&writer, &[1]);
        })
        .map_err(|e| err(format!("{name}: no thread: {e}")))?;
    let mut done = Some(done);
    let what = name.to_owned();
    stop.hold(host.readable(
        reader,
        Box::new(move |fd| {
            let mut byte = [0u8; 1];
            let read = rustix::io::read(fd, &mut byte);
            let result = slot.lock().ok().and_then(|mut s| s.take());
            match (result, read) {
                (Some(result), _) => {
                    if let Some(d) = done.take() {
                        d(result);
                    }
                    false
                }
                // The thread ended without a word: it panicked.
                (None, Ok(0)) => {
                    if let Some(d) = done.take() {
                        d(Err(format!("{what}: the work stopped")));
                    }
                    false
                }
                (None, _) => true,
            }
        }),
    ));
    Ok(())
}

/// Bytes into lines: what a stream gives is cut wherever the kernel cut it.
#[derive(Default)]
struct Lines(Vec<u8>);

impl Lines {
    fn feed(&mut self, data: &[u8], mut each: impl FnMut(&str)) {
        self.0.extend_from_slice(data);
        while let Some(end) = self.0.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.0.drain(..=end).collect();
            each(String::from_utf8_lossy(&line[..line.len() - 1]).trim_end_matches('\r'));
        }
    }

    /// The last line, if the stream ended without a newline.
    fn finish(&mut self, mut each: impl FnMut(&str)) {
        if !self.0.is_empty() {
            let line = std::mem::take(&mut self.0);
            each(&String::from_utf8_lossy(&line));
        }
    }
}

/// One read from a readable stream: `false` once it has ended.
fn read_once(file: &mut File, lines: &mut Lines, each: &mut dyn FnMut(&str)) -> bool {
    let mut buf = [0u8; 65536];
    match file.read(&mut buf) {
        Ok(0) => {
            lines.finish(each);
            false
        }
        Ok(n) => {
            lines.feed(&buf[..n], each);
            true
        }
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => true,
        Err(_) => {
            lines.finish(each);
            false
        }
    }
}

// ---- g.timer ---------------------------------------------------------------

pub struct Timer(Rc<Stop>);

impl UserData for Timer {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("stop", |_, this, ()| {
            this.0.stop();
            Ok(())
        });
    }
}

/// `g.timer { every = 2, on_tick = function() … end }`: for a value only
/// polling can give, at the interval the service declares.
pub fn timer(host: &HostRef, t: Table) -> mlua::Result<Timer> {
    let every: f64 = t.get("every")?;
    if !(every > 0.0) {
        return Err(err("g.timer: every is seconds, above 0"));
    }
    let on_tick: Function = t.get("on_tick")?;
    let every = Duration::from_secs_f64(every);
    let stop = Stop::new(host);
    let owner = stop.owner;
    stop.hold(host.timer(
        every,
        Box::new(move || {
            call(&on_tick, "g.timer", (), owner);
            Some(every)
        }),
    ));
    Ok(Timer(stop))
}

/// `g.after(seconds, function() … end)`: once, later; `:stop()` before it
/// fires to keep it from firing.
pub fn after(host: &HostRef, (seconds, f): (f64, Function)) -> mlua::Result<Timer> {
    let stop = Stop::new(host);
    let owner = stop.owner;
    let mut f = Some(f);
    stop.hold(host.timer(
        Duration::from_secs_f64(seconds.max(0.0)),
        Box::new(move || {
            if let Some(f) = f.take() {
                call(&f, "g.after", (), owner);
            }
            None
        }),
    ));
    Ok(Timer(stop))
}

// ---- g.process -------------------------------------------------------------

struct Running {
    child: RefCell<Option<Child>>,
    stdin: RefCell<Option<ChildStdin>>,
    stdout: RefCell<Option<File>>,
    stderr: RefCell<Option<File>>,
    out_lines: RefCell<Lines>,
    err_lines: RefCell<Lines>,
    on_line: Option<Function>,
    on_error_line: Option<Function>,
    exit: Cell<Option<i32>>,
    owner: Option<usize>,
}

impl Running {
    fn out(&self) -> impl FnMut(&str) + '_ {
        move |line| match &self.on_line {
            Some(f) => call(f, "g.process on_line", line.to_owned(), self.owner),
            None => {}
        }
    }

    fn err(&self) -> impl FnMut(&str) + '_ {
        move |line| match &self.on_error_line {
            Some(f) => call(f, "g.process on_error_line", line.to_owned(), self.owner),
            None => log::debug!("process: {line}"),
        }
    }

    /// Everything still in the pipes, read without waiting: the exit is
    /// reported after the last line, as it was written before it.
    fn drain(&self) {
        for (file, lines, out) in [(&self.stdout, &self.out_lines, true), (&self.stderr, &self.err_lines, false)] {
            let Some(f) = file.borrow_mut().take() else { continue };
            let mut f = f;
            let _ = rustix::fs::fcntl_setfl(&f, rustix::fs::OFlags::NONBLOCK);
            let mut l = lines.borrow_mut();
            let mut each: Box<dyn FnMut(&str)> = if out { Box::new(self.out()) } else { Box::new(self.err()) };
            let mut buf = [0u8; 65536];
            loop {
                match f.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => l.feed(&buf[..n], &mut each),
                }
            }
            l.finish(&mut each);
        }
    }
}

pub struct Process {
    running: Rc<Running>,
    stop: Rc<Stop>,
}

impl UserData for Process {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("running", |_, this| Ok(this.running.exit.get().is_none()));
        fields.add_field_method_get("exit_code", |_, this| Ok(this.running.exit.get()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // p:write("text"): to its standard input, as given.
        methods.add_method("write", |_, this, text: LuaString| {
            let mut stdin = this.running.stdin.borrow_mut();
            let Some(stdin) = stdin.as_mut() else { return Err(err("g.process: input closed")) };
            stdin.write_all(&text.as_bytes()).map_err(|e| err(format!("g.process write: {e}")))
        });
        methods.add_method("close_input", |_, this, ()| {
            this.running.stdin.borrow_mut().take();
            Ok(())
        });
        // p:kill(): the exit still comes through on_exit.
        methods.add_method("kill", |_, this, ()| {
            if let Some(child) = this.running.child.borrow_mut().as_mut() {
                let _ = child.kill();
            }
            Ok(())
        });
        // p:signal(2): a signal by number, for a program that ends cleanly
        // on one (a recorder closes its file on SIGINT). The exit still
        // comes through on_exit; an ended process is left alone, its pid
        // being someone else's once it is reaped.
        methods.add_method("signal", |_, this, number: i32| {
            if this.running.exit.get().is_some() {
                return Ok(());
            }
            let Some(signal) = rustix::process::Signal::from_named_raw(number) else {
                return Err(err(format!("g.process signal: no signal {number}")));
            };
            if let Some(child) = this.running.child.borrow().as_ref() {
                if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
                    let _ = rustix::process::kill_process(pid, signal);
                }
            }
            Ok(())
        });
        methods.add_method("stop", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

/// `g.process { command = { "nmcli", "-t", "device" }, on_line = …,
/// on_error_line = …, on_exit = function(code) … end, env = { … } }`. Lines
/// come as the program writes them; `on_exit` after the last one, with the
/// exit code (128 plus the signal for a process killed by one). `env` adds to
/// the shell's environment: what must not be on a command line every user
/// can read (`ps`) travels there. `cwd` is the directory it runs in, for a
/// program whose configuration names files relative to it.
pub fn process(host: &HostRef, t: Table) -> mlua::Result<Process> {
    let command: Vec<String> = t.get("command")?;
    let Some(program) = command.first() else { return Err(err("g.process: command is empty")) };
    let mut command_line = Command::new(program);
    command_line.args(&command[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = t.get::<Option<String>>("cwd")? {
        command_line.current_dir(expand(&cwd));
    }
    if let Some(env) = t.get::<Option<Table>>("env")? {
        for pair in env.pairs::<String, String>() {
            let (key, value) = pair?;
            command_line.env(key, value);
        }
    }
    // A program dies with the shell: killed outright, Genoma runs no
    // cleanup, and a watcher left behind would run for nobody.
    // Safety: one system call between fork and exec, async-signal-safe.
    unsafe {
        command_line.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))
                .map_err(std::io::Error::from)
        });
    }
    let mut child = command_line
        .spawn()
        .map_err(|e| err(format!("g.process {program}: {e}")))?;
    let stdout = child.stdout.take().map(OwnedFd::from);
    let stderr = child.stderr.take().map(OwnedFd::from);
    let pidfd = rustix::process::Pid::from_raw(child.id() as i32)
        .ok_or_else(|| err("g.process: no pid"))
        .and_then(|pid| {
            rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
                .map_err(|e| err(format!("g.process pidfd: {e}")))
        })?;
    let on_exit: Option<Function> = t.get("on_exit")?;
    let dup = |fd: &Option<OwnedFd>| fd.as_ref().and_then(|f| f.try_clone().ok());
    let (out_watch, err_watch) = (dup(&stdout), dup(&stderr));
    let running = Rc::new(Running {
        stdin: RefCell::new(child.stdin.take()),
        child: RefCell::new(Some(child)),
        stdout: RefCell::new(stdout.map(File::from)),
        stderr: RefCell::new(stderr.map(File::from)),
        out_lines: RefCell::default(),
        err_lines: RefCell::default(),
        on_line: t.get("on_line")?,
        on_error_line: t.get("on_error_line")?,
        exit: Cell::new(None),
        owner: current_owner(),
    });
    let stop = Stop::new(host);

    // The pipes, read once per wake: the loop wakes again while more is there.
    for (watch, out) in [(out_watch, true), (err_watch, false)] {
        let Some(watch) = watch else { continue };
        let r = running.clone();
        stop.hold(host.readable(
            watch,
            Box::new(move |_| {
                let file = if out { &r.stdout } else { &r.stderr };
                let mut file = file.borrow_mut();
                let Some(f) = file.as_mut() else { return false };
                let mut lines = if out { r.out_lines.borrow_mut() } else { r.err_lines.borrow_mut() };
                let mut each: Box<dyn FnMut(&str)> = if out { Box::new(r.out()) } else { Box::new(r.err()) };
                read_once(f, &mut lines, &mut each)
            }),
        ));
    }

    // The exit, from the pidfd: readable once the process has ended.
    let r = running.clone();
    stop.hold(host.readable(
        pidfd,
        Box::new(move |_| {
            let status = r.child.borrow_mut().as_mut().and_then(|c| c.try_wait().ok().flatten());
            let Some(status) = status else { return true };
            r.drain();
            r.child.borrow_mut().take();
            r.stdin.borrow_mut().take();
            let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            r.exit.set(Some(code));
            if let Some(f) = &on_exit {
                call(f, "g.process on_exit", code, r.owner);
            }
            false
        }),
    ));

    // Stopping, or the next load, ends the process and reaps it.
    let r = running.clone();
    *stop.also.borrow_mut() = Some(Box::new(move || {
        if let Some(mut child) = r.child.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }));
    Ok(Process { running, stop })
}

// ---- g.file ----------------------------------------------------------------

pub struct Watched {
    path: PathBuf,
    text: Signal<Option<String>>,
    stop: Rc<Stop>,
}

impl UserData for Watched {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // f.text: the content, nil while the file does not exist; a binding
        // that reads it follows the file.
        fields.add_field_method_get("text", |_, this| Ok(this.text.try_get().flatten()));
        fields.add_field_method_get("path", |_, this| Ok(this.path.display().to_string()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // f:write(text, { mode = "600" }): atomically, through a file beside
        // it and a rename, so a reader never sees half of it. The file
        // keeps the permissions of the one it replaces (a private file
        // stays private); `mode`, octal, sets them.
        methods.add_method("write", |_, this, (text, options): (String, Option<Table>)| {
            let mode = match options {
                Some(t) => match t.get::<Option<String>>("mode")? {
                    Some(m) => Some(
                        u32::from_str_radix(&m, 8)
                            .ok()
                            .filter(|m| *m <= 0o7777)
                            .ok_or_else(|| err(format!("g.file write: mode {m:?} is not octal permissions")))?,
                    ),
                    None => None,
                },
                None => None,
            };
            write_atomically(&this.path, &text, mode).map_err(|e| err(format!("g.file {}: {e}", this.path.display())))?;
            this.text.set(Some(text));
            Ok(())
        });
        // f:read(): read again now, and the content: /proc and /sys say
        // nothing when they change, so a sampler asks.
        methods.add_method("read", |_, this, ()| {
            let text = std::fs::read_to_string(&this.path).ok();
            this.text.set(text.clone());
            Ok(text)
        });
        methods.add_method("stop", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

fn write_atomically(path: &Path, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = path.with_file_name(format!(".{name}.genoma-{}", std::process::id()));
    // The mode asked for, or the one of the file being replaced; a new
    // file without either takes the usual one.
    let kept = mode.or_else(|| std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    // Created with no access for others from the start, so a private
    // file's content is never readable, not even for a moment.
    if let Some(m) = kept {
        options.mode(m & 0o700);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(text.as_bytes())?;
    if let Some(m) = kept {
        file.set_permissions(std::fs::Permissions::from_mode(m))?;
    }
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

/// `g.file { path = "~/.config/bioma/override.json" }`: read now, and again
/// whenever the file is written, replaced or removed (`watch = false` reads
/// once).
/// What a file's directory is watched for: written, moved in or out, made,
/// removed.
const FILE_EVENTS: inotify::WatchFlags = inotify::WatchFlags::CLOSE_WRITE
    .union(inotify::WatchFlags::MOVED_TO)
    .union(inotify::WatchFlags::MOVED_FROM)
    .union(inotify::WatchFlags::CREATE)
    .union(inotify::WatchFlags::DELETE);

/// `dir` watched for `events`; or, while it does not exist yet, the nearest
/// directory above it that does, for what is made in it. Called again on
/// each event, it goes down a level as each one is made, until the watch is
/// on `dir` itself: the watch it has, and true once it is on `dir`. A file
/// whose folder appears only later (matugen's output on a first start,
/// `~/.config/bioma/generated/`) is seen from then on, not never. Several
/// watches on one directory are one, its events those of all of them.
fn watch_toward(fd: BorrowedFd<'_>, dir: &Path, events: inotify::WatchFlags) -> (Option<i32>, bool) {
    let mut at = dir;
    while !at.is_dir() {
        match at.parent() {
            Some(up) => at = up,
            None => return (None, false),
        }
    }
    let own = at == dir;
    let wanted = if own { events } else { inotify::WatchFlags::CREATE | inotify::WatchFlags::MOVED_TO };
    match inotify::add_watch(fd, at, wanted | inotify::WatchFlags::MASK_ADD) {
        Ok(wd) => (Some(wd), own),
        Err(_) => (None, false),
    }
}

/// What a watched directory tells: the names of what changed in it, and
/// whether the watch has only now reached it (then everything may have).
type Changed = Box<dyn FnMut(&[Vec<u8>], bool)>;

/// One directory someone watches.
struct Watch {
    id: u64,
    dir: PathBuf,
    wd: Option<i32>,
    armed: bool,
    /// Taken out while it is called: what it sets may watch or stop
    /// watching something else.
    changed: Option<Changed>,
}

/// Every `g.file` and `g.folder` watched through one inotify instance: an
/// instance each was a descriptor each, and a user has few to give (128 on
/// many systems, shared with every other program).
struct Watches {
    fd: OwnedFd,
    host: HostRef,
    token: Option<RegistrationToken>,
    next: u64,
    watches: Vec<Watch>,
}

thread_local! {
    static WATCHES: RefCell<Option<Watches>> = const { RefCell::new(None) };
}

impl Watches {
    /// A watch nobody has any more is given back.
    fn release(&self, wd: Option<i32>) {
        if let Some(wd) = wd
            && !self.watches.iter().any(|w| w.wd == Some(wd))
        {
            let _ = inotify::remove_watch(&self.fd, wd);
        }
    }
}

/// `dir` watched until `stop` stops: `changed` hears of what happens in it.
fn watch_dir(host: &HostRef, stop: &Stop, dir: PathBuf, what: &str, changed: Changed) {
    let id = WATCHES.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_none() {
            let made = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC)
                .map_err(std::io::Error::from)
                .and_then(|fd| Ok((fd.try_clone()?, fd)));
            match made {
                Ok((fd, theirs)) => {
                    let token = host.readable(theirs, Box::new(watches_ready));
                    *w = Some(Watches { fd, host: host.clone(), token, next: 0, watches: Vec::new() });
                }
                Err(e) => {
                    log::warn!("{what} {}: no watch: {e}", dir.display());
                    return None;
                }
            }
        }
        let w = w.as_mut()?;
        let (wd, armed) = watch_toward(w.fd.as_fd(), &dir, FILE_EVENTS);
        w.next += 1;
        w.watches.push(Watch { id: w.next, dir, wd, armed, changed: Some(changed) });
        Some(w.next)
    });
    let Some(id) = id else { return };
    *stop.also.borrow_mut() = Some(Box::new(move || {
        WATCHES.with(|w| {
            let mut w = w.borrow_mut();
            let Some(all) = w.as_mut() else { return };
            let Some(at) = all.watches.iter().position(|x| x.id == id) else { return };
            let gone = all.watches.remove(at);
            all.release(gone.wd);
            // The last one: the instance goes with it.
            if all.watches.is_empty()
                && let Some(all) = w.take()
                && let Some(token) = all.token
            {
                all.host.remove(token);
            }
        });
    }));
}

/// The instance has events: each watch on a directory named in them hears
/// of what changed there.
fn watches_ready(fd: BorrowedFd<'_>) -> bool {
    let mut seen: Vec<(i32, Option<Vec<u8>>, bool)> = Vec::new();
    let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
    let mut events = inotify::Reader::new(fd, &mut buf);
    while let Ok(event) = events.next() {
        let gone = event.events().contains(inotify::ReadFlags::IGNORED);
        seen.push((event.wd(), event.file_name().map(|n| n.to_bytes().to_vec()), gone));
    }
    let ids: Vec<u64> = WATCHES.with(|w| {
        w.borrow().as_ref().map_or_else(Vec::new, |all| {
            all.watches.iter().filter(|x| seen.iter().any(|(wd, ..)| x.wd == Some(*wd))).map(|x| x.id).collect()
        })
    });
    for id in ids {
        let taken = WATCHES.with(|w| {
            let mut w = w.borrow_mut();
            let all = w.as_mut()?;
            let at = all.watches.iter().position(|x| x.id == id)?;
            let wd = all.watches[at].wd;
            let names: Vec<Vec<u8>> = seen.iter().filter(|(w, ..)| Some(*w) == wd).filter_map(|(_, n, _)| n.clone()).collect();
            // Its directory removed, the watch went with it: toward it again.
            if seen.iter().any(|(w, _, gone)| Some(*w) == wd && *gone) {
                all.watches[at].wd = None;
                all.watches[at].armed = false;
            }
            // Something made on the way to its folder: a level further.
            let mut came = false;
            if !all.watches[at].armed {
                let before = all.watches[at].wd;
                let (wd, armed) = watch_toward(all.fd.as_fd(), &all.watches[at].dir, FILE_EVENTS);
                all.watches[at].wd = wd;
                all.watches[at].armed = armed;
                if before != wd {
                    all.release(before);
                }
                came = true;
            }
            all.watches[at].changed.take().map(|c| (c, names, came))
        });
        let Some((mut changed, names, came)) = taken else { continue };
        changed(&names, came);
        WATCHES.with(|w| {
            if let Some(all) = w.borrow_mut().as_mut()
                && let Some(x) = all.watches.iter_mut().find(|x| x.id == id)
            {
                x.changed = Some(changed);
            }
        });
    }
    true
}

pub fn file(host: &HostRef, t: Table) -> mlua::Result<Watched> {
    let path = expand(&t.get::<String>("path")?);
    let read = {
        let path = path.clone();
        move || std::fs::read_to_string(&path).ok()
    };
    let text = Signal::new(read());
    let stop = Stop::new(host);
    if t.get::<Option<bool>>("watch")?.unwrap_or(true) {
        // The directory, not the file: an editor's or Bioma's atomic save
        // replaces the file, and a watch on the old one would see nothing.
        let dir = path.parent().unwrap_or(Path::new("/")).to_owned();
        let name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
        let text = text.clone();
        watch_dir(
            host,
            &stop,
            dir,
            "g.file",
            Box::new(move |names, came| {
                // Something made on the way to its folder: read, as the
                // file may have come with it.
                if came || names.iter().any(|n| n.as_slice() == name.as_encoded_bytes()) {
                    text.set(read());
                }
            }),
        );
    }
    Ok(Watched { path, text, stop })
}

// ---- g.folder --------------------------------------------------------------

/// One entry of a folder: its name, and whether it is a folder itself;
/// its size and when it last changed (seconds since 1970) when asked for
/// with `details`, else 0.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    name: String,
    dir: bool,
    size: u64,
    modified: f64,
}

/// What a folder's listing takes in.
#[derive(Clone, Copy)]
struct Listing {
    hidden: bool,
    details: bool,
}

pub struct Folder {
    path: PathBuf,
    entries: Signal<Vec<Entry>>,
    stop: Rc<Stop>,
    listing: Listing,
}

impl UserData for Folder {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // d.entries: { { name, path, dir } }, by name, hidden ones left out;
        // empty while the folder does not exist. A binding that reads it
        // follows the folder.
        fields.add_field_method_get("entries", |lua, this| {
            let list = lua.create_table()?;
            for (i, e) in this.entries.try_get().unwrap_or_default().iter().enumerate() {
                let t = lua.create_table()?;
                t.set("name", e.name.as_str())?;
                t.set("path", this.path.join(&e.name).display().to_string())?;
                t.set("dir", e.dir)?;
                if this.listing.details {
                    t.set("size", e.size)?;
                    t.set("modified", e.modified)?;
                }
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        fields.add_field_method_get("path", |_, this| Ok(this.path.display().to_string()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("stop", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

fn list_folder(path: &Path, listing: Listing) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(path) else { return Vec::new() };
    let mut out: Vec<Entry> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !listing.hidden {
                return None;
            }
            // Through a link, as a file manager shows it.
            let meta = std::fs::metadata(e.path()).ok();
            let dir = meta.as_ref().is_some_and(|m| m.is_dir());
            let (size, modified) = match (&meta, listing.details) {
                (Some(m), true) => (
                    m.len(),
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0.0, |d| d.as_secs_f64()),
                ),
                _ => (0, 0.0),
            };
            Some(Entry { name, dir, size, modified })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `g.folder { path = "~/Pictures/Wallpapers" }`: what is in a folder, read
/// now and again whenever an entry comes, goes or is renamed (`watch =
/// false` reads once). Not recursive. `hidden = true` keeps the entries
/// whose names begin with a dot; `details = true` gives each its `size`
/// and `modified` time, and then a file written in place changes the list.
pub fn folder(host: &HostRef, t: Table) -> mlua::Result<Folder> {
    let path = expand(&t.get::<String>("path")?);
    let listing = Listing {
        hidden: t.get::<Option<bool>>("hidden")?.unwrap_or(false),
        details: t.get::<Option<bool>>("details")?.unwrap_or(false),
    };
    let entries = Signal::new(list_folder(&path, listing));
    let stop = Stop::new(host);
    if t.get::<Option<bool>>("watch")?.unwrap_or(true) {
        let (dir, entries) = (path.clone(), entries.clone());
        // A folder not there yet is waited for, as a file's is.
        watch_dir(
            host,
            &stop,
            path.clone(),
            "g.folder",
            Box::new(move |_, _| {
                // Set only when the list differs: a file written in place
                // is a change of content, not of the folder.
                let now = list_folder(&dir, listing);
                if entries.try_get().as_ref() != Some(&now) {
                    entries.set(now);
                }
            }),
        );
    }
    Ok(Folder { path, entries, stop, listing })
}

// ---- g.socket --------------------------------------------------------------

/// A local or a network connection, read and written alike.
enum Conn {
    Unix(UnixStream),
    Tcp(std::net::TcpStream),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Unix(s) => s.read(buf),
            Conn::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Unix(s) => s.write(buf),
            Conn::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Unix(s) => s.flush(),
            Conn::Tcp(s) => s.flush(),
        }
    }
}

pub struct Socket {
    stream: Rc<RefCell<Option<Conn>>>,
    stop: Rc<Stop>,
}

impl UserData for Socket {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("connected", |_, this| Ok(this.stream.borrow().is_some()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // s:send("text\n"): as given; a line-delimited peer wants the newline.
        methods.add_method("send", |_, this, text: LuaString| {
            let mut stream = this.stream.borrow_mut();
            let Some(stream) = stream.as_mut() else { return Err(err("g.socket: not connected")) };
            stream.write_all(&text.as_bytes()).map_err(|e| err(format!("g.socket send: {e}")))
        });
        methods.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

/// `g.socket { path = "$XDG_RUNTIME_DIR/…", on_line, on_close }`: a Unix
/// socket, its input as lines. `g.socket { host = "127.0.0.1", port =
/// 4455, on_open, on_line, on_close }`: the same over TCP, connected off
/// the loop; `on_open()` once it is, `on_close(error)` when it ends (with
/// why, if it never opened or broke).
pub fn socket(host: &HostRef, t: Table) -> mlua::Result<Socket> {
    let on_line: Option<Function> = t.get("on_line")?;
    let on_close: Option<Function> = t.get("on_close")?;
    let on_open: Option<Function> = t.get("on_open")?;
    let stream: Rc<RefCell<Option<Conn>>> = Rc::new(RefCell::new(None));
    let stop = Stop::new(host);
    let s = stream.clone();
    *stop.also.borrow_mut() = Some(Box::new(move || {
        s.borrow_mut().take();
    }));
    if let Some(path) = t.get::<Option<String>>("path")? {
        let path = expand(&path);
        let unix = UnixStream::connect(&path).map_err(|e| err(format!("g.socket {}: {e}", path.display())))?;
        let watch = OwnedFd::from(unix.try_clone().map_err(|e| err(format!("g.socket: {e}")))?);
        *stream.borrow_mut() = Some(Conn::Unix(unix));
        listen(host, &stop, &stream, watch, on_line, on_close);
        return Ok(Socket { stream, stop });
    }
    let name: String = t.get::<Option<String>>("host")?.ok_or_else(|| err("g.socket: a path, or a host and a port"))?;
    let port: u16 = t.get::<Option<u16>>("port")?.ok_or_else(|| err("g.socket: a host needs a port"))?;
    let address = format!("{name}:{port}");
    let (h, st, s) = (host.clone(), stop.clone(), stream.clone());
    let owner = stop.owner;
    off_loop(host, &stop, "socket", move || connect_tcp(&address), move |result| match result {
        Ok(tcp) => {
            let watch = match tcp.set_nonblocking(true).and_then(|()| tcp.try_clone()) {
                Ok(c) => OwnedFd::from(c),
                Err(e) => {
                    if let Some(f) = &on_close {
                        call(f, "g.socket on_close", format!("g.socket: {e}"), owner);
                    }
                    return;
                }
            };
            *s.borrow_mut() = Some(Conn::Tcp(tcp));
            if let Some(f) = &on_open {
                call(f, "g.socket on_open", (), owner);
            }
            listen(&h, &st, &s, watch, on_line, on_close);
        }
        Err(e) => {
            if let Some(f) = &on_close {
                call(f, "g.socket on_close", e, owner);
            }
        }
    })?;
    Ok(Socket { stream, stop })
}

/// How long a connection, or a handshake on it, may take.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// "host:port" looked up and connected, each address in turn, on a thread.
pub(crate) fn connect_tcp(address: &str) -> Result<std::net::TcpStream, String> {
    use std::net::ToSocketAddrs;
    let addresses = address.to_socket_addrs().map_err(|e| format!("{address}: {e}"))?;
    let mut last = format!("{address}: no address");
    for a in addresses {
        match std::net::TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(tcp) => {
                let _ = tcp.set_nodelay(true);
                return Ok(tcp);
            }
            Err(e) => last = format!("{address}: {e}"),
        }
    }
    Err(last)
}

/// A connected stream's input, read whenever it is readable, as lines.
fn listen(host: &HostRef, stop: &Stop, stream: &Rc<RefCell<Option<Conn>>>, watch: OwnedFd, on_line: Option<Function>, on_close: Option<Function>) {
    let owner = stop.owner;
    let s = stream.clone();
    let mut lines = Lines::default();
    stop.hold(host.readable(
        watch,
        Box::new(move |_| {
            let mut buf = [0u8; 65536];
            let read = match s.borrow_mut().as_mut() {
                Some(stream) => stream.read(&mut buf),
                None => return false,
            };
            let mut each = |line: &str| {
                if let Some(f) = &on_line {
                    call(f, "g.socket on_line", line.to_owned(), owner);
                }
            };
            match read {
                Ok(n) if n > 0 => {
                    lines.feed(&buf[..n], &mut each);
                    true
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => true,
                ended => {
                    lines.finish(&mut each);
                    s.borrow_mut().take();
                    if let Some(f) = &on_close {
                        let why = match ended {
                            Err(e) => Some(format!("g.socket: {e}")),
                            _ => None,
                        };
                        call(f, "g.socket on_close", why, owner);
                    }
                    false
                }
            }
        }),
    ));
}

#[cfg(test)]
mod watch_tests {
    use super::*;

    #[test]
    fn a_write_keeps_the_permissions_it_replaces_or_takes_those_asked() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("genoma-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("secret.json");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        write_atomically(&path, "{}", Some(0o600)).unwrap();
        assert_eq!(mode(&path), 0o600, "asked for");
        write_atomically(&path, "{\"a\":1}", None).unwrap();
        assert_eq!(mode(&path), 0o600, "kept when rewritten");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        write_atomically(&path, "{}", Some(0o644)).unwrap();
        assert_eq!(mode(&path), 0o644, "changed when asked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_watch_goes_down_toward_a_folder_made_later() {
        let base = std::env::temp_dir().join(format!("genoma-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join("bioma").join("generated");
        let fd = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC).unwrap();
        assert!(!watch_toward(fd.as_fd(), &dir, FILE_EVENTS).1, "two levels missing: the base is watched");
        std::fs::create_dir(base.join("bioma")).unwrap();
        assert!(!watch_toward(fd.as_fd(), &dir, FILE_EVENTS).1, "one level missing: a level down");
        std::fs::create_dir(&dir).unwrap();
        assert!(watch_toward(fd.as_fd(), &dir, FILE_EVENTS).1, "there: its own watch");
        let _ = std::fs::remove_dir_all(&base);
    }
}
