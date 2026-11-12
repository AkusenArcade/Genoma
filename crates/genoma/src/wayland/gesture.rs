//! What a press becomes as it moves and ends — a drag past its threshold, a
//! long press, a swipe — and two fingers becoming a pinch. The arithmetic
//! only; `wayland.rs` feeds it pointer, touch and touchpad events and
//! delivers what it recognises.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use smithay_client_toolkit::reexports::calloop::RegistrationToken;

/// How far a press may wander and still be held still: a long press, and
/// the drag-and-drop of something given (the same six units).
pub const SLOP: f32 = 6.0;
/// The shortest stroke that is a swipe, logical units.
pub const SWIPE_DISTANCE: f32 = 24.0;
/// The slowest one, logical units a second.
pub const SWIPE_SPEED: f64 = 500.0;
/// The movement a release's speed is measured over: the last moments
/// before letting go, not the whole drag.
pub const WINDOW: Duration = Duration::from_millis(80);
/// A long press when the timing set has no `long_press`.
pub const LONG_PRESS: Duration = Duration::from_millis(500);

/// One press, from down to up.
#[derive(Default)]
pub struct Press {
    /// Counts presses, so a long-press timer from an earlier one is told
    /// apart.
    pub id: u64,
    pub dragging: bool,
    /// The interface took a long press: the release is no click.
    pub long_pressed: bool,
    /// Where the pointer was, recently, surface coordinates.
    pub samples: VecDeque<(Instant, f32, f32)>,
    pub timer: Option<RegistrationToken>,
}

impl Press {
    pub fn sample(&mut self, now: Instant, x: f32, y: f32) {
        self.samples.push_back((now, x, y));
        while self.samples.front().is_some_and(|s| now.duration_since(s.0) > WINDOW * 2) {
            self.samples.pop_front();
        }
    }

    /// The speed over the last `WINDOW`, logical units a second; still if
    /// the pointer stopped before letting go.
    pub fn velocity(&self, now: Instant) -> (f64, f64) {
        let recent: Vec<_> = self.samples.iter().filter(|s| now.duration_since(s.0) <= WINDOW).collect();
        let (Some(first), Some(last)) = (recent.first(), recent.last()) else { return (0.0, 0.0) };
        // Measured to now: fingers that stopped and then lifted have no speed.
        let dt = now.duration_since(first.0).as_secs_f64().max(1.0 / 240.0);
        if recent.len() < 2 {
            return (0.0, 0.0);
        }
        (((last.1 - first.1) as f64) / dt, ((last.2 - first.2) as f64) / dt)
    }
}

/// A stroke's direction, its way at the end, when it went far enough and
/// ended fast enough to be a swipe.
pub fn swipe(distance: f32, vx: f64, vy: f64) -> Option<&'static str> {
    if distance < SWIPE_DISTANCE || vx.hypot(vy) < SWIPE_SPEED {
        return None;
    }
    Some(if vx.abs() >= vy.abs() {
        if vx > 0.0 { "right" } else { "left" }
    } else if vy > 0.0 {
        "down"
    } else {
        "up"
    })
}

/// Two fingers on a screen: where each began and is now.
pub struct TwoFingers {
    pub surface: usize,
    pub ids: [i32; 2],
    pub start: [(f32, f32); 2],
    pub now: [(f32, f32); 2],
    /// The node under their middle when they met.
    pub node: genoma_scene::NodeId,
}

impl TwoFingers {
    /// Scale since they began, turn in degrees (clockwise, as the screen's
    /// y runs down), and their middle.
    pub fn reading(&self) -> (f64, f64, f32, f32) {
        let span = |p: [(f32, f32); 2]| ((p[1].0 - p[0].0) as f64).hypot((p[1].1 - p[0].1) as f64);
        let angle = |p: [(f32, f32); 2]| ((p[1].1 - p[0].1) as f64).atan2((p[1].0 - p[0].0) as f64);
        let scale = span(self.now) / span(self.start).max(1.0);
        let mut turn = (angle(self.now) - angle(self.start)).to_degrees();
        if turn > 180.0 {
            turn -= 360.0;
        } else if turn < -180.0 {
            turn += 360.0;
        }
        let mid = ((self.now[0].0 + self.now[1].0) / 2.0, (self.now[0].1 + self.now[1].1) / 2.0);
        (scale, turn, mid.0, mid.1)
    }
}

