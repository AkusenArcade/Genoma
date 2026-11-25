//! `@genoma/files` and `@genoma/trash` in Luau.
//!
//! The jobs outlive a reload of the interface: a copy half done is not
//! stopped by a change of colour, and what it asks can be answered by the
//! interface that came after.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use genoma_core::reactive::{Signal, batch, current_owner, detached, untracked};
use genoma_services::{
    files::{self, Answer, Control, Measure, Progress, Question, Seen, State, Task},
    mime, trash,
};
use mlua::{Function, Lua, LuaString, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value as LuaValue};

use crate::{
    err,
    io::{HostRef, Post, Stop, call, lasting_mailbox, mailbox, watch_dir},
    mime::path_of,
};

/// Deep trees are walked by functions that call themselves.
const STACK: usize = 64 << 20;

struct Job {
    id: u64,
    kind: &'static str,
    count: usize,
    to: Option<PathBuf>,
    control: Arc<Control>,
    progress: Progress,
    paused: bool,
    on_done: Option<(Function, Option<usize>)>,
}

/// The jobs of a whole run, made when an interface first asks.
#[derive(Clone, Default)]
pub struct Shared(Rc<RefCell<Option<Rc<Files>>>>);

impl Shared {
    pub fn get(&self, host: &HostRef) -> mlua::Result<Rc<Files>> {
        let mut made = self.0.borrow_mut();
        if let Some(files) = made.as_ref() {
            return Ok(files.clone());
        }
        let files = Files::new(host)?;
        *made = Some(files.clone());
        Ok(files)
    }
}

/// What the threads send.
enum Sent {
    Progress(u64, Progress),
    Ended(u64, Progress),
    /// A name changed or made: where, or why not.
    Named(u64, Result<PathBuf, String>),
    /// What was counted so far, and whether it is all.
    Measured(u64, Measure, bool),
    Told(u64, bool),
    Facts(u64, Option<Box<files::Facts>>),
    /// Those of some paths that something could not be done to.
    Failed(u64, Vec<PathBuf>),
}

pub struct Files {
    jobs: RefCell<Vec<Job>>,
    version: Signal<u64>,
    next: Cell<u64>,
    waiting: RefCell<HashMap<u64, (Function, Option<usize>)>>,
    post: RefCell<Option<Post<Sent>>>,
}

impl Files {
    pub fn new(host: &HostRef) -> mlua::Result<Rc<Files>> {
        let me = Rc::new(Files {
            jobs: RefCell::default(),
            version: detached(|| Signal::new(0)),
            next: Cell::new(1),
            waiting: RefCell::default(),
            post: RefCell::default(),
        });
        let weak = Rc::downgrade(&me);
        let post = lasting_mailbox(host, "files", move |sent| {
            if let Some(me) = weak.upgrade() {
                me.take(sent);
            }
        })?;
        *me.post.borrow_mut() = Some(post);
        Ok(me)
    }

    fn id(&self) -> u64 {
        self.next.replace(self.next.get() + 1)
    }

    fn moved(&self) {
        let v = untracked(|| self.version.try_get()).unwrap_or(0);
        self.version.set(v + 1);
    }

