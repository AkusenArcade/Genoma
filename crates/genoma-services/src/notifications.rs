//! Notifications (PRD §9.1): the `org.freedesktop.Notifications` server, as
//! Bioma's `services/Notifications.qml` runs Quickshell's
//! `NotificationServer`: actions, body with markup, images, persistence,
//! and the sound hints kept so a sender's own sound can win.
//!
//! One owner per session: Genoma asks for the name without queueing, so
//! while another server holds it the service says so and serves nothing.
//! The notifications live in the service, not in the interface, so a reload
//! keeps what is on screen.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use calloop::channel::Sender;
use genoma_core::reactive::{Signal, batch};
use zbus::{OwnedVariant, fdo::RequestNameFlags, object_server::SignalEmitter};

use crate::bus::{self, Bus};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// An image a sender gives as pixels (`image-data`), as it gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pixels {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub has_alpha: bool,
    pub channels: i32,
    pub data: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    /// (key, label) pairs, in order; "default" is the click on the body.
    pub actions: Vec<(String, String)>,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    /// Milliseconds; -1 the server decides, 0 never.
    pub expire_timeout: i32,
    pub desktop_entry: String,
    pub category: String,
    /// `image-path`, a file or an icon name.
    pub image_path: String,
    pub image: Option<Pixels>,
    pub resident: bool,
    pub transient: bool,
    /// `value`, a progress 0…100, when given.
    pub value: Option<i32>,
    pub sound_name: String,
    pub sound_file: String,
    pub suppress_sound: bool,
    /// It replaced an earlier one with the same id.
    pub replaced: bool,
}

pub enum Update {
    Serving(Result<(), String>),
    Notify(Box<Notification>),
    /// The sender closed it.
    Closed(u32),
}

pub struct Notifications {
    pub serving: Signal<bool>,
    pub error: Signal<String>,
    /// What is open, in arrival order.
    pub list: Signal<Vec<Notification>>,
    bus: RefCell<Option<Bus>>,
    /// The ids open, shared with the server: a close is signalled once,
    /// and only for one that is open.
    open: Arc<Mutex<HashSet<u32>>>,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications {
            serving: Signal::new(false),
            error: Signal::new(String::new()),
            list: Signal::new(Vec::new()),
            bus: RefCell::new(None),
            open: Arc::default(),
        }
    }
}

struct Server {
    post: Sender<Update>,
    next: Arc<AtomicU32>,
    open: Arc<Mutex<HashSet<u32>>>,
}

fn hint<T: TryFrom<OwnedVariant>>(hints: &HashMap<String, OwnedVariant>, keys: &[&str]) -> Option<T> {
    keys.iter().find_map(|k| hints.get(*k).and_then(|v| T::try_from(v.clone()).ok()))
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "body-markup", "body-images", "icon-static", "persistence", "sound"]
    }

    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("Genoma", "Bioma", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedVariant>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 { replaces_id } else { self.next.fetch_add(1, Ordering::Relaxed) };
        let image = hint::<(i32, i32, i32, bool, i32, i32, Vec<u8>)>(&hints, &["image-data", "image_data", "icon_data"])
            .map(|(width, height, rowstride, has_alpha, _, channels, data)| Pixels {
                width,
                height,
                rowstride,
                has_alpha,
                channels,
                data: Arc::new(data),
            });
        let notification = Notification {
            id,
            app_name,
            app_icon,
            summary,
            body,
            actions: actions.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0].clone(), p[1].clone())).collect(),
            urgency: hint::<u8>(&hints, &["urgency"]).unwrap_or(1),
            expire_timeout,
            desktop_entry: hint(&hints, &["desktop-entry"]).unwrap_or_default(),
            category: hint(&hints, &["category"]).unwrap_or_default(),
            image_path: hint(&hints, &["image-path", "image_path"]).unwrap_or_default(),
            image,
            resident: hint(&hints, &["resident"]).unwrap_or(false),
            transient: hint(&hints, &["transient"]).unwrap_or(false),
            value: hint(&hints, &["value"]),
            sound_name: hint(&hints, &["sound-name"]).unwrap_or_default(),
            sound_file: hint(&hints, &["sound-file"]).unwrap_or_default(),
            suppress_sound: hint(&hints, &["suppress-sound"]).unwrap_or(false),
            replaced: replaces_id != 0,
        };
        self.open.lock().unwrap().insert(id);
        let _ = self.post.send(Update::Notify(Box::new(notification)));
        id
    }

    /// One that is no longer open gets an error and no signal, as the
    /// specification asks (notify-send --wait closes what an action
    /// already closed).
    async fn close_notification(&self, id: u32, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        if !self.open.lock().unwrap().remove(&id) {
            return Err(zbus::fdo::Error::Failed(String::new()));
        }
        let _ = self.post.send(Update::Closed(id));
        // 3: closed by a call to CloseNotification.
        let _ = Self::notification_closed(&emitter, id, 3).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: &str) -> zbus::Result<()>;
}

