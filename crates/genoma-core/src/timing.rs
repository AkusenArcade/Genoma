//! The timing set (PRD §7.2): every duration has a name, and nothing else
//! has a duration.
//!
//! A shell defines its set once (Bioma's `Timing`: open, close, reflow,
//! stagger…). A transition names an entry and never states a number, so one
//! speed setting scales the whole interface and a literal cannot slip into a
//! cell.

use std::{cell::RefCell, collections::HashMap};

/// A CSS-style cubic Bézier from (0, 0) to (1, 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Curve {
    pub const LINEAR: Curve = Curve { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 };

    /// Progress at time `t` (both 0…1). `y` may leave 0…1: an overshoot.
    pub fn eval(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        let bezier = |a: f64, b: f64, s: f64| {
            let u = 1.0 - s;
            3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
        };
        // Find s with x(s) = t: Newton first, bisection when it stalls.
        let mut s = t;
        for _ in 0..8 {
            let x = bezier(self.x1, self.x2, s) - t;
            if x.abs() < 1e-6 {
                return bezier(self.y1, self.y2, s);
            }
            let u = 1.0 - s;
            let dx = 3.0 * u * u * self.x1 + 6.0 * u * s * (self.x2 - self.x1) + 3.0 * s * s * (1.0 - self.x2);
            if dx.abs() < 1e-6 {
                break;
            }
            s -= x / dx;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        s = t;
        for _ in 0..30 {
            let x = bezier(self.x1, self.x2, s);
            if (x - t).abs() < 1e-6 {
                break;
            }
            if x < t { lo = s } else { hi = s }
            s = (lo + hi) / 2.0;
        }
        bezier(self.y1, self.y2, s)
    }
}

/// A damped spring (PRD §8.5): it has no duration, it settles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// Seconds, before the speed setting.
    pub duration: f64,
    pub curve: Curve,
    /// Seconds to wait before moving: a cascade's stagger.
    pub delay: f64,
    /// Moves on a spring instead of the curve, when set.
    pub spring: Option<Spring>,
}

impl Timing {
    /// Whether this timing moves at all.
    pub fn moves(&self) -> bool {
        self.spring.is_some() || self.duration > 0.0
    }
}

#[derive(Default)]
struct Set {
    entries: HashMap<String, Timing>,
    speed: Option<f64>,
}

thread_local! {
    static SET: RefCell<Set> = RefCell::new(Set::default());
}

pub fn define(name: &str, timing: Timing) {
    SET.with(|s| s.borrow_mut().entries.insert(name.to_owned(), timing));
}

/// The entry, with the speed setting applied: speed 2 halves every duration,
/// as Bioma's `timing.speed` does.
pub fn get(name: &str) -> Option<Timing> {
    SET.with(|s| {
        let set = s.borrow();
        let mut timing = *set.entries.get(name)?;
        let speed = set.speed.unwrap_or(1.0).max(0.01);
        timing.duration /= speed;
        timing.delay /= speed;
        // Twice the speed is half the time: four times the stiffness, twice
        // the damping, the same ratio between them.
        if let Some(s) = timing.spring.as_mut() {
            s.stiffness *= speed * speed;
            s.damping *= speed;
        }
        Some(timing)
    })
}

pub fn set_speed(speed: f64) {
    SET.with(|s| s.borrow_mut().speed = Some(speed));
}

pub fn names() -> Vec<String> {
    SET.with(|s| s.borrow().entries.keys().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_hit_their_ends_and_overshoot() {
        let open = Curve { x1: 0.2, y1: 0.9, x2: 0.3, y2: 1.15 };
        assert_eq!(open.eval(0.0), 0.0);
        assert_eq!(open.eval(1.0), 1.0);
        assert!((0..=100).map(|i| open.eval(i as f64 / 100.0)).any(|y| y > 1.0), "overshoots");
        assert!((Curve::LINEAR.eval(0.37) - 0.37).abs() < 1e-4);
    }

    #[test]
    fn speed_scales_every_duration() {
        define("open", Timing { duration: 0.25, curve: Curve::LINEAR, delay: 0.0, spring: None });
        set_speed(2.0);
        assert_eq!(get("open").unwrap().duration, 0.125);
        assert!(get("fade").is_none(), "no entry, no duration");
        set_speed(1.0);
    }
}
