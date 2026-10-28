//! The reactive graph (PRD §4, §7.2).
//!
//! Three kinds of node:
//! - a **signal** holds a value that something outside the graph sets;
//! - a **memo** derives a value from others and passes a change on only when
//!   the result differs;
//! - an **effect** runs a function for what it does (set a node's property,
//!   damage a surface) and runs again when something it read changed.
//!
//! Nothing declares its dependencies. Whatever a memo or an effect reads while
//! it runs is recorded, and only that wakes it. A change marks the direct
//! readers dirty and everything further down "check". Effects then pull: a
//! "check" node recomputes only if one of its sources really changed, so a
//! memo whose result is unchanged stops the change there.
//!
//! The graph is single-threaded and lives in the main thread, which owns all
//! state (PRD §4). Nodes belong to the effect or scope that created them and
//! are disposed with it, so throwing away an interface tree disposes its
//! bindings too. Hot reload depends on that.

use std::{any::Any, cell::RefCell, marker::PhantomData, rc::Rc};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Clean,
    Check,
    Dirty,
}

type Compute = Rc<dyn Fn(Option<&dyn Any>) -> Option<Box<dyn Any>>>;
type EffectFn = Rc<RefCell<dyn FnMut()>>;

enum Kind {
    Signal,
    /// Returns the new value, or `None` when it equals the old one.
    Memo(Compute),
    Effect(EffectFn),
    Scope,
}

struct Node {
    generation: u32,
    kind: Kind,
    value: Option<Box<dyn Any>>,
    state: State,
    sources: Vec<usize>,
    subscribers: Vec<usize>,
    owner: Option<usize>,
    owned: Vec<usize>,
    cleanups: Vec<Box<dyn FnOnce()>>,
}

#[derive(Default)]
struct Runtime {
    nodes: Vec<Option<Node>>,
    generations: Vec<u32>,
    free: Vec<usize>,
    observer: Option<usize>,
    owner: Option<usize>,
    pending: Vec<usize>,
    batch: u32,
    flushing: bool,
}

thread_local! {
    static RUNTIME: RefCell<Runtime> = RefCell::new(Runtime::default());
}

fn with<R>(f: impl FnOnce(&mut Runtime) -> R) -> R {
    RUNTIME.with(|rt| f(&mut rt.borrow_mut()))
}

impl Runtime {
    fn insert(&mut self, kind: Kind, value: Option<Box<dyn Any>>, state: State) -> (usize, u32) {
        let id = self.free.pop().unwrap_or_else(|| {
            self.nodes.push(None);
            self.generations.push(0);
            self.nodes.len() - 1
        });
        let generation = self.generations[id];
        let owner = self.owner;
        self.nodes[id] = Some(Node {
            generation,
            kind,
            value,
            state,
            sources: Vec::new(),
            subscribers: Vec::new(),
            owner,
            owned: Vec::new(),
            cleanups: Vec::new(),
        });
        if let Some(owner) = owner {
            if let Some(node) = self.nodes[owner].as_mut() {
                node.owned.push(id);
            }
        }
        (id, generation)
    }

    fn node(&self, id: usize, generation: u32) -> Option<&Node> {
        self.nodes.get(id)?.as_ref().filter(|n| n.generation == generation)
    }

    fn track(&mut self, source: usize) {
        if let Some(observer) = self.observer {
            if let Some(node) = self.nodes[observer].as_mut() {
                if !node.sources.contains(&source) {
                    node.sources.push(source);
                    if let Some(src) = self.nodes[source].as_mut() {
                        src.subscribers.push(observer);
                    }
                }
            }
        }
    }

    /// The direct readers of `id` are dirty; everything below them is "check".
    fn mark_subscribers(&mut self, id: usize) {
        let subscribers = match self.nodes[id].as_ref() {
            Some(node) => node.subscribers.clone(),
            None => return,
        };
        for sub in subscribers {
            self.mark(sub, State::Dirty);
        }
    }

    fn mark(&mut self, id: usize, state: State) {
        let Some(node) = self.nodes[id].as_mut() else { return };
        if node.state == State::Clean {
            if matches!(node.kind, Kind::Effect(_)) {
                self.pending.push(id);
            }
            let subscribers = node.subscribers.clone();
            node.state = state;
            for sub in subscribers {
                self.mark(sub, State::Check);
            }
        } else if state == State::Dirty {
            node.state = State::Dirty;
        }
    }

