//! Genoma's services (PRD §9): one object per system domain, each a set of
//! signals, shared by every reader.
//!
//! A service never owns a thread or a timer of its own. It says what it needs
//! (a socket to read, an interval to sample at) and the runtime's event loop
//! drives it, so everything the shell does runs on one loop that sleeps when
//! nothing happens.

pub mod clock;
pub mod icons;
pub mod niri;
pub mod vitals;