    fn take(&self, sent: Sent) {
        match sent {
            Sent::Progress(id, progress) => {
                if let Some(job) = self.jobs.borrow_mut().iter_mut().find(|j| j.id == id) {
                    job.progress = progress;
                }
                self.moved();
            }
            Sent::Ended(id, progress) => {
                let ended = {
                    let mut jobs = self.jobs.borrow_mut();
                    jobs.iter().position(|j| j.id == id).map(|i| jobs.remove(i))
                };
                batch(|| {
                    self.moved();
                    if let Some((f, owner)) = ended.and_then(|j| j.on_done) {
                        // Its load replaced, nobody hears.
                        if f.weak_lua().try_upgrade().is_none() {
                            return;
                        }
                        let lua = f.weak_lua().upgrade();
                        match result(&lua, &progress) {
                            Ok(t) => call(&f, "files on_done", t, owner),
                            Err(e) => log::error!("files on_done: {e}"),
                        }
                    }
                });
            }
            Sent::Named(id, done) => {
                let Some((f, owner)) = self.waiting.borrow_mut().remove(&id) else { return };
                let Some(lua) = f.weak_lua().try_upgrade() else { return };
                let args: mlua::Result<MultiValue> = match done {
                    Ok(path) => bytes(&lua, &path).map(|p| MultiValue::from_vec(vec![LuaValue::String(p)])),
                    Err(e) => lua.create_string(&e).map(|e| MultiValue::from_vec(vec![LuaValue::Nil, LuaValue::String(e)])),
                };
                match args {
                    Ok(args) => call(&f, "files on_done", args, owner),
                    Err(e) => log::error!("files on_done: {e}"),
                }
            }
            Sent::Facts(id, found) => {
                let Some((f, owner)) = self.waiting.borrow_mut().remove(&id) else { return };
                let Some(lua) = f.weak_lua().try_upgrade() else { return };
                let told = found.map(|f| facts(&lua, &f)).transpose();
                match told {
                    Ok(t) => call(&f, "files on_told", t, owner),
                    Err(e) => log::error!("files on_told: {e}"),
                }
            }
            Sent::Failed(id, list) => {
                let Some((f, owner)) = self.waiting.borrow_mut().remove(&id) else { return };
                let Some(lua) = f.weak_lua().try_upgrade() else { return };
                match paths(&lua, &list) {
                    Ok(t) => call(&f, "files on_done", t, owner),
                    Err(e) => log::error!("files on_done: {e}"),
                }
            }
            Sent::Told(id, yes) => {
                let Some((f, owner)) = self.waiting.borrow_mut().remove(&id) else { return };
                if f.weak_lua().try_upgrade().is_some() {
                    call(&f, "files on_told", yes, owner);
                }
            }
            Sent::Measured(id, m, whole) => {
                let found = match whole {
                    true => self.waiting.borrow_mut().remove(&id),
                    false => self.waiting.borrow().get(&id).cloned(),
                };
                let Some((f, owner)) = found else { return };
                let Some(lua) = f.weak_lua().try_upgrade() else { return };
                match measured(&lua, &m) {
                    Ok(t) => call(&f, "files on_measured", (t, whole), owner),
                    Err(e) => log::error!("files on_measured: {e}"),
                }
            }
        }
    }

    fn start(&self, task: Task, on_done: Option<Function>) -> mlua::Result<u64> {
        let Some(post) = self.post.borrow().clone() else { return Err(err("files: not started")) };
        let id = self.id();
        let control = Arc::new(Control::default());
        let job = Job {
            id,
            kind: task.name(),
            count: task.count(),
            to: task.to().map(Path::to_owned),
            control: control.clone(),
            progress: Progress::default(),
            paused: false,
            on_done: on_done.map(|f| (f, current_owner())),
        };
        std::thread::Builder::new()
            .name("genoma-files".into())
            .stack_size(STACK)
            .spawn(move || {
                let report = |p: &Progress| post.send(Sent::Progress(id, p.clone()));
                let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| files::run_in_turn(&task, &control, &report)));
                // One that broke is one that stopped.
                post.send(Sent::Ended(id, ended.unwrap_or_else(|_| Progress { state: State::Cancelled, ..Default::default() })));
            })
            .map_err(|e| err(format!("files: no thread: {e}")))?;
        self.jobs.borrow_mut().push(job);
        self.moved();
        Ok(id)
    }

    /// What blocks a moment, off the loop.
    fn named(&self, work: impl FnOnce() -> Result<PathBuf, String> + Send + 'static, on_done: Option<Function>) -> mlua::Result<()> {
        let Some(post) = self.post.borrow().clone() else { return Err(err("files: not started")) };
        let id = self.id();
        if let Some(f) = on_done {
            self.waiting.borrow_mut().insert(id, (f, current_owner()));
        }
        std::thread::Builder::new()
            .name("genoma-files".into())
            .spawn(move || post.send(Sent::Named(id, work())))
            .map(|_| ())
            .map_err(|e| err(format!("files: no thread: {e}")))
    }

    fn job(&self, id: u64, f: impl FnOnce(&mut Job)) {
        if let Some(job) = self.jobs.borrow_mut().iter_mut().find(|j| j.id == id) {
            f(job);
        }
    }
}

fn bytes(lua: &Lua, path: &Path) -> mlua::Result<LuaString> {
    lua.create_string(path.as_os_str().as_bytes())
}