/// A touchpad gesture under way: the node it began over and what it has
/// added up to.
pub struct Pad {
    pub surface: usize,
    pub node: genoma_scene::NodeId,
    pub began: Instant,
    pub scale: f64,
    pub turn: f64,
    pub moved: (f64, f64),
    pub at: (f32, f32),
}

// ---- Touch and touchpad gestures, as the app receives them -------------------

use genoma_script::Input;
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    reexports::{
        client::{
            Connection, QueueHandle,
            protocol::{wl_surface::WlSurface, wl_touch::WlTouch},
        },
        protocols::wp::pointer_gestures::zv1::client::{
            zwp_pointer_gesture_hold_v1::{self, ZwpPointerGestureHoldV1},
            zwp_pointer_gesture_pinch_v1::{self, ZwpPointerGesturePinchV1},
            zwp_pointer_gesture_swipe_v1::{self, ZwpPointerGestureSwipeV1},
        },
    },
    seat::touch::TouchHandler,
};

use super::App;

/// The fingers on a screen: the first acts as the pointer; a second makes
/// the two a pinch.
#[derive(Default)]
pub struct Fingers {
    /// The first finger: its id, surface, and where it is.
    pub primary: Option<(i32, usize, f32, f32)>,
    pub two: Option<TwoFingers>,
}

impl TouchHandler for App {
    fn down(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, serial: u32, _: u32, surface: WlSurface, id: i32, position: (f64, f64)) {
        let Some(i) = self.by_wl(&surface) else { return };
        let (x, y) = (position.0 as f32, position.1 as f32);
        match self.fingers.primary {
            None if self.fingers.two.is_none() => {
                self.fingers.primary = Some((id, i, x, y));
                let hit = self.surfaces[i].scene.borrow().hit_test(x, y);
                self.surfaces[i].scene.borrow_mut().set_pointer(Some((x, y, true)));
                self.pointer_press(i, hit, x, y, 0x110, serial);
            }
            // A second finger on the same surface: what the first pressed
            // is let go without a click, and the two pinch.
            Some((first, fi, fx, fy)) if fi == i => {
                self.flush_drag();
                self.cancel_long_press(i);
                if let Some(node) = self.surfaces[i].pressed.take() {
                    let (lx, ly) = self.surfaces[i].scene.borrow().local(node, fx, fy);
                    self.deliver(i, node, Input::Release(0x110, lx, ly));
                    if self.surfaces[i].press.dragging {
                        self.deliver(i, node, Input::DragEnd(0.0, 0.0));
                    }
                }
                self.surfaces[i].press.dragging = false;
                self.fingers.primary = None;
                let (mx, my) = ((fx + x) / 2.0, (fy + y) / 2.0);
                let Some(node) = self.surfaces[i].scene.borrow().hit_test(mx, my) else { return };
                let two = TwoFingers { surface: i, ids: [first, id], start: [(fx, fy), (x, y)], now: [(fx, fy), (x, y)], node };
                self.deliver(i, node, Input::Pinch(1.0, 0.0, "begin", mx, my));
                self.fingers.two = Some(two);
            }
            _ => {}
        }
    }

