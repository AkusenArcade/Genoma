//! `@genoma/thumbnails` in Luau: each file's thumbnail as a value bindings
//! follow, made while something looks at it.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use genoma_core::reactive::{Signal, current_owner, on_cleanup, owned_by, untracked};
use genoma_services::thumbnails::{Outcome, Queue, Size, Wanted};
use mlua::{LuaString, Table, UserData, UserDataMethods};

use crate::{
    err,
    io::{HostRef, Stop, mailbox},
    mime::path_of,
};

/// The most thumbnails remembered that nothing looks at now.
const KEPT: usize = 4096;

struct Known {
    /// The thumbnail's file; nil until it is made, or when there is none.
    file: Signal<Option<String>>,
    /// The file's time it was asked for at, as given.
    modified: Option<f64>,
    mime: Option<String>,
    /// How many bindings look at it.
    looking: usize,
    asked: bool,
    answered: bool,
    /// When it was last looked at, among the others.
    used: u64,
}

pub struct Thumbnails {
    queue: Queue,
    known: RefCell<HashMap<Wanted, Known>>,
    /// What owns the signals: the load, not the binding that asked first.
    owner: Option<usize>,
    host: HostRef,
    clock: Cell<u64>,
    _stop: Rc<Stop>,
}

impl Thumbnails {
    pub fn new(host: &HostRef, owner: Option<usize>) -> mlua::Result<Rc<Thumbnails>> {
        let stop = owned_by(owner, || Stop::new(host));
        let me: Rc<RefCell<std::rc::Weak<Thumbnails>>> = Rc::default();
        let mine = me.clone();
        let post = mailbox(host, &stop, "thumbnails", move |(wanted, outcome): (Wanted, Outcome)| {
            if let Some(me) = mine.borrow().upgrade() {
                me.answer(wanted, outcome);
            }
        })?;
        let made = Rc::new(Thumbnails {
            queue: Queue::new(move |wanted, outcome| post.send((wanted, outcome))),
            known: RefCell::default(),
            owner,
            host: host.clone(),
            clock: Cell::new(0),
            _stop: stop,
        });
        *me.borrow_mut() = Rc::downgrade(&made);
        Ok(made)
    }

    /// The thumbnail of `path`, followed by the binding that asks and asked
    /// for while it looks.
    fn look(self: &Rc<Self>, wanted: Wanted, modified: Option<f64>, mime: Option<String>) -> Option<String> {
        self.clock.set(self.clock.get() + 1);
        let file = {
            let mut known = self.known.borrow_mut();
            let k = known.entry(wanted.clone()).or_insert_with(|| Known {
                file: owned_by(self.owner, || Signal::new(None)),
                modified,
                mime: None,
                looking: 0,
                asked: false,
                answered: false,
                used: 0,
            });
            k.used = self.clock.get();
            k.mime = mime;
            // Changed since, or nobody says: looked at again.
            if k.modified != modified || (modified.is_none() && k.looking == 0) {
                k.modified = modified;
                k.answered = false;
            }
            if current_owner().is_some() {
                k.looking += 1;
            }
            if !k.asked && !k.answered {
                k.asked = true;
                self.queue.ask(wanted.clone(), k.mime.clone());
            }
            k.file
        };
        if current_owner().is_some() {
            let me = Rc::downgrade(self);
            on_cleanup(move || {
                if let Some(me) = me.upgrade() {
                    me.leave(&wanted);
                }
            });
        }
        self.trim();
        file.try_get().flatten()
    }

    /// One binding fewer looks: the last takes it out of the queue.
    fn leave(&self, wanted: &Wanted) {
        let mut known = self.known.borrow_mut();
        let Some(k) = known.get_mut(wanted) else { return };
        k.looking = k.looking.saturating_sub(1);
        if k.looking == 0 && k.asked {
            k.asked = false;
            self.queue.forget(wanted);
        }
    }

    fn answer(self: &Rc<Self>, wanted: Wanted, outcome: Outcome) {
        let (file, now) = {
            let mut known = self.known.borrow_mut();
            let Some(k) = known.get_mut(&wanted) else { return };
            let file = k.file;
            k.asked = false;
            match outcome {
                Outcome::Ready { file: path, made } => {
                    k.answered = true;
                    let path = path.display().to_string();
                    if made {
                        // The same name as the one before it: what was
                        // drawn from that is of no more use.
                        genoma_scene::image_changed(&path);
                    }
                    (file, Some((Some(path), made)))
                }
                Outcome::None => {
                    k.answered = true;
                    (file, Some((None, false)))
                }
                Outcome::Later(after) => {
                    drop(known);
                    self.later(wanted, after);
                    return;
                }
            }
        };
        if let Some((path, made)) = now {
            if made && untracked(|| file.try_get()).flatten() == path {
                file.set(None);
            }
            file.set(path);
        }
    }

    /// Asked again once the file has had the time to be whole.
    fn later(self: &Rc<Self>, wanted: Wanted, after: Duration) {
        let me = Rc::downgrade(self);
        self.host.timer(
            after + Duration::from_millis(50),
            Box::new(move || {
                let me = me.upgrade()?;
                let mut known = me.known.borrow_mut();
                let k = known.get_mut(&wanted)?;
                if k.looking > 0 && !k.asked && !k.answered {
                    k.asked = true;
                    me.queue.ask(wanted.clone(), k.mime.clone());
                }
                None
            }),
        );
    }

    /// The ones longest unseen are let go of.
    fn trim(&self) {
        let mut known = self.known.borrow_mut();
        if known.len() <= KEPT * 2 {
            return;
        }
        let mut idle: Vec<(u64, Wanted)> = known.iter().filter(|(_, k)| k.looking == 0).map(|(w, k)| (k.used, w.clone())).collect();
        idle.sort_by_key(|(used, _)| *used);
        let spare = idle.len().saturating_sub(KEPT);
        for (_, w) in idle.into_iter().take(spare) {
            known.remove(&w);
        }
    }
}

pub struct ThumbnailsRef(pub Rc<Thumbnails>);

impl UserData for ThumbnailsRef {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // thumbnails:of(path, { size = "large", modified, mime }) → the
        // thumbnail's file, nil until it is made or when there is none.
        methods.add_method("of", |_, this, (path, options): (LuaString, Option<Table>)| {
            let (mut size, mut modified, mut mime) = (Size::Large, None, None);
            if let Some(t) = options {
                if let Some(name) = t.get::<Option<String>>("size")? {
                    size = Size::from_name(&name)
                        .ok_or_else(|| err(format!("thumbnails: size is \"normal\", \"large\", \"x-large\" or \"xx-large\", not {name:?}")))?;
                }
                modified = t.get::<Option<f64>>("modified")?;
                mime = t.get::<Option<String>>("mime")?;
            }
            Ok(this.0.look(Wanted { path: PathBuf::from(path_of(&path)), size }, modified, mime))
        });
        // thumbnails:can("video/mp4") → whether files of a type can have one.
        methods.add_method("can", |_, this, mime: String| Ok(this.0.queue.thumbnailer().can(&mime)));
    }
}