fn paths(lua: &Lua, all: &[PathBuf]) -> mlua::Result<Table> {
    let list = lua.create_table()?;
    for p in all {
        list.raw_push(bytes(lua, p)?)?;
    }
    Ok(list)
}

fn result(lua: &Lua, p: &Progress) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("cancelled", p.state == State::Cancelled)?;
    t.set("made", paths(lua, &p.made)?)?;
    t.set("skipped", paths(lua, &p.skipped)?)?;
    t.set("lost", p.lost.clone())?;
    Ok(t)
}

fn measured(lua: &Lua, m: &Measure) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("bytes", m.bytes as f64)?;
    t.set("on_disk", m.on_disk as f64)?;
    t.set("files", m.files as f64)?;
    t.set("folders", m.folders as f64)?;
    t.set("unread", m.unread as f64)?;
    Ok(t)
}

fn facts(lua: &Lua, f: &files::Facts) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("mode", f.mode)?;
    t.set("owner", f.owner.as_str())?;
    t.set("group", f.group.as_str())?;
    t.set("created", f.created)?;
    t.set("modified", f.modified)?;
    t.set("accessed", f.accessed)?;
    if let Some(target) = &f.target {
        t.set("target", bytes(lua, target)?)?;
    }
    t.set("links", f.links as f64)?;
    t.set("free", f.free as f64)?;
    t.set("total", f.total as f64)?;
    Ok(t)
}

fn seen(lua: &Lua, s: &Seen) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("dir", s.dir)?;
    t.set("size", s.size as f64)?;
    t.set("modified", s.modified)?;
    Ok(t)
}

fn question(lua: &Lua, q: &Question) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("answers", q.answers())?;
    match q {
        Question::Exists { from, to, source, target, .. } => {
            t.set("kind", "exists")?;
            t.set("path", bytes(lua, from)?)?;
            t.set("to", bytes(lua, to)?)?;
            t.set("source", seen(lua, source)?)?;
            t.set("target", seen(lua, target)?)?;
        }
        Question::Failed { path, error } => {
            t.set("kind", "failed")?;
            t.set("path", bytes(lua, path)?)?;
            t.set("error", error.as_str())?;
        }
        Question::Space { to, needed, available } => {
            t.set("kind", "space")?;
            t.set("path", bytes(lua, to)?)?;
            t.set("needed", *needed as f64)?;
            t.set("available", *available as f64)?;
        }
    }
    Ok(t)
}

fn job(lua: &Lua, j: &Job) -> mlua::Result<Table> {
    let p = &j.progress;
    let t = lua.create_table()?;
    t.set("id", j.id as f64)?;
    t.set("kind", j.kind)?;
    // Told to wait, it does at its next look.
    let state = if j.paused && p.state != State::Asking { State::Paused } else { p.state };
    t.set("state", state.name())?;
    t.set("count", j.count)?;
    if let Some(to) = &j.to {
        t.set("to", bytes(lua, to)?)?;
    }
    t.set("bytes", p.bytes as f64)?;
    t.set("total_bytes", p.total_bytes as f64)?;
    t.set("files", p.files as f64)?;
    t.set("total_files", p.total_files as f64)?;
    t.set("current", bytes(lua, &p.current)?)?;
    t.set("rate", p.rate)?;
    if p.rate > 1.0 && p.total_bytes > p.bytes {
        t.set("remaining", (p.total_bytes - p.bytes) as f64 / p.rate)?;
    }
    if let Some(q) = &p.question {
        t.set("question", question(lua, q)?)?;
    }
    Ok(t)
}

/// Absolute paths: one relative to wherever the program was started says
/// nothing.
fn whole(path: &LuaString) -> mlua::Result<PathBuf> {
    let path = path_of(path);
    if path.is_absolute() { Ok(path) } else { Err(err(format!("files: {} is not an absolute path", path.display()))) }
}

fn many(list: Table) -> mlua::Result<Vec<PathBuf>> {
    list.sequence_values::<LuaString>().map(|p| whole(&p?)).collect()
}

/// Something counted, stopped by `stop`.
pub struct Counting(Arc<AtomicBool>);

impl UserData for Counting {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("stop", |_, this, ()| {
            this.0.store(true, Ordering::Relaxed);
            Ok(())
        });
    }
}