    fn unsubscribe(&mut self, id: usize) {
        let sources = match self.nodes[id].as_mut() {
            Some(node) => std::mem::take(&mut node.sources),
            None => return,
        };
        for src in sources {
            if let Some(node) = self.nodes[src].as_mut() {
                node.subscribers.retain(|&s| s != id);
            }
        }
    }

    /// Disposes what `id` owns and runs its cleanups, leaving `id` itself.
    fn reset(&mut self, id: usize) -> Vec<Box<dyn FnOnce()>> {
        let (owned, cleanups) = match self.nodes[id].as_mut() {
            Some(node) => (std::mem::take(&mut node.owned), std::mem::take(&mut node.cleanups)),
            None => return Vec::new(),
        };
        let mut all = cleanups;
        for child in owned {
            all.extend(self.dispose(child));
        }
        all
    }

    /// Removes `id` and everything it owns. Returns the cleanups to run once
    /// the runtime is no longer borrowed.
    fn dispose(&mut self, id: usize) -> Vec<Box<dyn FnOnce()>> {
        let mut cleanups = self.reset(id);
        self.unsubscribe(id);
        if let Some(node) = self.nodes[id].take() {
            for sub in node.subscribers {
                if let Some(s) = self.nodes[sub].as_mut() {
                    s.sources.retain(|&x| x != id);
                }
            }
            if let Some(owner) = node.owner {
                if let Some(o) = self.nodes[owner].as_mut() {
                    o.owned.retain(|&x| x != id);
                }
            }
            cleanups.extend(node.cleanups);
            self.generations[id] = self.generations[id].wrapping_add(1);
            self.free.push(id);
        }
        cleanups
    }
}

fn run_cleanups(cleanups: Vec<Box<dyn FnOnce()>>) {
    for cleanup in cleanups {
        cleanup();
    }
}

/// Brings `id` up to date: a "check" node asks its sources first and
/// recomputes only if one of them changed.
fn update(id: usize) {
    let (state, sources) = match with(|rt| rt.nodes[id].as_ref().map(|n| (n.state, n.sources.clone()))) {
        Some(v) => v,
        None => return,
    };
    if state == State::Check {
        for src in sources {
            update(src);
            if with(|rt| rt.nodes[id].as_ref().map(|n| n.state)) == Some(State::Dirty) {
                break;
            }
        }
    }
    let state = with(|rt| rt.nodes[id].as_ref().map(|n| n.state));
    if state == Some(State::Dirty) {
        run(id);
    }
    with(|rt| {
        if let Some(node) = rt.nodes[id].as_mut() {
            node.state = State::Clean;
        }
    });
}

/// Runs a memo or an effect with itself as the observer.
fn run(id: usize) {
    enum Job {
        Memo(Compute, Option<Box<dyn Any>>),
        Effect(EffectFn),
        None,
    }
    let (job, cleanups) = with(|rt| {
        let cleanups = rt.reset(id);
        rt.unsubscribe(id);
        let job = match rt.nodes[id].as_mut() {
            Some(node) => match &node.kind {
                Kind::Memo(f) => Job::Memo(f.clone(), node.value.take()),
                Kind::Effect(f) => Job::Effect(f.clone()),
                _ => Job::None,
            },
            None => Job::None,
        };
        (job, cleanups)
    });
    run_cleanups(cleanups);

    let previous = with(|rt| (rt.observer.replace(id), rt.owner.replace(id)));
    match job {
        Job::Memo(compute, old) => {
            let new = compute(old.as_deref());
            with(|rt| {
                let changed = new.is_some();
                if let Some(node) = rt.nodes[id].as_mut() {
                    node.value = new.or(old);
                }
                if changed {
                    rt.mark_subscribers(id);
                }
            });
        }
        Job::Effect(f) => (f.borrow_mut())(),
        Job::None => {}
    }
    with(|rt| (rt.observer, rt.owner) = previous);
}

/// Runs every effect a change reached. Called at the end of the outermost
/// `set` or `batch`; the event loop never has to.
fn flush() {
    if with(|rt| rt.batch > 0 || std::mem::replace(&mut rt.flushing, true)) {
        return;
    }
    loop {
        let pending = with(|rt| std::mem::take(&mut rt.pending));
        if pending.is_empty() {
            break;
        }
        for id in pending {
            update(id);
        }
    }
    with(|rt| rt.flushing = false);
}

/// A value set from outside the graph.
pub struct Signal<T> {
    id: usize,
    generation: u32,
    marker: PhantomData<*const T>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Signal<T> {}

impl<T: Clone + PartialEq + 'static> Signal<T> {
    pub fn new(value: T) -> Signal<T> {
        let (id, generation) =
            with(|rt| rt.insert(Kind::Signal, Some(Box::new(value)), State::Clean));
        Signal { id, generation, marker: PhantomData }
    }

