//! The displays and how they are set up: `wlr-output-management`, each
//! head (a connector with a monitor on it) with its modes, position,
//! scale, transform, adaptive sync, and whether it is on; and changing any
//! of those, or only asking whether a change would work.
//!
//! The protocol lives on the main thread's Wayland connection, so the
//! runtime binds it (only once an interface asks), sets `heads`, and
//! carries out the [`Request`]s left here. A configuration names every
//! head or the compositor closes the connection, so [`plan`] fills in the
//! heads a change leaves alone with what they have now.

use std::cell::{Cell, RefCell};

use genoma_core::reactive::Signal;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mode {
    pub width: i32,
    pub height: i32,
    /// Hz; 0 when the compositor does not say.
    pub refresh: f64,
    pub preferred: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Head {
    /// The connector: "DP-1", the name `g.outputs()` gives too.
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    /// Millimetres; 0 when unknown (a projector, a virtual output).
    pub width_mm: i32,
    pub height_mm: i32,
    pub enabled: bool,
    pub modes: Vec<Mode>,
    /// The mode in use, when enabled and one of `modes`.
    pub mode: Option<Mode>,
    /// Logical position in the global space.
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    /// One of [`TRANSFORMS`].
    pub transform: &'static str,
    /// Known only from protocol version 4.
    pub adaptive_sync: Option<bool>,
}

/// `wl_output.transform` by its value, the names niri and Sway use.
pub const TRANSFORMS: [&str; 8] = ["normal", "90", "180", "270", "flipped", "flipped-90", "flipped-180", "flipped-270"];

pub fn transform_name(value: u32) -> &'static str {
    TRANSFORMS.get(value as usize).copied().unwrap_or("normal")
}

/// What an interface changes on one head; what it leaves `None` stays.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub name: String,
    pub enabled: Option<bool>,
    /// Width, height and, if it matters, the refresh in Hz.
    pub mode: Option<(i32, i32, Option<f64>)>,
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    pub transform: Option<String>,
    pub adaptive_sync: Option<bool>,
}

/// How a head's mode is set in a configuration.
#[derive(Clone, Debug, PartialEq)]
pub enum ModeChoice {
    /// One of the head's `modes`, by its place.
    Listed(usize),
    /// A size and refresh (mHz, 0 for any) the head does not list.
    Custom(i32, i32, i32),
}

/// One head in a whole configuration.
#[derive(Clone, Debug, PartialEq)]
pub enum Planned {
    Disabled { head: usize },
    Enabled { head: usize, mode: Option<ModeChoice>, position: (i32, i32), scale: f64, transform: u32, adaptive_sync: Option<bool> },
}

