//! Genoma's services (PRD §9): one object per system domain, each a set of
//! signals, shared by every reader.
//!
//! A service never owns a thread or a timer of its own. It says what it needs
//! (a socket to read, an interval to sample at) and the runtime's event loop
//! drives it, so everything the shell does runs on one loop that sleeps when
//! nothing happens. D-Bus is the exception the PRD allows (§4): its
//! connections live on one bus thread, which posts into the loop.

pub mod audio;
pub mod bluetooth;
pub mod bus;
pub mod clock;
pub mod icons;
pub mod media;
pub mod network;
pub mod niri;
pub mod pam;
pub mod power;
pub mod toplevels;
pub mod tray;
pub mod vitals;