    /// The value, recorded as a dependency of the running memo or effect.
    /// `None` once the signal has been disposed.
    pub fn try_get(&self) -> Option<T> {
        with(|rt| {
            let value = rt.node(self.id, self.generation)?.value.as_ref()?;
            let value = value.downcast_ref::<T>()?.clone();
            rt.track(self.id);
            Some(value)
        })
    }

    pub fn get(&self) -> T {
        self.try_get().expect("signal read after disposal")
    }

    /// The value, without becoming a dependency.
    pub fn get_untracked(&self) -> Option<T> {
        with(|rt| rt.node(self.id, self.generation)?.value.as_ref()?.downcast_ref::<T>().cloned())
    }

    /// Sets the value. An equal value changes nothing and wakes nobody.
    pub fn set(&self, value: T) {
        let changed = with(|rt| {
            let generation = self.generation;
            let Some(node) = rt.nodes.get_mut(self.id).and_then(|n| n.as_mut()) else { return false };
            if node.generation != generation {
                return false;
            }
            if node.value.as_ref().and_then(|v| v.downcast_ref::<T>()) == Some(&value) {
                return false;
            }
            node.value = Some(Box::new(value));
            rt.mark_subscribers(self.id);
            true
        });
        if changed {
            flush();
        }
    }

    pub fn update(&self, f: impl FnOnce(&T) -> T) {
        if let Some(old) = self.get_untracked() {
            self.set(f(&old));
        }
    }

    /// How many memos and effects read it now: an expensive source can stop
    /// when this falls to zero.
    pub fn subscriber_count(&self) -> usize {
        with(|rt| rt.node(self.id, self.generation).map_or(0, |n| n.subscribers.len()))
    }
}

/// A value derived from others, recomputed only when one of them changed,
/// and passing the change on only when the result differs.
pub struct Memo<T> {
    id: usize,
    generation: u32,
    marker: PhantomData<*const T>,
}

impl<T> Clone for Memo<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Memo<T> {}

impl<T: Clone + PartialEq + 'static> Memo<T> {
    pub fn new(f: impl Fn() -> T + 'static) -> Memo<T> {
        let compute: Compute = Rc::new(move |old: Option<&dyn Any>| {
            let new = f();
            match old.and_then(|o| o.downcast_ref::<T>()) {
                Some(old) if *old == new => None,
                _ => Some(Box::new(new) as Box<dyn Any>),
            }
        });
        let (id, generation) = with(|rt| rt.insert(Kind::Memo(compute), None, State::Dirty));
        Memo { id, generation, marker: PhantomData }
    }

    pub fn try_get(&self) -> Option<T> {
        with(|rt| rt.node(self.id, self.generation).map(|_| ()))?;
        update(self.id);
        with(|rt| {
            let value = rt.node(self.id, self.generation)?.value.as_ref()?.downcast_ref::<T>()?.clone();
            rt.track(self.id);
            Some(value)
        })
    }

    pub fn get(&self) -> T {
        self.try_get().expect("memo read after disposal")
    }
}

/// A function run for its effect, again whenever something it read changes.
/// It runs once at creation.
#[derive(Clone, Copy)]
pub struct Effect {
    id: usize,
    generation: u32,
}

impl Effect {
    pub fn new(f: impl FnMut() + 'static) -> Effect {
        let f: EffectFn = Rc::new(RefCell::new(f));
        let (id, generation) = with(|rt| rt.insert(Kind::Effect(f), None, State::Dirty));
        update(id);
        Effect { id, generation }
    }

    pub fn dispose(self) {
        let cleanups = with(|rt| match rt.node(self.id, self.generation) {
            Some(_) => rt.dispose(self.id),
            None => Vec::new(),
        });
        run_cleanups(cleanups);
    }
}

/// An owner for nodes created inside it, disposed together: an interface
/// tree's bindings, for example.
pub struct Scope {
    id: usize,
    generation: u32,
}

impl Scope {
    /// Runs `f` with the new scope as the owner of what it creates.
    pub fn new<R>(f: impl FnOnce() -> R) -> (Scope, R) {
        let (id, generation) = with(|rt| rt.insert(Kind::Scope, None, State::Clean));
        let previous = with(|rt| rt.owner.replace(id));
        let result = untracked(f);
        with(|rt| rt.owner = previous);
        (Scope { id, generation }, result)
    }

