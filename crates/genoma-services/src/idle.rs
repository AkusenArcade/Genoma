//! Whether anybody is at the machine (PRD §9.1): `ext-idle-notify`, idle
//! after so many seconds without input, as the compositor counts them. It
//! respects inhibitors, as a lock does: a video playing full screen is
//! somebody watching, not somebody gone.
//!
//! Several timeouts at once, each by name (a lock after ten minutes, a
//! suspend after thirty, the growth rings' presence after five): the
//! compositor counts each from the moment it is asked for.
//!
//! The protocol lives on the main thread's Wayland connection, so the
//! runtime binds it (only once an interface asks) and sets these.

use std::collections::BTreeMap;

use genoma_core::reactive::Signal;

/// The name `idle:after` and `idle.idle` use.
pub const DEFAULT: &str = "default";

pub struct Idle {
    /// Whether the compositor offers the protocol at all.
    pub available: Signal<bool>,
    /// Seconds by name, as the interface asked for them; a name absent or
    /// at 0 asks for nothing.
    pub timeouts: Signal<BTreeMap<String, u32>>,
    /// The names whose seconds have passed with no input and nothing
    /// inhibiting idle.
    pub idle: Signal<BTreeMap<String, bool>>,
}

impl Idle {
    pub fn set_timeout(&self, name: &str, seconds: u32) {
        let mut all = self.timeouts.get_untracked().unwrap_or_default();
        if all.get(name).copied().unwrap_or(0) == seconds {
            return;
        }
        if seconds == 0 {
            all.remove(name);
        } else {
            all.insert(name.to_owned(), seconds);
        }
        self.timeouts.set(all);
    }

    pub fn is_idle(&self, name: &str) -> bool {
        self.idle.get().get(name).copied().unwrap_or(false)
    }

    pub fn set_idle(&self, name: &str, on: bool) {
        let mut all = self.idle.get_untracked().unwrap_or_default();
        if all.get(name).copied().unwrap_or(false) == on {
            return;
        }
        all.insert(name.to_owned(), on);
        self.idle.set(all);
    }
}

impl Default for Idle {
    fn default() -> Self {
        Idle { available: Signal::new(false), timeouts: Signal::new(BTreeMap::new()), idle: Signal::new(BTreeMap::new()) }
    }
}
