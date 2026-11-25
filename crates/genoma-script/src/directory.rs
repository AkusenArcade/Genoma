//! `g.directory`: a folder for a file manager, read, sorted and filtered
//! off the loop, and handed to the interface a window at a time.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use genoma_core::reactive::{Effect, Signal, batch, owned_by, untracked};
use genoma_services::{
    directory::{self, By, Cancel, Entry, Found, Listing, View},
    mime,
};
use mlua::{Lua, LuaString, Table, UserData, UserDataFields, UserDataMethods, Value as LuaValue};

use crate::{
    err,
    io::{HostRef, Post, Stop, mailbox, watch_dir},
    mime::path_of,
};

/// The least between two readings of what changed: a copy of thousands of
/// files is a few listings a second, not one a file.
const SETTLE: Duration = Duration::from_millis(100);

/// How long nothing is typed before the folders below are looked in.
const PAUSE: Duration = Duration::from_millis(150);

/// What there is to do, beside what is being done.
#[derive(Default)]
struct Due {
    /// The folder read whole.
    read: bool,
    /// These entries of it read again.
    names: BTreeSet<OsString>,
    /// The same entries in the view asked for now.
    arrange: bool,
    /// What else was found below it.
    found: Vec<Arc<Entry>>,
}

/// A search below the folder.
#[derive(Default)]
struct Search {
    /// What it looks for: the filter, and whether hidden ones too.
    asked: Option<(String, bool)>,
    /// All of it was found.
    whole: bool,
}

struct Inner {
    host: HostRef,
    stop: Rc<Stop>,
    /// What is shown, and what moves each time it is replaced.
    shown: RefCell<Arc<Listing>>,
    version: Signal<u64>,
    loading: Signal<bool>,
    path: RefCell<PathBuf>,
    view: RefCell<View>,
    due: RefCell<Due>,
    busy: Cell<bool>,
    last: Cell<Option<Instant>>,
    waiting: Cell<bool>,
    /// Counts the folders asked for: a job for one before stops, and what
    /// it made is nobody's.
    asked: Arc<AtomicU64>,
    /// Counts the searches, the same way.
    searches: Arc<AtomicU64>,
    search: RefCell<Search>,
    searching: Signal<bool>,
    capped: Signal<bool>,
    /// Where the jobs leave what they made.
    post: RefCell<Option<Post<Made>>>,
    /// The watch on the folder asked for.
    watching: RefCell<Option<Rc<Stop>>>,
    stopped: Cell<bool>,
}

/// What a job made: for which folder asked for, in which view; nothing when
/// nothing changed, or why it broke.
enum Made {
    Listing { asked: u64, view: View, listing: Result<Option<Listing>, ()> },
    Found { search: u64, found: Found },
}

impl Inner {
    /// Starts what is due, when nothing is under way.
    fn run(self: &Rc<Self>) {
        if self.busy.get() || self.stopped.get() {
            return;
        }
        let shown = self.shown.borrow().clone();
        let current = shown.path == *self.path.borrow();
        let mut due = self.due.borrow_mut();
        if !due.read && !due.arrange && due.names.is_empty() && due.found.is_empty() {
            return;
        }
        // Only what changed outside waits; what the person asks does not.
        if !due.read && !due.arrange {
            let since = self.last.get().map_or(SETTLE, |t| t.elapsed());
            if since < SETTLE {
                if !self.waiting.replace(true) {
                    let me = Rc::downgrade(self);
                    self.host.timer(
                        SETTLE - since,
                        Box::new(move || {
                            if let Some(me) = me.upgrade() {
                                me.waiting.set(false);
                                me.run();
                            }
                            None
                        }),
                    );
                }
                return;
            }
        }
        let whole = due.read || !current;
        let names: Vec<OsString> = std::mem::take(&mut due.names).into_iter().collect();
        let found = std::mem::take(&mut due.found);
        // Read again, it is still the same search.
        let again = current && due.read;
        *due = Due::default();
        drop(due);
        let (path, view) = (self.path.borrow().clone(), self.view.borrow().clone());
        let cancel = Cancel { now: self.asked.clone(), started: self.asked() };
        let sort = view.clone();
        self.start(view, move || {
            let db = mime::database();
            let shown = if found.is_empty() { shown } else { Arc::new(shown.joined(found, &sort)) };
            if again {
                return Some(shown.again(&sort, &db, &cancel));
            }
            if whole {
                return Some(Listing::read(&path, &sort, &db, &cancel));
            }
            // A change with nothing in it still leaves what was found.
            if names.is_empty() { Some(shown.arranged(&sort)) } else { shown.updated(&names, &sort, &db).or_else(|| Some(shown.arranged(&sort))) }
        });
    }

