//! `scroll = "x" | "y" | "both"` on any node: Genoma moves its children
//! itself, as `g.list` moves its rows — the wheel in steps caught up with
//! over a few frames, a touchpad followed and flung, `snap` settling on a
//! child. A scroller takes only its own axes, and what it cannot use goes
//! on to the scroller around it: its `on_wheel` and `on_fling` answer
//! `false`, and the event climbs.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use calloop::RegistrationToken;
use genoma_scene::{NodeId, Prop, Scene, ScrollInfo, Value};
use mlua::{Function, Lua, Value as LuaValue};

use crate::{
    Handlers,
    io::HostRef,
    list::{CATCH_UP, GLIDE, STILL, TICK},
};

/// How long a touchpad rests before a snapping scroller settles: the
/// fingers stopped without a fling.
const SETTLE: Duration = Duration::from_millis(140);

struct Scroller {
    scene: Rc<RefCell<Scene>>,
    node: NodeId,
    /// Where a step, a snap or a fling is going, per axis.
    target: Cell<[f64; 2]>,
    /// A fling's speed, logical units a second; 0 when none.
    velocity: Cell<[f64; 2]>,
    last: Cell<Instant>,
    ticking: Cell<Option<RegistrationToken>>,
    settling: Cell<Option<RegistrationToken>>,
    host: Option<HostRef>,
    on_scroll: Option<Function>,
    told: Cell<[f64; 2]>,
}

impl Scroller {
    fn info(&self) -> Option<ScrollInfo> {
        self.scene.borrow().scroll_info(self.node)
    }

    fn clamp(info: &ScrollInfo, p: [f64; 2]) -> [f64; 2] {
        [0, 1].map(|a| if info.axes[a] { p[a].clamp(0.0, info.room[a] as f64) } else { 0.0 })
    }

    /// Where it is, within what its content allows now.
    fn now(&self, info: &ScrollInfo) -> [f64; 2] {
        let scene = self.scene.borrow();
        let read = |prop| match scene.value(self.node, prop) {
            Some(Value::Number(v)) => v as f64,
            _ => 0.0,
        };
        Self::clamp(info, [read(Prop::ScrollX), read(Prop::ScrollY)])
    }

    fn put(&self, p: [f64; 2]) {
        {
            let mut scene = self.scene.borrow_mut();
            let _ = scene.set(self.node, Prop::ScrollX, Value::Number(p[0] as f32), None);
            let _ = scene.set(self.node, Prop::ScrollY, Value::Number(p[1] as f32), None);
        }
        if self.told.get() != p {
            self.told.set(p);
            if let Some(f) = &self.on_scroll
                && let Err(e) = f.call::<()>((p[0], p[1]))
            {
                log::error!("on_scroll: {e}");
            }
        }
    }

    /// Followed at once: a touchpad's movement.
    fn jump(self: &Rc<Self>, info: &ScrollInfo, p: [f64; 2]) {
        self.stop();
        self.velocity.set([0.0; 2]);
        let p = Self::clamp(info, p);
        self.target.set(p);
        self.put(p);
    }

    /// Caught up with over a few frames: a wheel's step, a snap.
    fn glide(self: &Rc<Self>, info: &ScrollInfo, p: [f64; 2]) {
        self.velocity.set([0.0; 2]);
        self.target.set(Self::clamp(info, p));
        self.start();
    }

    fn fling(self: &Rc<Self>, v: [f64; 2]) {
        self.velocity.set(v);
        self.start();
    }

