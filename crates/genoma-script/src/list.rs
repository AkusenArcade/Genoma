//! Keyed rows. `g.each` shows a row for every item; `g.list` shows only
//! the rows in view of a list that scrolls by itself, so a list of
//! thousands costs what its window costs.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use calloop::RegistrationToken;
use genoma_core::reactive::{Effect, Scope, Signal, batch, detached, on_cleanup, untracked};
use genoma_scene::{NodeId, Prop, Scene, Value};
use mlua::{Function, Table, Value as LuaValue};

use crate::{Handlers, err, io::HostRef, mount};

thread_local! {
    /// The loop a list's scrolling steps on: set when an interface loads.
    static HOST: RefCell<Option<HostRef>> = const { RefCell::new(None) };
}

pub(crate) fn set_host(host: HostRef) {
    HOST.with(|h| *h.borrow_mut() = Some(host));
}

/// One row: what it was mounted from, kept while its key is shown.
struct Row {
    key: String,
    root: NodeId,
    scope: Scope,
    item: Signal<LuaValue>,
    index: Signal<f64>,
}

/// The rows a keyed list shows now.
struct Rows {
    what: &'static str,
    scene: Rc<RefCell<Scene>>,
    handlers: Handlers,
    target: usize,
    node: NodeId,
    row: Function,
    /// Every row this tall, or as it lays itself out.
    height: Option<f32>,
    shown: RefCell<Vec<Row>>,
}

impl Rows {
    /// Rows for `wanted` (key, item, place from 1), in that order. A key
    /// shown already keeps its row, which is handed the new item and place
    /// through `item()` and `index()`, so its bindings follow and nothing
    /// is mounted again; a new key mounts a row; a key no longer wanted
    /// takes its row, its bindings and its handlers with it. Returns the
    /// rows' roots in order.
    fn show(&self, wanted: Vec<(String, LuaValue, f64)>) -> Vec<NodeId> {
        let mut old = std::mem::take(&mut *self.shown.borrow_mut());
        let mut next = Vec::with_capacity(wanted.len());
        for (key, value, index) in wanted {
            if let Some(at) = old.iter().position(|r| r.key == key) {
                let r = old.swap_remove(at);
                r.item.set(value);
                r.index.set(index);
                next.push(r);
                continue;
            }
            // Detached from the list's effect, or each run would dispose
            // the rows; the list's own cleanup disposes them instead.
            let (scope, mounted) = detached(|| {
                Scope::new(|| -> mlua::Result<_> {
                    let item = Signal::new(value);
                    let index = Signal::new(index);
                    let lua = self.row.weak_lua().upgrade();
                    let read_item = lua.create_function(move |_, ()| Ok(item.try_get().unwrap_or(LuaValue::Nil)))?;
                    let read_index = lua.create_function(move |_, ()| Ok(index.try_get().unwrap_or(0.0)))?;
                    let t: Table = self.row.call((read_item, read_index))?;
                    Ok((mount(&self.scene, &self.handlers, self.target, &t, self.node)?, item, index))
                })
            });
            match mounted {
                Ok((root, item, index)) => {
                    if let Some(h) = self.height {
                        let _ = self.scene.borrow_mut().set(root, Prop::Height, Value::Number(h), None);
                    }
                    next.push(Row { key, root, scope, item, index });
                }
                Err(e) => {
                    log::error!("{} row {key}: {e}", self.what);
                    scope.dispose();
                }
            }
        }
        for r in old {
            r.scope.dispose();
            let gone = self.scene.borrow().subtree(r.root);
            self.handlers.borrow_mut().retain(|(t, n), _| *t != self.target || !gone.contains(n));
            self.scene.borrow_mut().remove(r.root);
        }
        let order = next.iter().map(|r| r.root).collect();
        *self.shown.borrow_mut() = next;
        order
    }

    fn dispose(&self) {
        for r in self.shown.borrow_mut().drain(..) {
            r.scope.dispose();
        }
    }

