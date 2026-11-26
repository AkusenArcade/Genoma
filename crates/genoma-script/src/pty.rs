//! `g.pty`: a program run on a pseudo-terminal, as a terminal emulator runs a
//! shell. The program sees a terminal of a size (and is told when it
//! changes), owns it as its controlling terminal, and is read without the
//! loop ever waiting on it.
//!
//! What it writes goes to `on_data` as bytes; a terminal emulator in Rust can
//! take the stream instead ([`Pty::take_output`]), so the bytes never cross
//! into Luau. What is written to it and cannot go at once (a long paste into
//! a program that is not reading) waits, in order, and is tried again.

use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{ffi::OsStrExt, process::CommandExt, process::ExitStatusExt},
    },
    process::{Child, Command, Stdio},
    rc::Rc,
    time::Duration,
};

use genoma_core::reactive::current_owner;
use mlua::{Function, LuaString, Table, UserData, UserDataFields, UserDataMethods};
use rustix::termios::Winsize;

use crate::io::{HostRef, Stop, call, expand};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// How long a write that could not all go waits before it is tried again.
const RETRY: Duration = Duration::from_millis(4);

/// Who reads what the program writes: Luau, or a sink in Rust.
pub(crate) type Sink = Box<dyn FnMut(&[u8])>;

pub(crate) struct Inner {
    pub(crate) host: HostRef,
    master: OwnedFd,
    child: RefCell<Option<Child>>,
    pid: i32,
    exit: Cell<Option<i32>>,
    /// Written, not yet taken by the terminal.
    pending: RefCell<Vec<u8>>,
    retrying: Cell<bool>,
    sink: RefCell<Option<Sink>>,
    on_data: Option<Function>,
    lua: mlua::WeakLua,
    owner: Option<usize>,
}

impl Inner {
    fn deliver(&self, data: &[u8]) {
        if let Some(sink) = self.sink.borrow_mut().as_mut() {
            sink(data);
            return;
        }
        let Some(f) = &self.on_data else { return };
        let Some(lua) = self.lua.try_upgrade() else { return };
        if let Ok(s) = lua.create_string(data) {
            call(f, "g.pty on_data", s, self.owner);
        }
    }

    /// One read: false once the other side is gone (EIO when the last
    /// process holding the terminal has closed it).
    fn read_once(&self) -> bool {
        let mut buf = vec![0u8; 1 << 16];
        match rustix::io::read(&self.master, &mut buf) {
            Ok(0) => false,
            Ok(n) => {
                self.deliver(&buf[..n]);
                true
            }
            Err(e) if e == rustix::io::Errno::AGAIN || e == rustix::io::Errno::INTR => true,
            Err(_) => false,
        }
    }

    /// Everything still to be read, now: the exit is told after the last
    /// of what was written before it.
    fn drain(&self) {
        for _ in 0..256 {
            let mut buf = vec![0u8; 1 << 16];
            match rustix::io::read(&self.master, &mut buf) {
                Ok(n) if n > 0 => self.deliver(&buf[..n]),
                _ => break,
            }
        }
    }

    /// As much of what waits as the terminal takes now; the rest later.
    fn flush(self: &Rc<Self>) {
        loop {
            let mut pending = self.pending.borrow_mut();
            if pending.is_empty() {
                return;
            }
            match rustix::io::write(&self.master, &pending) {
                Ok(n) => {
                    pending.drain(..n);
                }
                Err(e) if e == rustix::io::Errno::INTR => {}
                Err(e) if e == rustix::io::Errno::AGAIN => break,
                // The program is gone: nobody to write to.
                Err(_) => {
                    pending.clear();
                    return;
                }
            }
        }
        if self.retrying.replace(true) {
            return;
        }
        let me = Rc::downgrade(self);
        // A timer of its own, not the load's: it ends when the writes do.
        self.host.timer(
            RETRY,
            Box::new(move || {
                let me = me.upgrade()?;
                me.retrying.set(false);
                me.flush();
                None
            }),
        );
    }

    fn signal(&self, signal: rustix::process::Signal) {
        if self.exit.get().is_some() {
            return;
        }
        if let Some(pid) = rustix::process::Pid::from_raw(self.pid) {
            let _ = rustix::process::kill_process(pid, signal);
        }
    }

