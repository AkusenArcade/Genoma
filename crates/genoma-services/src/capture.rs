//! Screenshots (`@genoma/capture`): what the interface asks for, waiting
//! for the runtime, which takes the pictures through `wlr-screencopy` on its
//! Wayland connection, and the answers handed back to whoever asked.
//!
//! The service holds no connection and no thread: it is the queue between
//! a script and the runtime, as the displays service is for output
//! configurations.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
};

use genoma_core::reactive::Signal;

/// A rectangle in the compositor's logical coordinates, the space outputs
/// are laid out in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Area {
    pub fn intersection(&self, other: &Area) -> Option<Area> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.w).min(other.x + other.w);
        let bottom = (self.y + self.h).min(other.y + other.h);
        (right > x && bottom > y).then_some(Area { x, y, w: right - x, h: bottom - y })
    }

    /// The smallest rectangle holding both.
    pub fn union(&self, other: &Area) -> Area {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Area { x, y, w: (self.x + self.w).max(other.x + other.w) - x, h: (self.y + self.h).max(other.y + other.h) - y }
    }
}

/// What a picture is taken of.
#[derive(Clone, Debug, PartialEq)]
pub enum Of {
    /// Every output, as they are laid out.
    Everything,
    /// One output by name.
    Output(String),
    /// A logical rectangle, across outputs if it spans them.
    Area(Area),
}

/// What becomes of it.
#[derive(Clone, Debug, PartialEq)]
pub enum Keep {
    /// A PNG at this path; its folder is made if missing.
    File(PathBuf),
    /// The colour of the area's first pixel, "#rrggbb": a colour picker.
    Colour,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub of: Of,
    pub keep: Keep,
    /// The pointer drawn in.
    pub cursor: bool,
}

/// A path written, or a colour read.
pub type Taken = Result<String, String>;

type Waiting = Box<dyn FnOnce(Taken)>;

pub struct Capture {
    /// Whether the compositor offers wlr-screencopy.
    pub available: Signal<bool>,
    requests: RefCell<Vec<(u64, Shot)>>,
    waiting: RefCell<HashMap<u64, Waiting>>,
    next: Cell<u64>,
}

impl Default for Capture {
    fn default() -> Self {
        Capture { available: Signal::new(false), requests: RefCell::default(), waiting: RefCell::default(), next: Cell::new(1) }
    }
}

impl Capture {
    /// Asks for `shot`; `done` gets the answer on the main loop.
    pub fn take(&self, shot: Shot, done: impl FnOnce(Taken) + 'static) {
        let id = self.next.get();
        self.next.set(id + 1);
        self.waiting.borrow_mut().insert(id, Box::new(done));
        self.requests.borrow_mut().push((id, shot));
    }

    /// What has been asked for since the runtime last looked.
    pub fn take_requests(&self) -> Vec<(u64, Shot)> {
        std::mem::take(&mut *self.requests.borrow_mut())
    }

    /// The answer to request `id`, to whoever asked.
    pub fn finish(&self, id: u64, taken: Taken) {
        let waiting = self.waiting.borrow_mut().remove(&id);
        if let Some(done) = waiting {
            done(taken);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas() {
        let a = Area { x: 0.0, y: 0.0, w: 3440.0, h: 1440.0 };
        let b = Area { x: 760.0, y: 1440.0, w: 1920.0, h: 1080.0 };
        assert_eq!(a.intersection(&b), None, "touching is not overlapping");
        assert_eq!(a.union(&b), Area { x: 0.0, y: 0.0, w: 3440.0, h: 2520.0 });
        let r = Area { x: 700.0, y: 1400.0, w: 100.0, h: 100.0 };
        assert_eq!(r.intersection(&b), Some(Area { x: 760.0, y: 1440.0, w: 40.0, h: 60.0 }));
    }

    #[test]
    fn answered_once() {
        let c = Capture::default();
        let got = std::rc::Rc::new(RefCell::new(Vec::new()));
        let g = got.clone();
        c.take(Shot { of: Of::Everything, keep: Keep::Colour, cursor: false }, move |t| g.borrow_mut().push(t));
        let requests = c.take_requests();
        assert_eq!(requests.len(), 1);
        assert!(c.take_requests().is_empty());
        c.finish(requests[0].0, Ok("#000000".into()));
        c.finish(requests[0].0, Ok("#ffffff".into()));
        assert_eq!(*got.borrow(), vec![Ok("#000000".to_owned())]);
    }
}