    fn start(self: &Rc<Self>) {
        self.cancel_settle();
        if self.ticking.get().is_some() {
            return;
        }
        let Some(host) = &self.host else {
            // No loop to step on (a test): straight there.
            if let Some(info) = self.info() {
                let t = self.target.get();
                self.put(Self::clamp(&info, t));
            }
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

    fn cancel_settle(&self) {
        if let (Some(token), Some(host)) = (self.settling.take(), &self.host) {
            host.remove(token);
        }
    }

    /// One step; false once it rests.
    fn step(&self) -> bool {
        let Some(info) = self.info() else { return false };
        let now = Instant::now();
        let dt = now.duration_since(self.last.get()).as_secs_f64();
        self.last.set(now);
        let at = self.now(&info);
        let mut v = self.velocity.get();
        let mut target = self.target.get();
        let mut next = at;
        let mut rests = true;
        for a in 0..2 {
            if !info.axes[a] {
                continue;
            }
            let room = info.room[a] as f64;
            if v[a] != 0.0 {
                let y = (at[a] + v[a] * dt).clamp(0.0, room);
                let slowed = v[a] * (-dt / GLIDE).exp();
                let stopped = slowed.abs() < STILL || y <= 0.0 || y >= room;
                v[a] = if stopped { 0.0 } else { slowed };
                target[a] = y;
                next[a] = y;
                rests &= stopped;
            } else {
                let to = target[a].clamp(0.0, room);
                let y = at[a] + (to - at[a]) * (1.0 - (-dt / CATCH_UP).exp());
                if (to - y).abs() < 0.5 {
                    next[a] = to;
                } else {
                    next[a] = y;
                    rests = false;
                }
            }
        }
        self.velocity.set(v);
        self.target.set(target);
        if next != at {
            self.put(next);
        }
        !rests
    }

    /// Where a snapping scroller may rest along `axis`: each child at its
    /// start or its centre, within the room.
    fn points(info: &ScrollInfo, axis: usize) -> Vec<f64> {
        let room = info.room[axis] as f64;
        let mut out: Vec<f64> = info
            .children
            .iter()
            .map(|c| {
                let (at, len) = (c[axis] as f64, c[axis + 2] as f64);
                let start = info.start[axis] as f64;
                let p = if info.snap == 2 { at + len / 2.0 - start - info.view[axis] as f64 / 2.0 } else { at - start };
                p.clamp(0.0, room)
            })
            .collect();
        out.push(0.0);
        out.push(room);
        out.sort_by(f64::total_cmp);
        out.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        out
    }

    fn nearest(info: &ScrollInfo, axis: usize, to: f64) -> f64 {
        Self::points(info, axis).into_iter().min_by(|a, b| (a - to).abs().total_cmp(&(b - to).abs())).unwrap_or(to)
    }

    /// The next point beyond `from` in the direction of `sign`, or the end.
    fn next_point(info: &ScrollInfo, axis: usize, from: f64, sign: f64) -> f64 {
        let points = Self::points(info, axis);
        if sign > 0.0 {
            points.into_iter().find(|p| *p > from + 0.5).unwrap_or(info.room[axis] as f64)
        } else {
            points.into_iter().rev().find(|p| *p < from - 0.5).unwrap_or(0.0)
        }
    }

    /// The movement of a gesture this scroller takes: its own axes, the
    /// gesture's main direction. None when the gesture runs across it, so
    /// the scroller around it has it. A wheel's steps down move a
    /// scroller that only goes across, unless one around it goes down.
    fn route(&self, info: &ScrollInfo, dx: f64, dy: f64, steps: bool) -> Option<[f64; 2]> {
        match info.axes {
            [true, true] => Some([dx, dy]),
            [true, false] => {
                if dx != 0.0 && dx.abs() >= dy.abs() {
                    Some([dx, 0.0])
                } else if steps && dy != 0.0 && !self.scene.borrow().scrolls_down_around(self.node) {
                    Some([dy, 0.0])
                } else {
                    None
                }
            }
            [false, true] => (dy != 0.0 && dy.abs() >= dx.abs()).then_some([0.0, dy]),
            _ => None,
        }
    }

    /// A wheel or a touchpad; false when it could not move, so the event
    /// goes on up.
    fn wheel(self: &Rc<Self>, dx: f64, dy: f64, steps: bool) -> bool {
        let Some(info) = self.info() else { return false };
        let Some(u) = self.route(&info, dx, dy, steps) else { return false };
        let at = self.now(&info);
        if steps {
            let from = if self.ticking.get().is_some() { self.target.get() } else { at };
            let mut to = from;
            for a in 0..2 {
                if u[a] == 0.0 {
                    continue;
                }
                to[a] = if info.snap != 0 && !info.children.is_empty() {
                    Self::next_point(&info, a, from[a], u[a].signum())
                } else {
                    // A notch moves about the view's size to the two thirds,
                    // as GTK's does: a little in a small well, more in a page.
                    let notch = (info.view[a] as f64).powf(2.0 / 3.0).max(24.0);
                    from[a] + u[a] * notch
                };
            }
            let to = Self::clamp(&info, to);
            if to == at && self.ticking.get().is_none() {
                return false;
            }
            self.glide(&info, to);
        } else {
            let to = Self::clamp(&info, [at[0] + u[0], at[1] + u[1]]);
            if to == at {
                return false;
            }
            self.jump(&info, to);
            if info.snap != 0 {
                self.settle_later();
            }
        }
        true
    }

    /// Fingers lifted while moving.
    fn fling_from(self: &Rc<Self>, vx: f64, vy: f64) -> bool {
        let Some(info) = self.info() else { return false };
        let Some(v) = self.route(&info, vx, vy, false) else { return false };
        let at = self.now(&info);
        // Against an end it is pushing into, the fling is the outer one's.
        let blocked = (0..2).all(|a| {
            v[a] == 0.0 || (v[a] < 0.0 && at[a] <= 0.0) || (v[a] > 0.0 && at[a] >= info.room[a] as f64)
        });
        if blocked {
            return false;
        }
        if info.snap != 0 && !info.children.is_empty() {
            // Where it would have stopped, the child nearest that.
            let mut to = at;
            for a in 0..2 {
                if info.axes[a] {
                    to[a] = Self::nearest(&info, a, at[a] + v[a] * GLIDE);
                }
            }
            self.glide(&info, to);
        } else {
            self.fling(v);
        }
        true
    }

    fn settle_later(self: &Rc<Self>) {
        self.cancel_settle();
        let Some(host) = &self.host else { return };
        let me = Rc::downgrade(self);
        let token = host.timer(
            SETTLE,
            Box::new(move || {
                let me = me.upgrade()?;
                me.settling.set(None);
                if let Some(info) = me.info() {
                    let at = me.now(&info);
                    let to = [0, 1].map(|a| if info.axes[a] { Self::nearest(&info, a, at[a]) } else { 0.0 });
                    me.glide(&info, to);
                }
                None
            }),
        );
        self.settling.set(token);
    }
}

impl Drop for Scroller {
    fn drop(&mut self) {
        self.stop();
        self.cancel_settle();
    }
}

/// Makes `node` a scroller: its own `on_wheel` and `on_fling`, which take
/// the place of any the interface gave it.
pub(crate) fn attach(
    lua: &Lua,
    scene: Rc<RefCell<Scene>>,
    handlers: Handlers,
    target: usize,
    node: NodeId,
    on_scroll: Option<Function>,
) -> mlua::Result<()> {
    let scroller = Rc::new(Scroller {
        scene: scene.clone(),
        node,
        target: Cell::new([0.0; 2]),
        velocity: Cell::new([0.0; 2]),
        last: Cell::new(Instant::now()),
        ticking: Cell::new(None),
        settling: Cell::new(None),
        host: crate::list::host(),
        on_scroll,
        told: Cell::new([0.0; 2]),
    });
    scene.borrow_mut().set_interactive(node, true);
    // The handlers hold the scroller; they go with the node's.
    let s = scroller.clone();
    let on_wheel = lua.create_function(move |_, (dx, dy, steps): (f64, f64, Option<bool>)| {
        Ok(if s.wheel(dx, dy, steps.unwrap_or(false)) { LuaValue::Nil } else { LuaValue::Boolean(false) })
    })?;
    let s = scroller;
    let on_fling = lua.create_function(move |_, (vx, vy): (f64, f64)| {
        Ok(if s.fling_from(vx, vy) { LuaValue::Nil } else { LuaValue::Boolean(false) })
    })?;
    let mut handlers = handlers.borrow_mut();
    let mine = handlers.entry((target, node)).or_default();
    mine.insert("on_wheel", on_wheel);
    mine.insert("on_fling", on_fling);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use genoma_core::reactive::Scope;
    use mlua::{IntoLuaMulti, Table};

    use super::*;
    use crate::mount;

    /// A column that scrolls down, holding a row that scrolls across and
    /// snaps (three children 150 wide in 200: 250 of room) and a block
    /// below it (150 of content in 100: 50 of room).
    fn nested(lua: &Lua) -> (Rc<RefCell<Scene>>, Handlers, Scope) {
        let fonts = Arc::new(Mutex::new(genoma_render::cosmic_text::FontSystem::new_with_fonts(std::iter::empty())));
        let scene = Rc::new(RefCell::new(Scene::new(fonts)));
        let handlers: Handlers = Rc::default();
        let t: Table = lua
            .load(
                r#"
                moved = {}
                return { __kind = "box", id = "outer", scroll = "y", direction = "column", width = 200, height = 100,
                    on_scroll = function(x, y) moved = { x, y } end,
                    { __kind = "box", id = "inner", scroll = "x", snap = "start", width = 200, height = 50,
                        { __kind = "box", width = 150, height = 50 },
                        { __kind = "box", width = 150, height = 50 },
                        { __kind = "box", width = 150, height = 50 } },
                    { __kind = "box", width = 200, height = 100 } }
                "#,
            )
            .eval()
            .unwrap();
        let root = scene.borrow().root();
        let (scope, _) = Scope::new(|| mount(&scene, &handlers, 0, &t, root).unwrap());
        scene.borrow_mut().layout(Some(400.0), Some(400.0));
        let _ = scene.borrow_mut().draw_list(1.0);
        (scene, handlers, scope)
    }

    /// As the runtime delivers it: up from `id` to the first handler that
    /// does not answer false. Whether one took it.
    fn deliver(scene: &Rc<RefCell<Scene>>, handlers: &Handlers, id: &str, name: &str, args: impl IntoLuaMulti + Clone) -> bool {
        let mut at = scene.borrow().find(id);
        while let Some(node) = at {
            let f = handlers.borrow().get(&(0, node)).and_then(|h| h.get(name)).cloned();
            if let Some(f) = f {
                if !matches!(f.call::<LuaValue>(args.clone()).unwrap(), LuaValue::Boolean(false)) {
                    return true;
                }
            }
            at = scene.borrow().parent(node);
        }
        false
    }

    fn at(scene: &Rc<RefCell<Scene>>, id: &str) -> [f32; 2] {
        let scene = scene.borrow();
        let node = scene.find(id).unwrap();
        let read = |p| match scene.value(node, p) {
            Some(Value::Number(v)) => v,
            _ => f32::NAN,
        };
        [read(Prop::ScrollX), read(Prop::ScrollY)]
    }

    #[test]
    fn a_gesture_across_a_scroller_goes_to_the_one_around_it() {
        let lua = Lua::new();
        let (scene, handlers, _scope) = nested(&lua);
        // A wheel's step down over the row: the row only goes across, and
        // the column around it goes down, so the column takes it — a notch
        // of 100^(2/3), at least 24.
        assert!(deliver(&scene, &handlers, "inner", "on_wheel", (0.0, 1.0, true)));
        assert_eq!(at(&scene, "inner")[0], 0.0);
        assert_eq!(at(&scene, "outer")[1], 24.0);
        let moved: Table = lua.globals().get("moved").unwrap();
        assert_eq!(moved.get::<f64>(2).unwrap(), 24.0, "on_scroll heard it");
        // A touchpad across moves the row.
        assert!(deliver(&scene, &handlers, "inner", "on_wheel", (40.0, 3.0, false)));
        assert_eq!(at(&scene, "inner")[0], 40.0);
        assert_eq!(at(&scene, "outer")[1], 24.0, "the row took it whole");
    }

    #[test]
    fn a_snapping_scroller_steps_from_child_to_child_and_hands_on_at_its_end() {
        let lua = Lua::new();
        let (scene, handlers, _scope) = nested(&lua);
        assert!(deliver(&scene, &handlers, "inner", "on_wheel", (1.0, 0.0, true)));
        assert_eq!(at(&scene, "inner")[0], 150.0, "the second child's start");
        assert!(deliver(&scene, &handlers, "inner", "on_wheel", (1.0, 0.0, true)));
        assert_eq!(at(&scene, "inner")[0], 250.0, "the end: the third cannot reach the start");
        // At its end, a fling further across is nobody's here: the column
        // does not go across.
        assert!(!deliver(&scene, &handlers, "inner", "on_fling", (900.0, 0.0)));
        // A fling back lands on the child nearest where it would stop.
        assert!(deliver(&scene, &handlers, "inner", "on_fling", (-400.0, 0.0)));
        assert_eq!(at(&scene, "inner")[0], 150.0, "250 - 400 × 0.325 = 120, nearest 150");
        // The column at its end hands a wheel on too.
        assert!(deliver(&scene, &handlers, "outer", "on_wheel", (0.0, 1e6, false)));
        assert_eq!(at(&scene, "outer")[1], 50.0);
        assert!(!deliver(&scene, &handlers, "outer", "on_wheel", (0.0, 10.0, false)));
    }
}