    /// Bytes for the program, in order; what cannot go now waits.
    pub(crate) fn write(self: &Rc<Self>, data: &[u8]) {
        if self.exit.get().is_some() {
            return;
        }
        self.pending.borrow_mut().extend_from_slice(data);
        self.flush();
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16, width: u16, height: u16) {
        let _ = rustix::termios::tcsetwinsize(
            &self.master,
            Winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: width, ws_ypixel: height },
        );
    }

    /// The process the terminal is given to now: the shell, or what it runs.
    fn foreground(&self) -> Option<i32> {
        rustix::termios::tcgetpgrp(&self.master).ok().map(|p| p.as_raw_nonzero().get())
    }
}

pub struct Pty {
    pub(crate) inner: Rc<Inner>,
    stop: Rc<Stop>,
}

impl Pty {
    /// What the program writes goes to `sink` from now on, not to `on_data`.
    pub(crate) fn take_output(&self, sink: Sink) {
        *self.inner.sink.borrow_mut() = Some(sink);
    }
}

fn winsize(t: &Table) -> mlua::Result<Winsize> {
    let cols: u16 = t.get::<Option<u16>>("cols")?.unwrap_or(80).max(1);
    let rows: u16 = t.get::<Option<u16>>("rows")?.unwrap_or(24).max(1);
    let width: u16 = t.get::<Option<u16>>("width")?.unwrap_or(0);
    let height: u16 = t.get::<Option<u16>>("height")?.unwrap_or(0);
    Ok(Winsize { ws_row: rows, ws_col: cols, ws_xpixel: width, ws_ypixel: height })
}

fn read_link(path: String) -> Option<String> {
    std::fs::read_link(path).ok().map(|p| p.display().to_string())
}

fn name_of(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim_end().to_owned())
}

