//! Whether anybody is at the machine (PRD §9.1): `ext-idle-notify`, idle
//! after so many seconds without input, as the compositor counts them. It
//! respects inhibitors, as a lock does: a video playing full screen is
//! somebody watching, not somebody gone.
//!
//! The protocol lives on the main thread's Wayland connection, so the
//! runtime binds it (only once an interface asks) and sets these.

use genoma_core::reactive::Signal;

pub struct Idle {
    /// Whether the compositor offers the protocol at all.
    pub available: Signal<bool>,
    /// No input for `timeout` seconds, and nothing inhibiting idle.
    pub idle: Signal<bool>,
    /// Seconds, as the interface asked for them; 0 asks for nothing.
    pub timeout: Signal<u32>,
}

impl Default for Idle {
    fn default() -> Self {
        Idle { available: Signal::new(false), idle: Signal::new(false), timeout: Signal::new(0) }
    }
}