/// The whole configuration `changes` make of `heads`: every head, the ones
/// not named as they are now. A mistake (a head that is not there, a
/// transform with no name, a scale of nothing, every head turned off) is
/// told before anything reaches the compositor.
pub fn plan(heads: &[Head], changes: &[Change]) -> Result<Vec<Planned>, String> {
    for c in changes {
        if !heads.iter().any(|h| h.name == c.name) {
            return Err(format!("no display {}", c.name));
        }
        if changes.iter().filter(|o| o.name == c.name).count() > 1 {
            return Err(format!("{} changed twice", c.name));
        }
    }
    let mut out = Vec::with_capacity(heads.len());
    for (i, h) in heads.iter().enumerate() {
        let change = changes.iter().find(|c| c.name == h.name);
        let enabled = change.and_then(|c| c.enabled).unwrap_or(h.enabled);
        if !enabled {
            out.push(Planned::Disabled { head: i });
            continue;
        }
        let mode = match change.and_then(|c| c.mode) {
            // A custom mode of no size closes the connection.
            Some((w, hgt, refresh)) if w <= 0 || hgt <= 0 || refresh.is_some_and(|r| !(r.is_finite() && r >= 0.0)) => {
                return Err(format!("{}: no mode {w}x{hgt}", h.name));
            }
            Some((w, hgt, refresh)) => Some(choose(&h.modes, w, hgt, refresh)),
            // Turned on with no mode said: the one in use, or the
            // preferred one, or the compositor's choice.
            None => h
                .mode
                .as_ref()
                .and_then(|m| h.modes.iter().position(|o| o == m))
                .or_else(|| h.modes.iter().position(|m| m.preferred))
                .map(ModeChoice::Listed),
        };
        let scale = change.and_then(|c| c.scale).unwrap_or(h.scale);
        if !(scale.is_finite() && scale > 0.0) {
            return Err(format!("{}: scale {scale}", h.name));
        }
        let transform = match change.and_then(|c| c.transform.as_deref()) {
            Some(t) => TRANSFORMS.iter().position(|n| *n == t).ok_or_else(|| format!("{}: no transform {t:?} (one of {})", h.name, TRANSFORMS.join(", ")))? as u32,
            None => TRANSFORMS.iter().position(|n| *n == h.transform).unwrap_or(0) as u32,
        };
        out.push(Planned::Enabled {
            head: i,
            mode,
            position: change.and_then(|c| c.position).unwrap_or((h.x, h.y)),
            scale,
            transform,
            adaptive_sync: change.and_then(|c| c.adaptive_sync),
        });
    }
    if !heads.is_empty() && out.iter().all(|p| matches!(p, Planned::Disabled { .. })) {
        return Err("every display would be off".into());
    }
    Ok(out)
}

/// The listed mode of that size nearest the refresh asked (the highest
/// with none asked), or a custom one when the size is not listed.
fn choose(modes: &[Mode], width: i32, height: i32, refresh: Option<f64>) -> ModeChoice {
    let sized = modes.iter().enumerate().filter(|(_, m)| m.width == width && m.height == height);
    let best = match refresh {
        Some(r) => sized.min_by(|a, b| (a.1.refresh - r).abs().total_cmp(&(b.1.refresh - r).abs())),
        None => sized.max_by(|a, b| a.1.refresh.total_cmp(&b.1.refresh)),
    };
    match best {
        Some((i, m)) if refresh.is_none_or(|r| (m.refresh - r).abs() < 0.5) => ModeChoice::Listed(i),
        _ => ModeChoice::Custom(width, height, refresh.map_or(0, |r| (r * 1000.0).round() as i32)),
    }
}

/// What the interface asks of the displays.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Apply (or, with `test`, only try) these changes; the answer comes
    /// back in `result` under `id`.
    Configure { id: u64, changes: Vec<Change>, test: bool },
}

/// The compositor's answer to a configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub id: u64,
    pub test: bool,
    /// "applied" (or, for a test, "works"), "failed", or "cancelled" (the
    /// displays changed meanwhile: look again and ask again).
    pub status: &'static str,
}

pub struct Displays {
    /// Whether the compositor offers the protocol at all.
    pub available: Signal<bool>,
    /// In the order the compositor announced them.
    pub heads: Signal<Vec<Head>>,
    /// The answer to the latest configuration.
    pub result: Signal<Option<Outcome>>,
    pub requests: RefCell<Vec<Request>>,
    next: Cell<u64>,
}

impl Default for Displays {
    fn default() -> Self {
        Displays { available: Signal::new(false), heads: Signal::new(Vec::new()), result: Signal::new(None), requests: RefCell::new(Vec::new()), next: Cell::new(0) }
    }
}