impl UserData for Pty {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("pid", |_, this| Ok(this.inner.pid));
        fields.add_field_method_get("running", |_, this| Ok(this.inner.exit.get().is_none()));
        fields.add_field_method_get("exit_code", |_, this| Ok(this.inner.exit.get()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // p:write(bytes): as typed or pasted, in order.
        methods.add_method("write", |_, this, data: LuaString| {
            this.inner.write(&data.as_bytes());
            Ok(())
        });
        // p:resize { cols, rows, width, height }: the program is told
        // (SIGWINCH) by the kernel.
        methods.add_method("resize", |_, this, t: Table| {
            rustix::termios::tcsetwinsize(&this.inner.master, winsize(&t)?)
                .map_err(|e| err(format!("g.pty resize: {e}")))
        });
        // p:foreground(): the program the terminal is given to now, its pid
        // and name ("fish", "vim", "claude"): what a tab is called by.
        methods.add_method("foreground", |lua, this, ()| {
            let Some(pid) = this.inner.foreground() else { return Ok(mlua::Value::Nil) };
            let t = lua.create_table()?;
            t.set("pid", pid)?;
            t.set("name", name_of(pid))?;
            Ok(mlua::Value::Table(t))
        });
        // p:cwd(): the folder of the program in the foreground, else the
        // shell's; for a terminal whose shell says nothing (no OSC 7).
        methods.add_method("cwd", |_, this, ()| {
            let pid = this.inner.foreground().unwrap_or(this.inner.pid);
            Ok(read_link(format!("/proc/{pid}/cwd")).or_else(|| read_link(format!("/proc/{}/cwd", this.inner.pid))))
        });
        // p:signal(2): to the program in the foreground, as the terminal
        // would send it.
        methods.add_method("signal", |_, this, number: i32| {
            let Some(signal) = rustix::process::Signal::from_named_raw(number) else {
                return Err(err(format!("g.pty signal: no signal {number}")));
            };
            match this.inner.foreground().and_then(rustix::process::Pid::from_raw) {
                Some(group) => {
                    let _ = rustix::process::kill_process_group(group, signal);
                }
                None => this.inner.signal(signal),
            }
            Ok(())
        });
        // p:stop(): the terminal is hung up (SIGHUP, as closing a terminal
        // window does); on_exit still comes.
        methods.add_method("stop", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

/// `g.pty { command, cwd, env, cols, rows, width, height, on_data, on_exit }`.
pub fn open(host: &HostRef, lua: &mlua::Lua, t: Table) -> mlua::Result<Pty> {
    let command: Vec<String> = match t.get::<Option<Vec<String>>>("command")? {
        Some(c) if !c.is_empty() => c,
        Some(_) => return Err(err("g.pty: command is empty")),
        None => vec![std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())],
    };
    let size = winsize(&t)?;

    use rustix::fs::OFlags;
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)
        .map_err(|e| err(format!("g.pty: no terminal: {e}")))?;
    grantpt(&master).and_then(|()| unlockpt(&master)).map_err(|e| err(format!("g.pty: {e}")))?;
    let name = ptsname(&master, Vec::new()).map_err(|e| err(format!("g.pty: {e}")))?;
    let slave: OwnedFd = rustix::fs::open(name.as_c_str(), OFlags::RDWR | OFlags::NOCTTY, rustix::fs::Mode::empty())
        .map_err(|e| err(format!("g.pty {}: {e}", name.to_string_lossy())))?;
    rustix::termios::tcsetwinsize(&slave, size).map_err(|e| err(format!("g.pty: {e}")))?;

    let program = &command[0];
    let mut line = Command::new(OsStr::from_bytes(program.as_bytes()));
    line.args(&command[1..]);
    let dup = |fd: &OwnedFd| fd.try_clone().map(Stdio::from).map_err(|e| err(format!("g.pty: {e}")));
    line.stdin(dup(&slave)?).stdout(dup(&slave)?).stderr(dup(&slave)?);
    if let Some(cwd) = t.get::<Option<String>>("cwd")? {
        line.current_dir(expand(&cwd));
    }
    // A terminal's environment, and none of Genoma's own: a `genoma ipc`
    // typed in it means the shell's interface, not this window's.
    line.env("TERM", "xterm-256color").env("COLORTERM", "truecolor").env_remove("GENOMA_IPC");
    for key in ["WINDOWID", "LINES", "COLUMNS"] {
        line.env_remove(key);
    }
    if let Some(env) = t.get::<Option<Table>>("env")? {
        for pair in env.pairs::<String, String>() {
            let (key, value) = pair?;
            line.env(key, value);
        }
    }
    // The child leads a session of its own whose controlling terminal this
    // is: ^C reaches what runs in it, and a closed window hangs it up.
    // Safety: system calls only, async-signal-safe, between fork and exec.
    unsafe {
        line.pre_exec(|| {
            rustix::process::setsid().map_err(std::io::Error::from)?;
            rustix::process::ioctl_tiocsctty(std::io::stdin().as_fd()).map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    let child = line.spawn().map_err(|e| err(format!("g.pty {program}: {e}")))?;
    // Ours closed: the last process holding it closes the terminal.
    drop(slave);
    let pid = child.id() as i32;
    rustix::fs::fcntl_setfl(&master, OFlags::NONBLOCK).map_err(|e| err(format!("g.pty: {e}")))?;
    let pidfd = rustix::process::Pid::from_raw(pid)
        .ok_or_else(|| err("g.pty: no pid"))
        .and_then(|p| {
            rustix::process::pidfd_open(p, rustix::process::PidfdFlags::empty()).map_err(|e| err(format!("g.pty pidfd: {e}")))
        })?;

    let on_exit: Option<Function> = t.get("on_exit")?;
    let inner = Rc::new(Inner {
        host: host.clone(),
        master,
        child: RefCell::new(Some(child)),
        pid,
        exit: Cell::new(None),
        pending: RefCell::default(),
        retrying: Cell::new(false),
        sink: RefCell::default(),
        on_data: t.get("on_data")?,
        lua: lua.weak(),
        owner: current_owner(),
    });
    let stop = Stop::new(host);

    let watch = inner.master.try_clone().map_err(|e| err(format!("g.pty: {e}")))?;
    let me = inner.clone();
    stop.hold(host.readable(watch, Box::new(move |_| me.read_once())));

    let me = inner.clone();
    stop.hold(host.readable(
        pidfd,
        Box::new(move |_| {
            let status = me.child.borrow_mut().as_mut().and_then(|c| c.try_wait().ok().flatten());
            let Some(status) = status else { return true };
            me.drain();
            me.child.borrow_mut().take();
            let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            me.exit.set(Some(code));
            me.pending.borrow_mut().clear();
            if let Some(f) = &on_exit {
                call(f, "g.pty on_exit", code, me.owner);
            }
            false
        }),
    ));

    // Stopped, or the next load: hung up as a closed terminal is, and reaped
    // once it has gone.
    let me = inner.clone();
    *stop.also.borrow_mut() = Some(Box::new(move || {
        if let Some(mut child) = me.child.borrow_mut().take() {
            if let Some(group) = rustix::process::Pid::from_raw(me.pid) {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::HUP);
            }
            // A shell that ignores the hang-up is ended.
            if !matches!(child.try_wait(), Ok(Some(_))) {
                std::thread::sleep(Duration::from_millis(20));
                if !matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                }
            }
            let _ = child.wait();
        }
    }));
    Ok(Pty { inner, stop })
}