    /// `(key, item, place)` for the item at `index` (from 0), or why not.
    fn wanted(&self, key: &Function, value: LuaValue, index: usize, taken: &[(String, LuaValue, f64)]) -> Option<(String, LuaValue, f64)> {
        let k = match key.call::<LuaValue>(value.clone()).and_then(|k| k.to_string()) {
            Ok(k) => k,
            Err(e) => {
                log::error!("{} key: {e}", self.what);
                return None;
            }
        };
        if taken.iter().any(|(t, ..)| *t == k) {
            log::error!("{}: key {k} twice; the second is not drawn", self.what);
            return None;
        }
        Some((k, value, (index + 1) as f64))
    }
}

fn rows(what: &'static str, scene: Rc<RefCell<Scene>>, handlers: Handlers, target: usize, node: NodeId, row: Function, height: Option<f32>) -> Rc<Rows> {
    Rc::new(Rows { what, scene, handlers, target, node, row, height, shown: RefCell::default() })
}

/// `g.each`: a row per item, by key.
pub(crate) fn each(scene: Rc<RefCell<Scene>>, handlers: Handlers, target: usize, node: NodeId, items: Function, key: Function, row: Function) {
    let rows = rows("g.each", scene, handlers, target, node, row, None);
    on_cleanup({
        let rows = rows.clone();
        move || rows.dispose()
    });
    Effect::new(move || {
        let list = match items.call::<Table>(()) {
            Ok(list) => list,
            Err(e) => {
                log::error!("g.each items: {e}");
                return;
            }
        };
        let values: Vec<LuaValue> = list.sequence_values::<LuaValue>().filter_map(Result::ok).collect();
        untracked(|| {
            let mut wanted = Vec::with_capacity(values.len());
            for (i, value) in values.into_iter().enumerate() {
                if let Some(w) = rows.wanted(&key, value, i, &wanted) {
                    wanted.push(w);
                }
            }
            let order = rows.show(wanted);
            rows.scene.borrow_mut().set_children(node, &order);
        });
    });
}

// ---- g.list's scrolling ------------------------------------------------------

/// The step between two positions while moving: about a frame at 120 Hz.
const TICK: Duration = Duration::from_millis(8);
/// Rows kept beyond each edge of the view, so a row is there as it enters.
const OVERSCAN: usize = 3;
/// How fast a wheel's step is caught up with: the seconds to cover 63% of
/// what is left.
const CATCH_UP: f64 = 0.05;
/// How fast a fling slows, the same way: it carries on about its speed
/// times this far.
const GLIDE: f64 = 0.325;
/// A fling slower than this, logical units a second, has stopped.
const STILL: f64 = 20.0;
/// The view's height before the first layout says it: a screen's worth of
/// rows is mounted at once rather than a few, then the rest a frame later.
const FIRST_VIEW: f64 = 1080.0;

/// Where a list is scrolled to and where it is going.
struct Scroll {
    /// Shown: what the window of rows follows.
    offset: Signal<f64>,
    /// The height the rows have to be seen in.
    view: Signal<f64>,
    /// Where a step or a reveal is going.
    target: Cell<f64>,
    /// A fling's speed, logical units a second; 0 when none.
    velocity: Cell<f64>,
    /// How far it can go: the rows' height less the view's.
    room: Cell<f64>,
    /// A row and the gap after it.
    pitch: f64,
    last: Cell<Instant>,
    ticking: Cell<Option<RegistrationToken>>,
    host: Option<HostRef>,
}

impl Scroll {
    fn clamp(&self, y: f64) -> f64 {
        y.clamp(0.0, self.room.get())
    }

    /// Where it is now, within what the content allows now.
    fn now(&self) -> f64 {
        self.clamp(self.offset.get_untracked().unwrap_or(0.0))
    }

    /// Followed at once: a touchpad's movement.
    fn jump(self: &Rc<Self>, y: f64) {
        self.stop();
        self.velocity.set(0.0);
        let y = self.clamp(y);
        self.target.set(y);
        if self.offset.get_untracked() != Some(y) {
            self.offset.set(y);
        }
    }