pub struct FilesRef(pub Rc<Files>);

impl UserData for FilesRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // files.jobs: what is being done and what waits, in order; one that
        // ended is told to its on_done and gone from here.
        fields.add_field_method_get("jobs", |lua, this| {
            let _ = this.0.version.try_get();
            let list = lua.create_table()?;
            for j in this.0.jobs.borrow().iter() {
                list.raw_push(job(lua, j)?)?;
            }
            Ok(list)
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // files:copy(paths, folder, on_done) → the job's id.
        methods.add_method("copy", |_, this, (from, to, f): (Table, LuaString, Option<Function>)| {
            this.0.start(Task::Copy { from: many(from)?, to: whole(&to)? }, f)
        });
        methods.add_method("move", |_, this, (from, to, f): (Table, LuaString, Option<Function>)| {
            this.0.start(Task::Move { from: many(from)?, to: whole(&to)? }, f)
        });
        methods.add_method("link", |_, this, (from, to, f): (Table, LuaString, Option<Function>)| {
            this.0.start(Task::Link { from: many(from)?, to: whole(&to)? }, f)
        });
        // files:delete(paths, on_done): for good.
        methods.add_method("delete", |_, this, (list, f): (Table, Option<Function>)| {
            this.0.start(Task::Delete { paths: many(list)? }, f)
        });
        methods.add_method("trash", |_, this, (list, f): (Table, Option<Function>)| {
            this.0.start(Task::Trash { paths: many(list)? }, f)
        });
        methods.add_method("pause", |_, this, id: u64| {
            this.0.job(id, |j| {
                j.paused = true;
                j.control.pause(true);
            });
            this.0.moved();
            Ok(())
        });
        methods.add_method("resume", |_, this, id: u64| {
            this.0.job(id, |j| {
                j.paused = false;
                j.control.pause(false);
            });
            this.0.moved();
            Ok(())
        });
        methods.add_method("cancel", |_, this, id: u64| {
            this.0.job(id, |j| j.control.cancel());
            Ok(())
        });
        // files:answer(id, "replace", for_all).
        methods.add_method("answer", |_, this, (id, name, for_all): (u64, String, Option<bool>)| {
            let answer = Answer::from_name(&name).ok_or_else(|| err(format!("files: {name:?} is not an answer")))?;
            let mut takes = Ok(());
            this.0.job(id, |j| match &j.progress.question {
                Some(q) if q.answers().contains(&name.as_str()) => j.control.answer(answer, for_all.unwrap_or(false)),
                Some(q) => takes = Err(err(format!("files: {name:?} is no answer to this, {} are", q.answers().join(", ")))),
                None => {}
            });
            takes
        });
        // files:rename(path, name, function(path, error) end): never over
        // another.
        methods.add_method("rename", |_, this, (path, name, f): (LuaString, LuaString, Option<Function>)| {
            let (path, name) = (whole(&path)?, OsStr::from_bytes(&name.as_bytes()).to_owned());
            this.0.named(move || files::rename(&path, &name), f)
        });
        methods.add_method("make_folder", |_, this, (dir, name, f): (LuaString, LuaString, Option<Function>)| {
            let (dir, name) = (whole(&dir)?, OsStr::from_bytes(&name.as_bytes()).to_owned());
            this.0.named(move || files::make_folder(&dir, &name), f)
        });
        methods.add_method("make_file", |_, this, (dir, name, f): (LuaString, LuaString, Option<Function>)| {
            let (dir, name) = (whole(&dir)?, OsStr::from_bytes(&name.as_bytes()).to_owned());
            this.0.named(move || files::make_file(&dir, &name), f)
        });
        // files:same_disk(paths, folder, function(same) end): whether moving
        // them there only changes names.
        methods.add_method("same_disk", |_, this, (list, folder, f): (Table, LuaString, Function)| {
            let (list, folder) = (many(list)?, whole(&folder)?);
            let Some(post) = this.0.post.borrow().clone() else { return Err(err("files: not started")) };
            let id = this.0.id();
            this.0.waiting.borrow_mut().insert(id, (f, current_owner()));
            std::thread::Builder::new()
                .name("genoma-files".into())
                .spawn(move || post.send(Sent::Told(id, files::same_disk(&list, &folder))))
                .map(|_| ())
                .map_err(|e| err(format!("files: no thread: {e}")))
        });
        // files:facts(path, function(facts or nil) end).
        methods.add_method("facts", |_, this, (path, f): (LuaString, Function)| {
            let path = whole(&path)?;
            let Some(post) = this.0.post.borrow().clone() else { return Err(err("files: not started")) };
            let id = this.0.id();
            this.0.waiting.borrow_mut().insert(id, (f, current_owner()));
            std::thread::Builder::new()
                .name("genoma-files".into())
                .spawn(move || post.send(Sent::Facts(id, files::facts(&path).ok().map(Box::new))))
                .map(|_| ())
                .map_err(|e| err(format!("files: no thread: {e}")))
        });
        // files:set_attribute(paths, name, value or nil, function(failed) end).
        methods.add_method("set_attribute", |_, this, (list, name, value, f): (Table, String, Option<String>, Option<Function>)| {
            if !name.starts_with("user.") {
                return Err(err("files:set_attribute: its name begins with \"user.\""));
            }
            let list = many(list)?;
            let Some(post) = this.0.post.borrow().clone() else { return Err(err("files: not started")) };
            let id = this.0.id();
            if let Some(f) = f {
                this.0.waiting.borrow_mut().insert(id, (f, current_owner()));
            }
            std::thread::Builder::new()
                .name("genoma-files".into())
                .spawn(move || {
                    let failed = list.into_iter().filter(|p| files::set_attribute(p, &name, value.as_deref()).is_err()).collect();
                    post.send(Sent::Failed(id, failed));
                })
                .map(|_| ())
                .map_err(|e| err(format!("files: no thread: {e}")))
        });
        // files:check_name(name) → why a thing cannot be called that, or
        // nil: told as it is typed.
        methods.add_method("check_name", |_, _, name: LuaString| Ok(files::check_name(OsStr::from_bytes(&name.as_bytes())).err()));
        // files:measure(paths, function(total, whole) end) → what stops it.
        methods.add_method("measure", |_, this, (list, f): (Table, Function)| {
            let list = many(list)?;
            let Some(post) = this.0.post.borrow().clone() else { return Err(err("files: not started")) };
            let id = this.0.id();
            this.0.waiting.borrow_mut().insert(id, (f, current_owner()));
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            std::thread::Builder::new()
                .name("genoma-files".into())
                .spawn(move || {
                    let all = files::measure(&list, |m| {
                        post.send(Sent::Measured(id, *m, false));
                        !stopped.load(Ordering::Relaxed)
                    });
                    if let Some(all) = all.filter(|_| !stopped.load(Ordering::Relaxed)) {
                        post.send(Sent::Measured(id, all, true));
                    }
                })
                .map_err(|e| err(format!("files: no thread: {e}")))?;
            Ok(Counting(stop))
        });
    }
}

