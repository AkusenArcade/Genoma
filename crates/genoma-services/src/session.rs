//! The session as logind keeps it (`@genoma/session`): what the machine
//! can do (suspend, hibernate, reboot, power off) and doing it, the moment
//! before it sleeps or shuts down, a request to lock this session, and
//! inhibitors, held and listed. Read when the service starts and followed
//! on logind's signals; nothing polls.

use std::{cell::RefCell, collections::HashMap};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{OwnedFd, proxy::CacheProperties};

use crate::bus::{self, Bus};

/// What logind says of each action: "yes", "no", "challenge" (it asks for
/// a password through polkit), "na" (the machine cannot).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Can {
    pub suspend: String,
    pub hibernate: String,
    pub hybrid_sleep: String,
    pub reboot: String,
    pub power_off: String,
}

/// An inhibitor somebody holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inhibitor {
    /// "sleep", "shutdown", "idle", "handle-lid-switch", … joined by ':'.
    pub what: String,
    pub who: String,
    pub why: String,
    /// "block" or "delay".
    pub mode: String,
    pub uid: u32,
    pub pid: u32,
}

pub struct Session {
    pub available: Signal<bool>,
    pub can: Signal<Can>,
    /// Between PrepareForSleep(true) and (false): the moment to lock.
    pub preparing_for_sleep: Signal<bool>,
    pub preparing_for_shutdown: Signal<bool>,
    /// Moves on each request to lock this session (`loginctl lock-session`).
    pub lock_requests: Signal<u64>,
    /// Moves on each request to unlock it.
    pub unlock_requests: Signal<u64>,
    pub inhibitors: Signal<Vec<Inhibitor>>,
    /// The inhibitors this interface holds, by handle; dropping the file
    /// releases one.
    held: RefCell<HashMap<u64, OwnedFd>>,
    /// Let go before logind gave them: dropped as they arrive.
    released: RefCell<std::collections::HashSet<u64>>,
    next: std::cell::Cell<u64>,
    bus: RefCell<Option<(Bus, Sender<Update>)>>,
}

/// What the bus thread posts.
pub enum Update {
    Available(bool),
    Can(Can),
    PreparingForSleep(bool),
    PreparingForShutdown(bool),
    Lock,
    Unlock,
    Inhibitors(Vec<Inhibitor>),
    Held(u64, OwnedFd),
}

impl Default for Session {
    fn default() -> Self {
        Session {
            available: Signal::new(false),
            can: Signal::new(Can::default()),
            preparing_for_sleep: Signal::new(false),
            preparing_for_shutdown: Signal::new(false),
            lock_requests: Signal::new(0),
            unlock_requests: Signal::new(0),
            inhibitors: Signal::new(Vec::new()),
            held: RefCell::default(),
            released: RefCell::default(),
            next: std::cell::Cell::new(1),
            bus: RefCell::new(None),
        }
    }
}

#[zbus::proxy(interface = "org.freedesktop.login1.Manager", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1")]
trait Manager {
    fn can_suspend(&self) -> zbus::Result<String>;
    fn can_hibernate(&self) -> zbus::Result<String>;
    fn can_hybrid_sleep(&self) -> zbus::Result<String>;
    fn can_reboot(&self) -> zbus::Result<String>;
    fn can_power_off(&self) -> zbus::Result<String>;
    fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    fn hibernate(&self, interactive: bool) -> zbus::Result<()>;
    fn hybrid_sleep(&self, interactive: bool) -> zbus::Result<()>;
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<OwnedFd>;
    fn list_inhibitors(&self) -> zbus::Result<Vec<(String, String, String, String, u32, u32)>>;
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;
}

/// This process's own session: logind's `auto` path.
#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1/session/auto")]
trait OwnSession {
    fn lock(&self) -> zbus::Result<()>;
    #[zbus(signal, name = "Lock")]
    fn lock_asked(&self) -> zbus::Result<()>;
    #[zbus(signal, name = "Unlock")]
    fn unlock_asked(&self) -> zbus::Result<()>;
}

async fn can(manager: &ManagerProxy<'_>) -> Can {
    Can {
        suspend: manager.can_suspend().await.unwrap_or_else(|_| "na".into()),
        hibernate: manager.can_hibernate().await.unwrap_or_else(|_| "na".into()),
        hybrid_sleep: manager.can_hybrid_sleep().await.unwrap_or_else(|_| "na".into()),
        reboot: manager.can_reboot().await.unwrap_or_else(|_| "na".into()),
        power_off: manager.can_power_off().await.unwrap_or_else(|_| "na".into()),
    }
}

async fn inhibitors(manager: &ManagerProxy<'_>) -> Vec<Inhibitor> {
    manager
        .list_inhibitors()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(what, who, why, mode, uid, pid)| Inhibitor { what, who, why, mode, uid, pid })
        .collect()
}

/// The actions logind takes, by the names the interface gives them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Suspend,
    Hibernate,
    HybridSleep,
    Reboot,
    PowerOff,
}