    fn up(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: u32, _: u32, id: i32) {
        if let Some(two) = &self.fingers.two
            && two.ids.contains(&id)
        {
            let (scale, turn, x, y) = two.reading();
            let (i, node) = (two.surface, two.node);
            self.fingers.two = None;
            self.deliver(i, node, Input::Pinch(scale, turn, "end", x, y));
            return;
        }
        if let Some((first, i, x, y)) = self.fingers.primary
            && first == id
        {
            self.fingers.primary = None;
            self.flush_drag();
            let hit = self.surfaces[i].scene.borrow().hit_test(x, y);
            self.pointer_release(i, hit, x, y, 0x110);
            self.surfaces[i].scene.borrow_mut().set_pointer(None);
        }
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: u32, id: i32, position: (f64, f64)) {
        let (x, y) = (position.0 as f32, position.1 as f32);
        if let Some(two) = &mut self.fingers.two {
            let Some(k) = two.ids.iter().position(|f| *f == id) else { return };
            two.now[k] = (x, y);
            let (scale, turn, mx, my) = two.reading();
            let (i, node) = (two.surface, two.node);
            self.deliver(i, node, Input::Pinch(scale, turn, "update", mx, my));
            return;
        }
        if let Some((first, i, _, _)) = self.fingers.primary
            && first == id
        {
            self.fingers.primary = Some((id, i, x, y));
            self.surfaces[i].scene.borrow_mut().set_pointer(Some((x, y, true)));
            self.pointer_motion(i, x, y);
            // No frame of the pointer's to wait for: the drag goes now.
            self.flush_drag();
        }
    }

    fn shape(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64, _: f64) {}

    fn orientation(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64) {}

    /// The compositor took the touches for a gesture of its own.
    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch) {
        if let Some(two) = self.fingers.two.take() {
            let (scale, turn, x, y) = two.reading();
            self.deliver(two.surface, two.node, Input::Pinch(scale, turn, "cancel", x, y));
        }
        if let Some((_, i, x, y)) = self.fingers.primary.take() {
            self.flush_drag();
            self.cancel_long_press(i);
            if let Some(node) = self.surfaces[i].pressed.take() {
                let (lx, ly) = self.surfaces[i].scene.borrow().local(node, x, y);
                self.deliver(i, node, Input::Release(0x110, lx, ly));
            }
            self.surfaces[i].press.dragging = false;
            self.surfaces[i].scene.borrow_mut().set_pointer(None);
        }
    }
}

/// The touchpad's gestures, their events.
pub struct GestureData;

impl App {
    /// Where a touchpad gesture on `surface` begins: under the pointer.
    fn pad_begin(&mut self, surface: &WlSurface) -> Option<Pad> {
        let i = self.by_wl(surface)?;
        let (x, y, _) = self.surfaces[i].scene.borrow().pointer()?;
        let node = self.surfaces[i].scene.borrow().hit_test(x, y)?;
        Some(Pad { surface: i, node, began: Instant::now(), scale: 1.0, turn: 0.0, moved: (0.0, 0.0), at: (x, y) })
    }
}

impl Dispatch2<ZwpPointerGesturePinchV1, App> for GestureData {
    fn event(&self, app: &mut App, _: &ZwpPointerGesturePinchV1, event: zwp_pointer_gesture_pinch_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use zwp_pointer_gesture_pinch_v1::Event as E;
        match event {
            E::Begin { surface, .. } => {
                app.pad = app.pad_begin(&surface);
                if let Some(p) = &app.pad {
                    let (i, node, (x, y)) = (p.surface, p.node, p.at);
                    app.deliver(i, node, Input::Pinch(1.0, 0.0, "begin", x, y));
                }
            }
            E::Update { scale, rotation, .. } => {
                let Some(p) = &mut app.pad else { return };
                // The scale is since the beginning; the turn comes in steps.
                p.scale = scale;
                p.turn += rotation;
                let (i, node, s, t, (x, y)) = (p.surface, p.node, p.scale, p.turn, p.at);
                app.deliver(i, node, Input::Pinch(s, t, "update", x, y));
            }
            E::End { cancelled, .. } => {
                let Some(p) = app.pad.take() else { return };
                let phase = if cancelled != 0 { "cancel" } else { "end" };
                app.deliver(p.surface, p.node, Input::Pinch(p.scale, p.turn, phase, p.at.0, p.at.1));
            }
            _ => {}
        }
    }
}