// ---- @genoma/trash -----------------------------------------------------------

/// An item, and what a list shows of it.
#[derive(Clone)]
struct Thrown {
    item: trash::Item,
    dir: bool,
    size: u64,
    mime: String,
}

pub struct Trash {
    files: Rc<Files>,
    host: HostRef,
    stop: Rc<Stop>,
    items: RefCell<Vec<Thrown>>,
    version: Signal<u64>,
    loading: Signal<bool>,
    busy: Cell<bool>,
    again: Cell<bool>,
    watching: RefCell<Vec<(PathBuf, Rc<Stop>)>>,
    post: RefCell<Option<Post<(Vec<Thrown>, Vec<PathBuf>)>>>,
}

impl Trash {
    pub fn new(host: &HostRef, files: Rc<Files>) -> mlua::Result<Rc<Trash>> {
        let me = Rc::new(Trash {
            files,
            host: host.clone(),
            stop: Stop::new(host),
            items: RefCell::default(),
            version: Signal::new(0),
            loading: Signal::new(true),
            busy: Cell::new(false),
            again: Cell::new(false),
            watching: RefCell::default(),
            post: RefCell::default(),
        });
        let weak = Rc::downgrade(&me);
        let post = mailbox(host, &me.stop, "trash", move |(items, watched)| {
            if let Some(me) = weak.upgrade() {
                me.show(items, watched);
            }
        })?;
        *me.post.borrow_mut() = Some(post);
        me.read();
        Ok(me)
    }

