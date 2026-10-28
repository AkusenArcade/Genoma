//! Windows, compositor-agnostic (PRD §9.1): `ext-foreign-toplevel-list`,
//! each open window's title, app id and identifier. Bioma reads niri's IPC
//! instead, which also gives geometry, pids, workspaces and outputs; this is
//! the fallback should Bioma ever run on another compositor.
//!
//! The protocol lives on the main thread's Wayland connection, so the
//! runtime binds it (only once an interface asks) and sets this list.

use genoma_core::reactive::Signal;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Toplevel {
    /// Stable for the window's life, whichever handle reports it.
    pub identifier: String,
    pub title: String,
    pub app_id: String,
}

pub struct Toplevels {
    /// Whether the compositor offers the protocol at all.
    pub available: Signal<bool>,
    /// In the order the compositor announced them.
    pub list: Signal<Vec<Toplevel>>,
}

impl Default for Toplevels {
    fn default() -> Self {
        Toplevels { available: Signal::new(false), list: Signal::new(Vec::new()) }
    }
}