    /// Then the types the names left open, as a job of its own: the folder
    /// is seen before any file in it is opened.
    fn tell(self: &Rc<Self>) {
        let shown = self.shown.borrow().clone();
        if self.busy.get() || self.stopped.get() || !shown.unsure() {
            return;
        }
        let view = self.view.borrow().clone();
        let cancel = Cancel { now: self.asked.clone(), started: self.asked() };
        let sort = view.clone();
        self.start(view, move || Some(shown.told(&sort, &mime::database(), &cancel)));
    }

    fn asked(&self) -> u64 {
        self.asked.load(Ordering::Relaxed)
    }

    fn start(&self, view: View, work: impl FnOnce() -> Option<Listing> + Send + 'static) {
        let Some(post) = self.post.borrow().clone() else { return };
        self.busy.set(true);
        let asked = self.asked();
        let thread = std::thread::Builder::new().name("genoma-directory".into()).spawn(move || {
            // A job that broke must not leave the folder waiting for it.
            let listing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).map_err(|_| ());
            post.send(Made::Listing { asked, view, listing });
        });
        if let Err(e) = thread {
            self.busy.set(false);
            log::error!("g.directory: no thread: {e}");
        }
    }

    /// The search the view asks for, begun when it is another than the one
    /// made or under way.
    fn look(self: &Rc<Self>, anew: bool) {
        let view = self.view.borrow().clone();
        let wanted = (view.deep && !view.filter.trim().is_empty()).then(|| (directory::folded(&view.filter), view.hidden));
        {
            let mut s = self.search.borrow_mut();
            // All that has this name in it has the shorter one in it too.
            let covered = match (&s.asked, &wanted) {
                (Some(a), Some(b)) => a == b || (s.whole && a.1 == b.1 && b.0.contains(&a.0)),
                (None, None) => true,
                _ => false,
            };
            if covered && !anew {
                // What is asked for next is measured against this.
                return;
            }
            if !covered || anew {
                *s = Search { asked: wanted.clone(), whole: false };
            }
        }
        let search = self.searches.fetch_add(1, Ordering::Relaxed) + 1;
        self.due.borrow_mut().found.clear();
        batch(|| {
            self.capped.set(false);
            self.searching.set(wanted.is_some());
        });
        let Some((filter, hidden)) = wanted else { return };
        let me = Rc::downgrade(self);
        self.host.timer(
            PAUSE,
            Box::new(move || {
                if let Some(me) = me.upgrade().filter(|m| m.searches.load(Ordering::Relaxed) == search && !m.stopped.get()) {
                    me.begin(search, filter.clone(), hidden);
                }
                None
            }),
        );
    }

    fn begin(&self, search: u64, filter: String, hidden: bool) {
        let Some(post) = self.post.borrow().clone() else { return };
        let path = self.path.borrow().clone();
        let cancel = Cancel { now: self.searches.clone(), started: search };
        let thread = std::thread::Builder::new().name("genoma-search".into()).spawn(move || {
            let last = post.clone();
            let post = std::sync::Mutex::new(post);
            let work = || {
                directory::search(&path, &filter, hidden, &mime::database(), &cancel, &|found| {
                    post.lock().unwrap().send(Made::Found { search, found });
                });
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
                last.send(Made::Found { search, found: Found { entries: Vec::new(), done: true, capped: false } });
            }
        });
        if let Err(e) = thread {
            self.searching.set(false);
            log::error!("g.directory: no thread: {e}");
        }
    }

    fn done(self: &Rc<Self>, made: Made) {
        let (asked, view, listing) = match made {
            Made::Listing { asked, view, listing } => (asked, view, listing),
            Made::Found { search, found } => {
                if search != self.searches.load(Ordering::Relaxed) || self.stopped.get() {
                    return;
                }
                self.due.borrow_mut().found.extend(found.entries);
                if found.done {
                    self.search.borrow_mut().whole = !found.capped;
                    batch(|| {
                        self.capped.set(found.capped);
                        self.searching.set(false);
                    });
                }
                self.run();
                return;
            }
        };
        // Another folder was asked for meanwhile, and did not wait.
        if asked != self.asked() || self.stopped.get() {
            return;
        }
        self.busy.set(false);
        match listing {
            Ok(listing) => self.show(listing, &view),
            Err(()) => log::error!("g.directory {}: the reading stopped", self.path.borrow().display()),
        }
        self.run();
        self.tell();
    }

    /// `made` in `view` is what is shown now; none, that nothing changed.
    fn show(&self, made: Option<Listing>, view: &View) {
        self.last.set(Some(Instant::now()));
        if *self.view.borrow() != *view {
            self.due.borrow_mut().arrange = true;
        }
        let Some(made) = made else { return };
        *self.shown.borrow_mut() = Arc::new(made);
        batch(|| {
            self.loading.set(false);
            let v = untracked(|| self.version.try_get()).unwrap_or(0);
            self.version.set(v + 1);
        });
    }

    /// What is shown, followed by the binding that reads it.
    fn read(&self) -> Arc<Listing> {
        let _ = self.version.try_get();
        self.shown.borrow().clone()
    }
}