    fn read(&self) {
        if self.busy.replace(true) {
            self.again.set(true);
            return;
        }
        let Some(post) = self.post.borrow().clone() else { return };
        let thread = std::thread::Builder::new().name("genoma-trash".into()).spawn(move || {
            let db = mime::database();
            let thrown = trash::items().into_iter().map(|item| {
                let m = std::fs::symlink_metadata(&item.file).ok();
                let dir = m.as_ref().is_some_and(|m| m.is_dir());
                Thrown { dir, size: m.filter(|_| !dir).map_or(0, |m| m.len()), mime: db.of(&item.file).to_string(), item }
            });
            post.send((thrown.collect(), trash::watched()));
        });
        if thread.is_err() {
            self.busy.set(false);
        }
    }

    fn show(self: &Rc<Self>, items: Vec<Thrown>, watched: Vec<PathBuf>) {
        self.busy.set(false);
        {
            let mut watching = self.watching.borrow_mut();
            watching.retain(|(p, stop)| {
                let kept = watched.contains(p);
                if !kept {
                    stop.stop();
                }
                kept
            });
            for path in watched {
                if watching.iter().any(|(p, _)| *p == path) {
                    continue;
                }
                let stop = genoma_core::reactive::owned_by(self.stop.owner, || Stop::new(&self.host));
                let me = Rc::downgrade(self);
                watch_dir(
                    &self.host,
                    &stop,
                    path.clone(),
                    "trash",
                    Box::new(move |_, _| {
                        if let Some(me) = me.upgrade() {
                            me.read();
                        }
                    }),
                );
                watching.push((path, stop));
            }
        }
        *self.items.borrow_mut() = items;
        batch(|| {
            self.loading.set(false);
            let v = untracked(|| self.version.try_get()).unwrap_or(0);
            self.version.set(v + 1);
        });
        if self.again.replace(false) {
            self.read();
        }
    }

    /// The items whose `file` is among `files`; all of them without.
    fn chosen(&self, files: Option<Vec<PathBuf>>) -> Vec<trash::Item> {
        let items = self.items.borrow();
        items.iter().map(|t| &t.item).filter(|i| files.as_ref().is_none_or(|f| f.contains(&i.file))).cloned().collect()
    }
}

pub struct TrashRef(pub Rc<Trash>);

impl UserData for TrashRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // trash.items → { { name, path, file, deleted, dir, size, mime } },
        // the latest thrown away first.
        fields.add_field_method_get("items", |lua, this| {
            let _ = this.0.version.try_get();
            let list = lua.create_table()?;
            for t in this.0.items.borrow().iter() {
                let e = lua.create_table()?;
                e.set("name", t.item.path.file_name().unwrap_or_default().to_string_lossy().as_ref())?;
                e.set("path", bytes(lua, &t.item.path)?)?;
                e.set("file", bytes(lua, &t.item.file)?)?;
                e.set("deleted", trash::seconds(&t.item.deleted))?;
                e.set("dir", t.dir)?;
                e.set("size", t.size as f64)?;
                e.set("mime", t.mime.as_str())?;
                list.raw_push(e)?;
            }
            Ok(list)
        });
        fields.add_field_method_get("count", |_, this| {
            let _ = this.0.version.try_get();
            Ok(this.0.items.borrow().len())
        });
        fields.add_field_method_get("loading", |_, this| Ok(this.0.loading.try_get().unwrap_or(false)));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // trash:restore(files, on_done): back where they were; `files` are
        // the items' own.
        methods.add_method("restore", |_, this, (list, f): (Table, Option<Function>)| {
            this.0.files.start(Task::Restore { items: this.0.chosen(Some(many(list)?)) }, f)
        });
        // trash:erase(files, on_done): for good.
        methods.add_method("erase", |_, this, (list, f): (Table, Option<Function>)| {
            this.0.files.start(Task::Erase { items: this.0.chosen(Some(many(list)?)) }, f)
        });
        // trash:empty(on_done): all of what it holds now, for good.
        methods.add_method("empty", |_, this, f: Option<Function>| this.0.files.start(Task::Erase { items: this.0.chosen(None) }, f));
        methods.add_method("reload", |_, this, ()| {
            this.0.read();
            Ok(())
        });
    }
}
