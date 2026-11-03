//! The session lock (PRD §6.1): `ext-session-lock-v1`, the compositor
//! holding every output for one client until it says the session may open
//! again. While locked, the interface's `"lock"` surfaces are the only thing
//! on screen, one per output.
//!
//! The protocol lives on the main thread's Wayland connection, so the
//! runtime binds it (only once an interface asks) and sets these; the
//! interface only asks.

use genoma_core::reactive::Signal;

pub struct Lock {
    /// Whether the compositor offers the protocol at all.
    pub available: Signal<bool>,
    /// The interface asked for the lock and has not let it go.
    pub wanted: Signal<bool>,
    /// The compositor says every output is covered: the session is locked.
    pub locked: Signal<bool>,
    /// The compositor refused the lock or ended it on its own: there is
    /// nothing left to unlock, and the interface's lock surfaces are gone.
    pub finished: Signal<bool>,
}

impl Default for Lock {
    fn default() -> Self {
        Lock {
            available: Signal::new(false),
            wanted: Signal::new(false),
            locked: Signal::new(false),
            finished: Signal::new(false),
        }
    }
}