impl Displays {
    /// Checks `changes` against the heads now and leaves them for the
    /// runtime; the number its answer will carry.
    pub fn configure(&self, changes: Vec<Change>, test: bool) -> Result<u64, String> {
        if !self.available.get_untracked().unwrap_or(false) {
            return Err("the compositor offers no output management".into());
        }
        plan(&self.heads.get_untracked().unwrap_or_default(), &changes)?;
        let id = self.next.get() + 1;
        self.next.set(id);
        self.requests.borrow_mut().push(Request::Configure { id, changes, test });
        Ok(id)
    }

    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.borrow_mut())
    }

    pub fn set_heads(&self, heads: Vec<Head>) {
        if self.heads.get_untracked().as_ref() != Some(&heads) {
            self.heads.set(heads);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: i32, height: i32, refresh: f64) -> Mode {
        Mode { width, height, refresh, preferred: false }
    }

    fn heads() -> Vec<Head> {
        let modes = vec![mode(3440, 1440, 59.97), Mode { preferred: true, ..mode(3440, 1440, 164.9) }, mode(1920, 1080, 60.0)];
        vec![
            Head { name: "DP-1".into(), enabled: true, mode: Some(modes[1].clone()), modes, scale: 1.0, transform: "normal", ..Head::default() },
            Head { name: "HDMI-A-1".into(), enabled: true, modes: vec![mode(1920, 1080, 60.0)], x: 3440, scale: 1.25, transform: "90", ..Head::default() },
        ]
    }

    #[test]
    fn heads_left_alone_keep_what_they_have() {
        let plan = plan(&heads(), &[Change { name: "DP-1".into(), scale: Some(1.5), ..Change::default() }]).unwrap();
        assert_eq!(
            plan,
            [
                Planned::Enabled { head: 0, mode: Some(ModeChoice::Listed(1)), position: (0, 0), scale: 1.5, transform: 0, adaptive_sync: None },
                Planned::Enabled { head: 1, mode: None, position: (3440, 0), scale: 1.25, transform: 1, adaptive_sync: None },
            ]
        );
    }

    #[test]
    fn a_mode_is_the_nearest_listed_or_a_custom_one() {
        let h = heads();
        let mode_of = |m| match &plan(&h, &[Change { name: "DP-1".into(), mode: Some(m), ..Change::default() }]).unwrap()[0] {
            Planned::Enabled { mode, .. } => mode.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(mode_of((3440, 1440, Some(60.0))), Some(ModeChoice::Listed(0)));
        assert_eq!(mode_of((3440, 1440, None)), Some(ModeChoice::Listed(1)), "the fastest with no refresh asked");
        assert_eq!(mode_of((3440, 1440, Some(100.0))), Some(ModeChoice::Custom(3440, 1440, 100_000)));
        assert_eq!(mode_of((2560, 1080, None)), Some(ModeChoice::Custom(2560, 1080, 0)));
    }

    #[test]
    fn mistakes_are_told_before_the_compositor_hears() {
        let h = heads();
        let off = |name: &str| Change { name: name.into(), enabled: Some(false), ..Change::default() };
        assert_eq!(plan(&h, &[off("DP-2")]), Err("no display DP-2".into()));
        assert_eq!(plan(&h, &[off("DP-1"), off("HDMI-A-1")]), Err("every display would be off".into()));
        assert!(plan(&h, &[Change { name: "DP-1".into(), transform: Some("45".into()), ..Change::default() }]).is_err());
        assert!(plan(&h, &[Change { name: "DP-1".into(), scale: Some(0.0), ..Change::default() }]).is_err());
        assert!(plan(&h, &[off("DP-1"), off("DP-1")]).is_err());
        assert!(plan(&h, &[Change { name: "DP-1".into(), mode: Some((0, 1080, None)), ..Change::default() }]).is_err());
        assert!(plan(&h, &[Change { name: "DP-1".into(), mode: Some((1920, 1080, Some(-1.0))), ..Change::default() }]).is_err());
        assert_eq!(plan(&h, &[off("HDMI-A-1")]).unwrap()[1], Planned::Disabled { head: 1 });
    }

    #[test]
    fn requests_wait_for_the_runtime() {
        let d = Displays::default();
        assert!(d.configure(Vec::new(), true).is_err(), "nothing to configure without the protocol");
        d.available.set(true);
        d.set_heads(heads());
        assert_eq!(d.configure(Vec::new(), true), Ok(1));
        assert_eq!(d.configure(vec![Change { name: "DP-1".into(), ..Change::default() }], false), Ok(2));
        assert_eq!(d.take_requests().len(), 2);
        assert!(d.take_requests().is_empty());
    }
}