    /// Caught up with over a few frames: a wheel's step, a row revealed.
    fn glide(self: &Rc<Self>, y: f64) {
        self.velocity.set(0.0);
        self.target.set(self.clamp(y));
        self.start();
    }

    /// Carried on at `velocity`, slowing: the fingers left the touchpad.
    fn fling(self: &Rc<Self>, velocity: f64) {
        self.velocity.set(velocity);
        self.start();
    }

    fn start(self: &Rc<Self>) {
        if self.ticking.get().is_some() {
            return;
        }
        let Some(host) = &self.host else {
            // No loop to step on (a test): straight there.
            let to = self.target.get();
            self.offset.set(to);
            return;
        };
        self.last.set(Instant::now());
        let me = Rc::downgrade(self);
        let token = host.timer(
            TICK,
            Box::new(move || {
                let me = me.upgrade()?;
                if me.step() {
                    Some(TICK)
                } else {
                    me.ticking.set(None);
                    None
                }
            }),
        );
        self.ticking.set(token);
    }

    fn stop(&self) {
        if let (Some(token), Some(host)) = (self.ticking.take(), &self.host) {
            host.remove(token);
        }
    }

    /// One step; false once it rests.
    fn step(&self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last.get()).as_secs_f64();
        self.last.set(now);
        let at = self.now();
        let velocity = self.velocity.get();
        let (next, rests) = if velocity != 0.0 {
            let y = self.clamp(at + velocity * dt);
            let velocity = velocity * (-dt / GLIDE).exp();
            let stopped = velocity.abs() < STILL || y <= 0.0 || y >= self.room.get();
            self.velocity.set(if stopped { 0.0 } else { velocity });
            self.target.set(y);
            (y, stopped)
        } else {
            let to = self.clamp(self.target.get());
            let y = at + (to - at) * (1.0 - (-dt / CATCH_UP).exp());
            if (to - y).abs() < 0.5 { (to, true) } else { (y, false) }
        };
        if next != at {
            batch(|| self.offset.set(next));
        }
        !rests
    }
}