    pub fn dispose(self) {
        let cleanups = with(|rt| match rt.node(self.id, self.generation) {
            Some(_) => rt.dispose(self.id),
            None => Vec::new(),
        });
        run_cleanups(cleanups);
    }
}

/// Runs `f` when the current effect re-runs or its owner is disposed.
pub fn on_cleanup(f: impl FnOnce() + 'static) {
    with(|rt| {
        if let Some(owner) = rt.owner {
            if let Some(node) = rt.nodes[owner].as_mut() {
                node.cleanups.push(Box::new(f));
            }
        }
    });
}

/// Several changes, one wave of effects.
pub fn batch<R>(f: impl FnOnce() -> R) -> R {
    with(|rt| rt.batch += 1);
    let result = f();
    with(|rt| rt.batch -= 1);
    flush();
    result
}

/// Runs `f` without recording what it reads.
pub fn untracked<R>(f: impl FnOnce() -> R) -> R {
    let previous = with(|rt| rt.observer.take());
    let result = f();
    with(|rt| rt.observer = previous);
    result
}

/// Live nodes, for tests and for leak checks after a reload.
pub fn node_count() -> usize {
    with(|rt| rt.nodes.iter().filter(|n| n.is_some()).count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn an_effect_runs_once_then_on_each_change() {
        let a = Signal::new(1);
        let runs = Rc::new(Cell::new(0));
        let seen = Rc::new(Cell::new(0));
        let (r, s) = (runs.clone(), seen.clone());
        Effect::new(move || {
            r.set(r.get() + 1);
            s.set(a.get());
        });
        assert_eq!((runs.get(), seen.get()), (1, 1));
        a.set(2);
        assert_eq!((runs.get(), seen.get()), (2, 2));
        a.set(2);
        assert_eq!(runs.get(), 2, "an equal value wakes nobody");
    }

    #[test]
    fn a_memo_that_does_not_change_stops_the_wave() {
        let load = Signal::new(0.10);
        let level = Memo::new(move || if load.get() < 0.3 { "calm" } else { "active" });
        let runs = Rc::new(Cell::new(0));
        let r = runs.clone();
        Effect::new(move || {
            level.get();
            r.set(r.get() + 1);
        });
        load.set(0.12);
        load.set(0.20);
        assert_eq!(runs.get(), 1, "still calm: the effect does not run");
        load.set(0.50);
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn dependencies_follow_the_branch_taken() {
        let flag = Signal::new(true);
        let a = Signal::new(1);
        let b = Signal::new(10);
        let runs = Rc::new(Cell::new(0));
        let r = runs.clone();
        Effect::new(move || {
            r.set(r.get() + 1);
            if flag.get() { a.get() } else { b.get() };
        });
        b.set(11);
        assert_eq!(runs.get(), 1, "b was not read");
        flag.set(false);
        a.set(2);
        assert_eq!(runs.get(), 2, "a is no longer read");
        b.set(12);
        assert_eq!(runs.get(), 3);
    }

    #[test]
    fn a_batch_runs_effects_once() {
        let a = Signal::new(1);
        let b = Signal::new(1);
        let runs = Rc::new(Cell::new(0));
        let r = runs.clone();
        Effect::new(move || {
            a.get();
            b.get();
            r.set(r.get() + 1);
        });
        batch(|| {
            a.set(2);
            b.set(2);
        });
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn disposing_a_scope_frees_its_bindings_and_runs_cleanups() {
        let before = node_count();
        let a = Signal::new(1);
        let cleaned = Rc::new(Cell::new(false));
        let runs = Rc::new(Cell::new(0));
        let (c, r) = (cleaned.clone(), runs.clone());
        let (scope, ()) = Scope::new(move || {
            let doubled = Memo::new(move || a.get() * 2);
            Effect::new(move || {
                doubled.get();
                r.set(r.get() + 1);
            });
            on_cleanup(move || c.set(true));
        });
        assert_eq!(a.subscriber_count(), 1);
        scope.dispose();
        assert!(cleaned.get());
        assert_eq!(a.subscriber_count(), 0);
        a.set(5);
        assert_eq!(runs.get(), 1, "a disposed effect never runs again");
        assert_eq!(node_count(), before + 1, "only the signal is left");
    }

    #[test]
    fn nodes_created_by_an_effect_go_with_its_next_run() {
        let a = Signal::new(0);
        let (scope, ()) = Scope::new(move || {
            Effect::new(move || {
                a.get();
                Memo::new(|| 1);
            });
        });
        let count = node_count();
        for i in 1..50 {
            a.set(i);
        }
        assert_eq!(node_count(), count, "each run replaces its inner memo");
        scope.dispose();
    }
}