impl Notifications {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Serving(Ok(())) => self.serving.set(true),
            Update::Serving(Err(e)) => self.error.set(e),
            Update::Notify(n) => {
                let mut list = self.list.get_untracked().unwrap_or_default();
                match list.iter_mut().find(|m| m.id == n.id) {
                    Some(m) => *m = *n,
                    None => list.push(*n),
                }
                self.list.set(list);
            }
            Update::Closed(id) => self.remove(id),
        });
    }

    fn remove(&self, id: u32) {
        let mut list = self.list.get_untracked().unwrap_or_default();
        let before = list.len();
        list.retain(|n| n.id != id);
        if list.len() != before {
            self.list.set(list);
        }
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some(bus.clone());
        let open = self.open.clone();
        bus.spawn("notifications", async move {
            let result: zbus::Result<()> = async {
                let conn = bus::session().await?;
                let server = Server { post: post.clone(), next: Arc::new(AtomicU32::new(1)), open };
                conn.object_server().at(PATH, server).await?;
                // No queue: while another server holds the name, Genoma
                // does not wait in line to take it later.
                let reply = conn.request_name_with_flags(NAME, RequestNameFlags::DO_NOT_QUEUE).await;
                let owned = matches!(
                    reply,
                    Ok(zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner)
                );
                if !owned {
                    // Nothing served under a name another holds.
                    let _ = conn.object_server().remove::<Server, _>(PATH).await;
                    return Err(zbus::Error::Failure("another notification server owns the session's name".into()));
                }
                Ok(())
            }
            .await;
            let _ = post.send(Update::Serving(result.map_err(|e| e.to_string())));
            Ok(())
        });
    }

    fn signal(&self, name: &'static str, work: impl FnOnce(SignalEmitter<'static>) -> futures_util::future::BoxFuture<'static, zbus::Result<()>> + Send + 'static) {
        let Some(bus) = self.bus.borrow().clone() else { return };
        bus.spawn(name, async move {
            let conn = bus::session().await?;
            let emitter = SignalEmitter::new(&conn, PATH)?.into_owned();
            work(emitter).await
        });
    }

    /// Closes it: `reason` "expired" when its time ran out, "dismissed" when
    /// the person closed it.
    pub fn close(&self, id: u32, reason: &str) {
        self.remove(id);
        if !self.open.lock().unwrap().remove(&id) {
            return;
        }
        let code = if reason == "expired" { 1 } else { 2 };
        self.signal("notification closed", move |e| {
            Box::pin(async move { Server::notification_closed(&e, id, code).await })
        });
    }

    /// The person chose the action `key`. A resident notification stays;
    /// any other is dismissed after, as the specification asks.
    pub fn invoke(&self, id: u32, key: String) {
        let resident = self.list.get_untracked().unwrap_or_default().iter().any(|n| n.id == id && n.resident);
        self.signal("action invoked", move |e| {
            Box::pin(async move { Server::action_invoked(&e, id, &key).await })
        });
        if !resident {
            self.close(id, "dismissed");
        }
    }
}