/// `g.list`: the rows of `items` in view, every one `row_height` tall.
/// The wheel steps through it, a touchpad moves it and flings it, and
/// `reveal` (a row's place, from 1) brings a row into view.
pub(crate) fn list(scene: Rc<RefCell<Scene>>, handlers: Handlers, target: usize, node: NodeId, t: &Table) -> mlua::Result<()> {
    let want = |name: &str| -> mlua::Result<Function> {
        t.get::<Option<Function>>(name)?.ok_or_else(|| err(format!("g.list needs {name}, a function")))
    };
    let (items, key, row) = (want("items")?, want("key")?, want("row")?);
    let row_height = t
        .get::<Option<f64>>("row_height")?
        .filter(|h| *h > 0.0)
        .ok_or_else(|| err("g.list needs row_height, above 0: every row is that tall"))?;
    let reveal: Option<Function> = t.get("reveal")?;
    let on_scroll: Option<Function> = t.get("on_scroll")?;
    let gap = scene.borrow().gap(node) as f64;

    let rows = rows("g.list", scene.clone(), handlers.clone(), target, node, row, Some(row_height as f32));
    let scroll = Rc::new(Scroll {
        offset: Signal::new(0.0),
        view: Signal::new(FIRST_VIEW),
        target: Cell::new(0.0),
        velocity: Cell::new(0.0),
        room: Cell::new(0.0),
        pitch: row_height + gap,
        last: Cell::new(Instant::now()),
        ticking: Cell::new(None),
        host: HOST.with(|h| h.borrow().clone()),
    });
    on_cleanup({
        let (rows, scroll) = (rows.clone(), scroll.clone());
        move || {
            scroll.stop();
            rows.dispose();
        }
    });

    // Its own wheel, fling and size, the size after any on_size of the
    // interface's.
    let lua = items.weak_lua().upgrade();
    let s = Rc::downgrade(&scroll);
    let on_wheel = lua.create_function(move |_, (_dx, dy, steps): (f64, f64, Option<bool>)| {
        let Some(s) = s.upgrade() else { return Ok(()) };
        if steps.unwrap_or(false) {
            // A notch is three rows, never more than half the view.
            let view = s.view.get_untracked().unwrap_or(0.0);
            let notch = (3.0 * s.pitch).min(view * 0.5).max(s.pitch);
            let from = if s.ticking.get().is_some() { s.target.get() } else { s.now() };
            s.glide(from + dy * notch);
        } else {
            s.jump(s.now() + dy);
        }
        Ok(())
    })?;
    let s = Rc::downgrade(&scroll);
    let on_fling = lua.create_function(move |_, (_dx, dy): (f64, f64)| {
        if let Some(s) = s.upgrade() {
            s.fling(dy);
        }
        Ok(())
    })?;
    let theirs = handlers.borrow().get(&(target, node)).and_then(|h| h.get("on_size")).cloned();
    let (s, sc) = (Rc::downgrade(&scroll), scene.clone());
    let on_size = lua.create_function(move |_, (w, h): (f64, f64)| {
        if let Some(s) = s.upgrade() {
            let view = (h - sc.borrow().inset_y(node) as f64).max(0.0);
            if s.view.get_untracked() != Some(view) {
                s.view.set(view);
            }
        }
        match &theirs {
            Some(f) => f.call::<()>((w, h)),
            None => Ok(()),
        }
    })?;
    {
        let mut handlers = handlers.borrow_mut();
        let mine = handlers.entry((target, node)).or_default();
        mine.insert("on_wheel", on_wheel);
        mine.insert("on_fling", on_fling);
        mine.insert("on_size", on_size);
    }
    {
        let mut scene = scene.borrow_mut();
        scene.watch_size(node);
        scene.set_interactive(node, true);
        scene.mark_list(node);
    }

    // The window: the rows in view and a few beyond, the first of them
    // drawn where the offset puts it. Scrolled relative to that row, so a
    // list of a million rows keeps whole-pixel positions in f32.
    let told = Cell::new((f64::NAN, f64::NAN));
    let s = scroll.clone();
    Effect::new(move || {
        let list = match items.call::<Table>(()) {
            Ok(list) => list,
            Err(e) => {
                log::error!("g.list items: {e}");
                return;
            }
        };
        let count = list.raw_len();
        let view = s.view.get();
        let offset = s.offset.get();
        untracked(|| {
            let room = (count as f64 * s.pitch - gap - view).max(0.0);
            s.room.set(room);
            // Content that shrank under the scroll brings it back in reach.
            let offset = offset.min(room);
            if s.target.get() > room {
                s.target.set(room);
            }
            let first = ((offset / s.pitch).floor() as usize).saturating_sub(OVERSCAN).min(count);
            let last = (((offset + view) / s.pitch).ceil() as usize + OVERSCAN).min(count);
            let mut wanted = Vec::with_capacity(last - first);
            for i in first..last {
                let value = list.raw_get::<LuaValue>(i + 1).unwrap_or(LuaValue::Nil);
                if let Some(w) = rows.wanted(&key, value, i, &wanted) {
                    wanted.push(w);
                }
            }
            let order = rows.show(wanted);
            {
                let mut scene = rows.scene.borrow_mut();
                scene.set_children(node, &order);
                let _ = scene.set(node, Prop::ScrollY, Value::Number((offset - first as f64 * s.pitch) as f32), None);
            }
            if let Some(f) = &on_scroll
                && told.get() != (offset, room)
            {
                told.set((offset, room));
                if let Err(e) = f.call::<()>((offset, room)) {
                    log::error!("g.list on_scroll: {e}");
                }
            }
        });
    });

    // reveal: the row at that place brought into view, the least way, and
    // kept there when the view changes height.
    if let Some(reveal) = reveal {
        let s = scroll.clone();
        Effect::new(move || {
            let at = match reveal.call::<Option<f64>>(()) {
                Ok(at) => at,
                Err(e) => {
                    log::error!("g.list reveal: {e}");
                    return;
                }
            };
            let view = s.view.get();
            let Some(at) = at else { return };
            untracked(|| {
                let top = (at - 1.0).max(0.0) * s.pitch;
                let bottom = top + row_height;
                let now = s.target.get();
                if top < now {
                    s.glide(top);
                } else if bottom > now + view {
                    s.glide(bottom - view);
                }
            });
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::{Arc, Mutex}};

    use genoma_core::reactive::Scope;
    use mlua::Lua;

    use super::*;

    /// A list of `count` rows 20 tall in a well 200 tall, mounted and told
    /// its size as a layout would.
    fn mounted(lua: &Lua, count: usize, extra: &str) -> (Rc<RefCell<Scene>>, Handlers, NodeId, Scope) {
        let fonts = Arc::new(Mutex::new(genoma_render::cosmic_text::FontSystem::new_with_fonts(std::iter::empty())));
        let scene = Rc::new(RefCell::new(Scene::new(fonts)));
        let handlers: Handlers = Rc::new(RefCell::new(HashMap::new()));
        let source = format!(
            r#"local data = {{}}
            for i = 1, {count} do data[i] = {{ id = i }} end
            return {{
                __kind = "list", height = 200, row_height = 20, {extra}
                items = function() return data end,
                key = function(item) return item.id end,
                row = function(item) return {{ __kind = "box", id = "row-" .. item().id }} end,
            }}"#
        );
        let t: Table = lua.load(&source).eval().unwrap();
        let root = scene.borrow().root();
        let (scope, node) = Scope::new(|| mount(&scene, &handlers, 0, &t, root).unwrap());
        scene.borrow_mut().layout(Some(300.0), Some(200.0));
        let sized = scene.borrow_mut().take_sized();
        for (n, w, h) in sized {
            call(&handlers, n, "on_size", (w as f64, h as f64));
        }
        (scene, handlers, node, scope)
    }

    fn call(handlers: &Handlers, node: NodeId, name: &str, args: impl mlua::IntoLuaMulti) {
        let f = handlers.borrow().get(&(0, node)).and_then(|h| h.get(name)).cloned().expect(name);
        f.call::<()>(args).unwrap();
    }

    fn shows(scene: &Rc<RefCell<Scene>>, id: usize) -> bool {
        scene.borrow().find(&format!("row-{id}")).is_some()
    }

    fn scroll_y(scene: &Rc<RefCell<Scene>>, node: NodeId) -> f32 {
        match scene.borrow().value(node, Prop::ScrollY) {
            Some(Value::Number(y)) => y,
            other => panic!("scroll_y: {other:?}"),
        }
    }

    #[test]
    fn each_keeps_a_row_while_its_key_stays() {
        let lua = Lua::new();
        let fonts = Arc::new(Mutex::new(genoma_render::cosmic_text::FontSystem::new_with_fonts(std::iter::empty())));
        let scene = Rc::new(RefCell::new(Scene::new(fonts)));
        let handlers: Handlers = Rc::new(RefCell::new(HashMap::new()));
        // Items 1 to `count`, newest first: what a notification list does.
        let count = Signal::new(5usize);
        let c = count;
        let items = lua
            .create_function(move |lua, ()| {
                let t = lua.create_table()?;
                for i in (1..=c.get()).rev() {
                    t.raw_push(i)?;
                }
                Ok(t)
            })
            .unwrap();
        lua.globals().set("items", items).unwrap();
        let t: Table = lua
            .load(r#"return { __kind = "each", items = items, key = function(i) return i end,
                row = function(i) return { __kind = "box", id = "row-" .. i() } end }"#)
            .eval()
            .unwrap();
        let root = scene.borrow().root();
        let (_scope, node) = Scope::new(|| mount(&scene, &handlers, 0, &t, root).unwrap());
        let find = |i: usize| scene.borrow().find(&format!("row-{i}"));
        let three = find(3).expect("row 3");
        assert_eq!(scene.borrow().subtree(node).len(), 1 + 5);
        count.set(7);
        assert_eq!(find(3), Some(three), "kept, not mounted again");
        assert!(find(7).is_some());
        count.set(2);
        assert!(find(3).is_none() && find(2).is_some());
        assert_eq!(scene.borrow().subtree(node).len(), 1 + 2);
    }

    #[test]
    fn a_list_mounts_only_the_rows_in_view() {
        let lua = Lua::new();
        let (scene, _, node, _scope) = mounted(&lua, 10_000, "");
        // Ten rows in view and three beyond.
        assert!(shows(&scene, 1) && shows(&scene, 13));
        assert!(!shows(&scene, 14));
        assert_eq!(scene.borrow().subtree(node).len(), 1 + 13);
    }

    #[test]
    fn a_touchpad_moves_the_window_and_the_scroll_stays_small() {
        let lua = Lua::new();
        let (scene, handlers, node, _scope) = mounted(&lua, 10_000, "");
        call(&handlers, node, "on_wheel", (0.0, 500.0, false));
        // At 500, rows 26 to 35 are in view: 23 to 38 are mounted, the first
        // of them drawn from 60 above the top.
        assert!(!shows(&scene, 22) && shows(&scene, 23) && shows(&scene, 38) && !shows(&scene, 39));
        assert_eq!(scroll_y(&scene, node), 60.0);
        // Never past the last row.
        call(&handlers, node, "on_wheel", (0.0, 1e9, false));
        assert!(shows(&scene, 10_000));
        assert_eq!(scroll_y(&scene, node), (200_000.0 - 200.0 - (10_000.0 - 13.0) * 20.0) as f32);
        call(&handlers, node, "on_wheel", (0.0, -1e9, false));
        assert!(shows(&scene, 1));
        assert_eq!(scroll_y(&scene, node), 0.0);
    }

    #[test]
    fn a_wheel_notch_is_three_rows() {
        let lua = Lua::new();
        let (scene, handlers, node, _scope) = mounted(&lua, 100, "");
        call(&handlers, node, "on_wheel", (0.0, 2.0, true));
        // Without a loop to glide on, it is there at once: 120.
        assert!(!shows(&scene, 3) && shows(&scene, 4));
        assert_eq!(scroll_y(&scene, node), 120.0 - 3.0 * 20.0);
    }

    #[test]
    fn reveal_brings_a_row_into_view_the_least_way() {
        let lua = Lua::new();
        let (scene, _, node, _scope) = mounted(&lua, 1000, "reveal = function() return 500 end,");
        // Row 500 ends at 10000: scrolled to 9800, it is the last in view.
        assert!(shows(&scene, 500) && shows(&scene, 491));
        assert_eq!(scroll_y(&scene, node), (9800.0 - (490.0 - 3.0) * 20.0) as f32);
    }

    #[test]
    fn a_list_wants_a_row_height_and_its_own_wheel() {
        let lua = Lua::new();
        let fonts = Arc::new(Mutex::new(genoma_render::cosmic_text::FontSystem::new_with_fonts(std::iter::empty())));
        let scene = Rc::new(RefCell::new(Scene::new(fonts)));
        let handlers: Handlers = Rc::new(RefCell::new(HashMap::new()));
        let root = scene.borrow().root();
        for source in [
            r#"return { __kind = "list", items = function() return {} end, key = tostring, row = function() return { __kind = "box" } end }"#,
            r#"return { __kind = "list", row_height = 20, on_wheel = function() end, items = function() return {} end, key = tostring, row = function() return { __kind = "box" } end }"#,
        ] {
            let t: Table = lua.load(source).eval().unwrap();
            let (_scope, mounted) = Scope::new(|| mount(&scene, &handlers, 0, &t, root));
            assert!(mounted.is_err(), "{source}");
        }
    }
}