pub struct Directory(Rc<Inner>);

fn bytes(lua: &Lua, path: &std::path::Path) -> mlua::Result<LuaString> {
    lua.create_string(path.as_os_str().as_bytes())
}

fn entry(lua: &Lua, listing: &Listing, e: &Entry) -> mlua::Result<Table> {
    let t = lua.create_table_with_capacity(0, 11)?;
    let (leaf, within) = e.leaf();
    t.raw_set("name", leaf.to_string_lossy().as_ref())?;
    if let Some(within) = within {
        t.raw_set("within", within.to_string_lossy().as_ref())?;
    }
    t.raw_set("path", bytes(lua, &listing.path.join(&e.name))?)?;
    t.raw_set("dir", e.dir)?;
    t.raw_set("link", e.link)?;
    t.raw_set("broken", e.broken)?;
    t.raw_set("hidden", e.hidden())?;
    t.raw_set("executable", e.executable)?;
    t.raw_set("size", e.size as f64)?;
    t.raw_set("modified", e.modified)?;
    t.raw_set("mime", &*e.mime)?;
    Ok(t)
}

impl UserData for Directory {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // d.count: how many entries are shown. A binding that reads it
        // follows the folder, a rename too.
        fields.add_field_method_get("count", |_, this| Ok(this.0.read().shown.len()));
        // d.total: how many there are, the hidden and the filtered out too.
        fields.add_field_method_get("total", |_, this| Ok(this.0.read().total()));
        // d.path: the folder shown; the one asked for, once it is read.
        fields.add_field_method_get("path", |lua, this| bytes(lua, &this.0.read().path));
        // d.loading: the folder asked for is being read for the first time.
        fields.add_field_method_get("loading", |_, this| Ok(this.0.loading.try_get().unwrap_or(false)));
        // d.searching: the folders below are being looked in.
        fields.add_field_method_get("searching", |_, this| Ok(this.0.searching.try_get().unwrap_or(false)));
        // d.capped: more was found there than is kept.
        fields.add_field_method_get("capped", |_, this| Ok(this.0.capped.try_get().unwrap_or(false)));
        // d.error: why it could not be read, in words, or nil.
        fields.add_field_method_get("error", |_, this| Ok(this.0.read().error.clone()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // d:at(i) → the entry at a place, from 1, or nil.
        methods.add_method("at", |lua, this, i: usize| {
            let listing = this.0.read();
            match i.checked_sub(1).and_then(|i| listing.shown.get(i)) {
                Some(e) => Ok(LuaValue::Table(entry(lua, &listing, e)?)),
                None => Ok(LuaValue::Nil),
            }
        });
        // d:slice(first, last) → the entries between two places.
        methods.add_method("slice", |lua, this, (first, last): (usize, usize)| {
            let listing = this.0.read();
            let list = lua.create_table()?;
            for e in listing.shown.iter().take(last).skip(first.max(1) - 1) {
                list.raw_push(entry(lua, &listing, e)?)?;
            }
            Ok(list)
        });
        // d:paths(first?, last?) → their paths; all of them.
        methods.add_method("paths", |lua, this, (first, last): (Option<usize>, Option<usize>)| {
            let listing = this.0.read();
            let list = lua.create_table()?;
            for e in listing.shown.iter().take(last.unwrap_or(usize::MAX)).skip(first.unwrap_or(1).max(1) - 1) {
                list.raw_push(bytes(lua, &listing.path.join(&e.name))?)?;
            }
            Ok(list)
        });
        // d:index(path) → an entry's place, or nil when it is not shown.
        methods.add_method("index", |_, this, path: LuaString| {
            let listing = this.0.read();
            let path = path_of(&path);
            let Ok(name) = path.strip_prefix(&listing.path) else { return Ok(None) };
            Ok(listing.shown.iter().position(|e| e.name == name.as_os_str()).map(|i| i + 1))
        });
        // d:reload(): read whole again (a filesystem that tells nobody when
        // it changes: a network's, some FUSE ones).
        methods.add_method("reload", |_, this, ()| {
            this.0.due.borrow_mut().read = true;
            untracked(|| this.0.look(true));
            this.0.run();
            Ok(())
        });
        methods.add_method("stop", |_, this, ()| {
            this.0.stopped.set(true);
            this.0.asked.fetch_add(1, Ordering::Relaxed);
            this.0.searches.fetch_add(1, Ordering::Relaxed);
            if let Some(watch) = this.0.watching.borrow_mut().take() {
                watch.stop();
            }
            this.0.stop.stop();
            Ok(())
        });
    }
}

/// `g.directory { path, hidden, sort, descending, folders_first, filter,
/// deep, watch }`: each a value or a function, followed: `path` moves it to
/// another folder, the others show the same one differently.
pub fn directory(host: &HostRef, base: PathBuf, t: Table) -> mlua::Result<Directory> {
    let value = move |t: &Table, name: &str| -> mlua::Result<LuaValue> {
        match t.get::<LuaValue>(name)? {
            LuaValue::Function(f) => f.call::<LuaValue>(()),
            v => Ok(v),
        }
    };
    let asked = move |t: &Table| -> mlua::Result<(PathBuf, View)> {
        let LuaValue::String(path) = value(t, "path")? else { return Err(err("g.directory wants a path, a string")) };
        // A relative path is the interface's own.
        let path = match path_of(&path) {
            p if p.is_absolute() => p,
            p => base.join(p),
        };
        // "/home/" and "/home/." are "/home": an entry's parent is compared
        // with it.
        let path: PathBuf = path.components().collect();
        let yes = |name: &str, default: bool| -> mlua::Result<bool> {
            match value(t, name)? {
                LuaValue::Nil => Ok(default),
                LuaValue::Boolean(b) => Ok(b),
                _ => Err(err(format!("g.directory: {name} is a boolean"))),
            }
        };
        let text = |name: &str| -> mlua::Result<Option<String>> {
            match value(t, name)? {
                LuaValue::Nil => Ok(None),
                LuaValue::String(s) => Ok(Some(s.to_string_lossy())),
                _ => Err(err(format!("g.directory: {name} is a string"))),
            }
        };
        let by = text("sort")?.map_or(Ok(By::Name), |s| {
            By::from_name(&s).ok_or_else(|| err(format!("g.directory: sort is \"name\", \"size\", \"modified\" or \"type\", not {s:?}")))
        })?;
        let view = View {
            hidden: yes("hidden", false)?,
            by,
            descending: yes("descending", false)?,
            folders_first: yes("folders_first", true)?,
            filter: text("filter")?.unwrap_or_default(),
            deep: yes("deep", false)?,
        };
        Ok((path, view))
    };
    // A mistake in how it is asked for is told where it is made.
    untracked(|| asked(&t))?;
    let watch = t.get::<Option<bool>>("watch")?.unwrap_or(true);
    let inner = Rc::new(Inner {
        host: host.clone(),
        stop: Stop::new(host),
        shown: RefCell::default(),
        version: Signal::new(0),
        loading: Signal::new(true),
        path: RefCell::default(),
        view: RefCell::default(),
        due: RefCell::default(),
        busy: Cell::new(false),
        last: Cell::new(None),
        waiting: Cell::new(false),
        asked: Arc::default(),
        searches: Arc::default(),
        search: RefCell::default(),
        searching: Signal::new(false),
        capped: Signal::new(false),
        post: RefCell::default(),
        watching: RefCell::default(),
        stopped: Cell::new(false),
    });
    let me = Rc::downgrade(&inner);
    let post = mailbox(host, &inner.stop, "g.directory", move |made| {
        if let Some(me) = me.upgrade() {
            me.done(made);
        }
    })?;
    *inner.post.borrow_mut() = Some(post);
    let me = Rc::downgrade(&inner);
    let first = Cell::new(true);
    Effect::new(move || {
        let Some(me) = me.upgrade().filter(|m| !m.stopped.get()) else { return };
        let (path, view) = match asked(&t) {
            Ok(a) => a,
            Err(e) => {
                log::error!("{e}");
                return;
            }
        };
        untracked(|| {
            let moved = first.replace(false) || *me.path.borrow() != path;
            let turned = *me.view.borrow() != view;
            *me.view.borrow_mut() = view;
            if moved {
                *me.path.borrow_mut() = path.clone();
                me.asked.fetch_add(1, Ordering::Relaxed);
                *me.due.borrow_mut() = Due { read: true, ..Default::default() };
                *me.search.borrow_mut() = Search::default();
                me.searches.fetch_add(1, Ordering::Relaxed);
                me.searching.set(false);
                // A folder on a mount that hangs holds its own thread only.
                me.busy.set(false);
                me.loading.set(true);
                // Watched before it is read: what changes in between is
                // read again rather than missed.
                if let Some(old) = me.watching.borrow_mut().take() {
                    old.stop();
                }
                if watch {
                    // The load's, not this effect's, whose next run is only
                    // another order of the same folder.
                    let stop = owned_by(me.stop.owner, || Stop::new(&me.host));
                    let m = Rc::downgrade(&me);
                    watch_dir(
                        &me.host,
                        &stop,
                        path,
                        "g.directory",
                        Box::new(move |names, came| {
                            let Some(m) = m.upgrade() else { return };
                            {
                                let mut due = m.due.borrow_mut();
                                due.read |= came;
                                due.names.extend(names.iter().map(|n| OsString::from_vec(n.clone())));
                            }
                            m.run();
                        }),
                    );
                    *me.watching.borrow_mut() = Some(stop);
                }
            } else if turned {
                me.due.borrow_mut().arrange = true;
            }
            me.look(false);
            me.run();
        });
    });
    Ok(Directory(inner))
}