impl Dispatch2<ZwpPointerGestureSwipeV1, App> for GestureData {
    fn event(&self, app: &mut App, _: &ZwpPointerGestureSwipeV1, event: zwp_pointer_gesture_swipe_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use zwp_pointer_gesture_swipe_v1::Event as E;
        match event {
            E::Begin { surface, .. } => app.pad = app.pad_begin(&surface),
            E::Update { dx, dy, .. } => {
                if let Some(p) = &mut app.pad {
                    p.moved.0 += dx;
                    p.moved.1 += dy;
                }
            }
            E::End { cancelled, .. } => {
                let Some(p) = app.pad.take() else { return };
                if cancelled != 0 {
                    return;
                }
                // Fingers on a touchpad travel less than a pointer: their
                // speed over the whole stroke, against a lower bar.
                let secs = p.began.elapsed().as_secs_f64().max(0.01);
                let (vx, vy) = (p.moved.0 / secs, p.moved.1 / secs);
                let distance = p.moved.0.hypot(p.moved.1) as f32;
                if let Some(way) = swipe(distance, vx, vy) {
                    app.deliver(p.surface, p.node, Input::Swipe(way, vx.hypot(vy)));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch2<ZwpPointerGestureHoldV1, App> for GestureData {
    fn event(&self, app: &mut App, _: &ZwpPointerGestureHoldV1, event: zwp_pointer_gesture_hold_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use zwp_pointer_gesture_hold_v1::Event as E;
        match event {
            E::Begin { surface, .. } => {
                if let Some(p) = app.pad_begin(&surface) {
                    app.deliver(p.surface, p.node, Input::Hold(true));
                    app.held = Some((p.surface, p.node));
                }
            }
            E::End { .. } => {
                if let Some((i, node)) = app.held.take() {
                    app.deliver(i, node, Input::Hold(false));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_is_as_fast_as_its_last_moments() {
        let t0 = Instant::now();
        let mut p = Press::default();
        // A long slow start, then 20 units over the last 80 ms: the speed
        // is the last 80 ms', not the whole drag's.
        p.sample(t0, 0.0, 0.0);
        p.sample(t0 + Duration::from_millis(100), 2.0, 0.0);
        p.sample(t0 + Duration::from_millis(160), 12.0, 0.0);
        p.sample(t0 + Duration::from_millis(180), 22.0, 0.0);
        let (vx, vy) = p.velocity(t0 + Duration::from_millis(180));
        assert!((vx - 250.0).abs() < 1.0, "{vx}");
        assert_eq!(vy, 0.0);
        // Stopped, then lifted: no speed left.
        assert_eq!(p.velocity(t0 + Duration::from_millis(400)), (0.0, 0.0));
    }

    #[test]
    fn a_swipe_is_long_and_fast_enough_and_goes_its_last_way() {
        assert_eq!(swipe(40.0, 900.0, 100.0), Some("right"));
        assert_eq!(swipe(60.0, -5.0, -900.0), Some("up"));
        assert_eq!(swipe(10.0, 900.0, 0.0), None, "too short");
        assert_eq!(swipe(80.0, 200.0, 0.0), None, "too slow");
        // Dragged right a long way, flicked back left at the end.
        assert_eq!(swipe(200.0, -800.0, 0.0), Some("left"));
    }

    #[test]
    fn two_fingers_read_as_scale_and_turn() {
        let id = genoma_scene::NodeId::from(1u64);
        let mut f = TwoFingers { surface: 0, ids: [1, 2], start: [(0.0, 0.0), (100.0, 0.0)], now: [(0.0, 0.0), (100.0, 0.0)], node: id };
        f.now = [(-50.0, 0.0), (150.0, 0.0)];
        let (scale, turn, x, y) = f.reading();
        assert!((scale - 2.0).abs() < 1e-9 && turn.abs() < 1e-9 && (x, y) == (50.0, 0.0));
        // A quarter turn clockwise on screen.
        f.now = [(50.0, -50.0), (50.0, 50.0)];
        let (scale, turn, ..) = f.reading();
        assert!((scale - 1.0).abs() < 1e-9 && (turn - 90.0).abs() < 1e-9, "{turn}");
    }
}