impl Session {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Available(on) => self.available.set(on),
            Update::Can(can) => self.can.set(can),
            Update::PreparingForSleep(on) => self.preparing_for_sleep.set(on),
            Update::PreparingForShutdown(on) => self.preparing_for_shutdown.set(on),
            Update::Lock => self.lock_requests.set(self.lock_requests.get_untracked().unwrap_or(0) + 1),
            Update::Unlock => self.unlock_requests.set(self.unlock_requests.get_untracked().unwrap_or(0) + 1),
            Update::Inhibitors(list) => {
                if self.inhibitors.get_untracked().as_ref() != Some(&list) {
                    self.inhibitors.set(list);
                }
            }
            Update::Held(handle, fd) => {
                if !self.released.borrow_mut().remove(&handle) {
                    self.held.borrow_mut().insert(handle, fd);
                }
                self.refresh_inhibitors();
            }
        });
    }

    /// Starts following logind on the bus thread; updates arrive through
    /// `post`.
    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some((bus.clone(), post.clone()));
        let to = post.clone();
        bus.spawn("logind", async move {
            let conn = bus::system().await?;
            let manager = ManagerProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
            let found = can(&manager).await;
            let _ = to.send(Update::Available(found.reboot != "na" || found.suspend != "na"));
            let _ = to.send(Update::Can(found));
            let _ = to.send(Update::Inhibitors(inhibitors(&manager).await));
            let sleep = manager.receive_prepare_for_sleep().await?.map(|s| s.args().map(|a| Update::PreparingForSleep(a.start)).ok());
            let shutdown = manager.receive_prepare_for_shutdown().await?.map(|s| s.args().map(|a| Update::PreparingForShutdown(a.start)).ok());
            let mut both = futures_util::stream::select(sleep, shutdown);
            while let Some(update) = both.next().await {
                if let Some(update) = update {
                    let _ = to.send(update);
                }
            }
            Ok(())
        });
        bus.spawn("logind session", async move {
            let conn = bus::system().await?;
            let session = OwnSessionProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
            let locks = session.receive_lock_asked().await?.map(|_| Update::Lock);
            let unlocks = session.receive_unlock_asked().await?.map(|_| Update::Unlock);
            let mut both = futures_util::stream::select(locks, unlocks);
            while let Some(update) = both.next().await {
                let _ = post.send(update);
            }
            Ok(())
        });
    }

    fn with_bus(&self, name: &'static str, work: impl FnOnce(Bus, Sender<Update>)) {
        match self.bus.borrow().clone() {
            Some((bus, post)) => work(bus, post),
            None => log::warn!("session: {name} before the service started"),
        }
    }

    /// Asks logind to take `action`; one that wants a password asks for it
    /// through polkit.
    pub fn act(&self, action: Action) {
        self.with_bus("an action", |bus, _| {
            bus.spawn("logind action", async move {
                let conn = bus::system().await?;
                let manager = ManagerProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
                match action {
                    Action::Suspend => manager.suspend(true).await,
                    Action::Hibernate => manager.hibernate(true).await,
                    Action::HybridSleep => manager.hybrid_sleep(true).await,
                    Action::Reboot => manager.reboot(true).await,
                    Action::PowerOff => manager.power_off(true).await,
                }
            });
        });
    }

    /// Asks logind to lock this session: every listener to the request (a
    /// lock screen, this interface) hears it.
    pub fn lock_session(&self) {
        self.with_bus("lock_session", |bus, _| {
            bus.spawn("logind lock", async move {
                let conn = bus::system().await?;
                let session = OwnSessionProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
                session.lock().await
            });
        });
    }

    /// Holds an inhibitor: `what` ("sleep", "shutdown", "idle", "sleep:idle",
    /// "handle-lid-switch"…), `why` as shown to the user, `mode` "block" or
    /// "delay". The handle releases it (`release`).
    pub fn inhibit(&self, what: String, why: String, mode: String, who: String) -> u64 {
        let handle = self.next.get();
        self.next.set(handle + 1);
        self.with_bus("inhibit", |bus, post| {
            bus.spawn("logind inhibit", async move {
                let conn = bus::system().await?;
                let manager = ManagerProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
                let fd = manager.inhibit(&what, &who, &why, &mode).await?;
                let _ = post.send(Update::Held(handle, fd));
                Ok(())
            });
        });
        handle
    }

    /// Lets an inhibitor go. One asked for and not yet given is let go as
    /// it arrives.
    pub fn release(&self, handle: u64) {
        if self.held.borrow_mut().remove(&handle).is_some() {
            self.refresh_inhibitors();
        } else if handle < self.next.get() {
            self.released.borrow_mut().insert(handle);
        }
    }

    /// Whether `handle` is held now.
    pub fn holds(&self, handle: u64) -> bool {
        self.held.borrow().contains_key(&handle)
    }

    fn refresh_inhibitors(&self) {
        self.with_bus("inhibitors", |bus, post| {
            bus.spawn("logind inhibitors", async move {
                let conn = bus::system().await?;
                let manager = ManagerProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
                let _ = post.send(Update::Inhibitors(inhibitors(&manager).await));
                Ok(())
            });
        });
    }
}
