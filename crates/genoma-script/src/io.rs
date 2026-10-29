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
        fd::{BorrowedFd, OwnedFd},
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
use genoma_core::reactive::{Signal, on_cleanup};
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
}

pub type HostRef = Rc<dyn Host>;

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

fn call(f: &Function, what: &str, args: impl IntoLuaMulti) {
    // A callback of a load that has been replaced: its state is gone. What
    // a g.watch handler starts is detached, so its timers outlive the load
    // they belong to, and fire into nothing.
    if f.weak_lua().try_upgrade().is_none() {
        return;
    }
    if let Err(e) = f.call::<()>(args) {
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
}

impl Stop {
    pub(crate) fn new(host: &HostRef) -> Rc<Stop> {
        let stop = Rc::new(Stop { host: host.clone(), tokens: RefCell::default(), also: RefCell::default() });
        // Owned by the load: the next one stops it.
        let s = stop.clone();
        on_cleanup(move || s.stop());
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
    stop.hold(host.timer(
        every,
        Box::new(move || {
            call(&on_tick, "g.timer", ());
            Some(every)
        }),
    ));
    Ok(Timer(stop))
}

/// `g.after(seconds, function() … end)`: once, later; `:stop()` before it
/// fires to keep it from firing.
pub fn after(host: &HostRef, (seconds, f): (f64, Function)) -> mlua::Result<Timer> {
    let stop = Stop::new(host);
    let mut f = Some(f);
    stop.hold(host.timer(
        Duration::from_secs_f64(seconds.max(0.0)),
        Box::new(move || {
            if let Some(f) = f.take() {
                call(&f, "g.after", ());
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
}

impl Running {
    fn out(&self) -> impl FnMut(&str) + '_ {
        move |line| match &self.on_line {
            Some(f) => call(f, "g.process on_line", line.to_owned()),
            None => {}
        }
    }

    fn err(&self) -> impl FnMut(&str) + '_ {
        move |line| match &self.on_error_line {
            Some(f) => call(f, "g.process on_error_line", line.to_owned()),
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
/// can read (`ps`) travels there.
pub fn process(host: &HostRef, t: Table) -> mlua::Result<Process> {
    let command: Vec<String> = t.get("command")?;
    let Some(program) = command.first() else { return Err(err("g.process: command is empty")) };
    let mut command_line = Command::new(program);
    command_line.args(&command[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
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
                call(f, "g.process on_exit", code);
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
        // f:write(text): atomically, through a file beside it and a rename,
        // so a reader never sees half of it.
        methods.add_method("write", |_, this, text: String| {
            write_atomically(&this.path, &text).map_err(|e| err(format!("g.file {}: {e}", this.path.display())))?;
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

fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = path.with_file_name(format!(".{name}.genoma-{}", std::process::id()));
    let mut file = File::create(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

/// `g.file { path = "~/.config/bioma/override.json" }`: read now, and again
/// whenever the file is written, replaced or removed (`watch = false` reads
/// once).
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
        let watch = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC).and_then(|fd| {
            inotify::add_watch(
                &fd,
                &dir,
                inotify::WatchFlags::CLOSE_WRITE
                    | inotify::WatchFlags::MOVED_TO
                    | inotify::WatchFlags::MOVED_FROM
                    | inotify::WatchFlags::CREATE
                    | inotify::WatchFlags::DELETE,
            )?;
            Ok(fd)
        });
        match watch {
            Ok(fd) => stop.hold(host.readable(
                fd,
                Box::new(move |fd| {
                    let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
                    let mut events = inotify::Reader::new(fd, &mut buf);
                    let mut ours = false;
                    while let Ok(event) = events.next() {
                        ours |= event.file_name().is_some_and(|n| n.to_bytes() == name.as_encoded_bytes());
                    }
                    if ours {
                        text.set(read());
                    }
                    true
                }),
            )),
            Err(e) => log::warn!("g.file {}: no watch on {}: {e}", path.display(), dir.display()),
        }
    }
    Ok(Watched { path, text, stop })
}

// ---- g.socket --------------------------------------------------------------

pub struct Socket {
    stream: Rc<RefCell<Option<UnixStream>>>,
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
            let Some(stream) = stream.as_mut() else { return Err(err("g.socket: closed")) };
            stream.write_all(&text.as_bytes()).map_err(|e| err(format!("g.socket send: {e}")))
        });
        methods.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

/// `g.socket { path = "$XDG_RUNTIME_DIR/…", on_line = …, on_close = … }`: a
/// Unix socket, its input as lines.
pub fn socket(host: &HostRef, t: Table) -> mlua::Result<Socket> {
    let path = expand(&t.get::<String>("path")?);
    let stream = UnixStream::connect(&path).map_err(|e| err(format!("g.socket {}: {e}", path.display())))?;
    let watch = OwnedFd::from(stream.try_clone().map_err(|e| err(format!("g.socket: {e}")))?);
    let on_line: Option<Function> = t.get("on_line")?;
    let on_close: Option<Function> = t.get("on_close")?;
    let stream = Rc::new(RefCell::new(Some(stream)));
    let stop = Stop::new(host);
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
                    call(f, "g.socket on_line", line.to_owned());
                }
            };
            match read {
                Ok(n) if n > 0 => {
                    lines.feed(&buf[..n], &mut each);
                    true
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => true,
                _ => {
                    lines.finish(&mut each);
                    s.borrow_mut().take();
                    if let Some(f) = &on_close {
                        call(f, "g.socket on_close", ());
                    }
                    false
                }
            }
        }),
    ));
    let s = stream.clone();
    *stop.also.borrow_mut() = Some(Box::new(move || {
        s.borrow_mut().take();
    }));
    Ok(Socket { stream, stop })
}
