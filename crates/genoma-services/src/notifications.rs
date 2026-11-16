//! Notifications (PRD §9.1): the `org.freedesktop.Notifications` server, as
//! Bioma's `services/Notifications.qml` runs Quickshell's
//! `NotificationServer`: actions, body with markup, images, persistence,
//! and the sound hints kept so a sender's own sound can win.
//!
//! Beyond the specification's core: inline replies (KDE's `inline-reply`
//! and `NotificationReplied`), the activation token of version 1.2 so the
//! sender's window takes the focus, rules by application (muted, silent,
//! urgency, timeout), and a history kept on request, in memory or on disk.
//! An image a sender gives only as pixels is written as a PNG, so the
//! interface draws it as it draws any picture.
//!
//! One owner per session: Genoma asks for the name without queueing, so
//! while another server holds it the service says so and serves nothing.
//! The notifications live in the service, not in the interface, so a reload
//! keeps what is on screen.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
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
/// The action key KDE's inline replies travel under.
const REPLY: &str = "inline-reply";

/// An image a sender gives as pixels (`image-data`), as it gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pixels {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub has_alpha: bool,
    pub channels: i32,
    pub data: Vec<u8>,
}

impl Pixels {
    /// As a PNG at `path`. Only 8 bits a sample, RGB or RGBA, as the
    /// specification allows.
    pub fn write_png(&self, path: &Path) -> Result<(), String> {
        let (w, h, stride, channels) = (self.width as usize, self.height as usize, self.rowstride as usize, self.channels as usize);
        if w == 0 || h == 0 || !matches!(channels, 3 | 4) || (channels == 4) != self.has_alpha {
            return Err(format!("unreadable image-data: {w}×{h}, {channels} channels"));
        }
        if stride < w * channels || self.data.len() < stride * (h - 1) + w * channels {
            return Err("image-data shorter than its size".into());
        }
        let mut rows = Vec::with_capacity(w * h * channels);
        for row in 0..h {
            rows.extend_from_slice(&self.data[row * stride..row * stride + w * channels]);
        }
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
        encoder.set_color(if channels == 4 { png::ColorType::Rgba } else { png::ColorType::Rgb });
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().and_then(|mut writer| writer.write_image_data(&rows)).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    /// (key, label) pairs, in order; "default" is the click on the body.
    /// An inline reply is not among them: see `can_reply`.
    pub actions: Vec<(String, String)>,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    /// Milliseconds; -1 the server decides, 0 never.
    pub expire_timeout: i32,
    pub desktop_entry: String,
    pub category: String,
    /// `image-path`, a file or an icon name.
    pub image_path: String,
    /// The sender's `image-data`, written as a PNG; empty when it gave none.
    pub image: String,
    pub resident: bool,
    pub transient: bool,
    /// `value`, a progress 0…100, when given.
    pub value: Option<i32>,
    pub sound_name: String,
    pub sound_file: String,
    pub suppress_sound: bool,
    /// It replaced an earlier one with the same id.
    pub replaced: bool,
    /// The sender takes a typed answer (`inline-reply`).
    pub can_reply: bool,
    /// What the reply field says while empty, when the sender says.
    pub reply_placeholder: String,
    /// When it arrived, Unix seconds.
    pub at: i64,
}

/// What a rule does to an application's notifications. An application is
/// matched by name or desktop entry, without regard to case.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rule {
    pub app: String,
    /// Not shown, not kept: closed as soon as it comes.
    pub mute: bool,
    /// Shown without a sound.
    pub silent: bool,
    pub urgency: Option<u8>,
    /// Milliseconds, in place of the sender's.
    pub timeout: Option<i32>,
}

/// A notification as the history keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    /// Unique across runs: ids start again at 1 with every server.
    pub key: String,
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub desktop_entry: String,
    pub summary: String,
    pub body: String,
    pub image_path: String,
    pub image: String,
    pub urgency: u8,
    pub at: i64,
}

pub enum Update {
    Serving(Result<(), String>),
    Notify(Box<Notification>),
    /// The sender closed it.
    Closed(u32),
}

/// Where the history lives and how much of it.
struct Keeping {
    limit: usize,
    /// The file, when it is kept on disk; its pictures beside it, in
    /// `<file>.images/`.
    file: Option<PathBuf>,
}

pub struct Notifications {
    pub serving: Signal<bool>,
    pub error: Signal<String>,
    /// What is open, in arrival order.
    pub list: Signal<Vec<Notification>>,
    /// What has been said, newest first, once `keep` asks for it.
    pub history: Signal<Vec<Kept>>,
    bus: RefCell<Option<Bus>>,
    /// The ids open, shared with the server: a close is signalled once,
    /// and only for one that is open.
    open: Arc<Mutex<HashSet<u32>>>,
    rules: RefCell<Vec<Rule>>,
    keeping: RefCell<Option<Keeping>>,
    /// This run's mark in history keys.
    run: i64,
    /// Pictures of this run's notifications, written on the bus thread.
    images: PathBuf,
    kept_count: Cell<u64>,
}

impl Default for Notifications {
    fn default() -> Self {
        let run = now();
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        Notifications {
            serving: Signal::new(false),
            error: Signal::new(String::new()),
            list: Signal::new(Vec::new()),
            history: Signal::new(Vec::new()),
            bus: RefCell::new(None),
            open: Arc::default(),
            rules: RefCell::default(),
            keeping: RefCell::new(None),
            run,
            images: runtime.join("genoma").join(format!("notifications-{}", std::process::id())),
            kept_count: Cell::new(0),
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

struct Server {
    post: Sender<Update>,
    next: Arc<AtomicU32>,
    open: Arc<Mutex<HashSet<u32>>>,
    images: PathBuf,
    /// Each picture a name of its own: a replaced notification's new
    /// picture must not be the file the interface still caches.
    written: AtomicU32,
}

/// The vendor is the shell, not the toolkit: the program Genoma runs as
/// (`exec -a bioma genoma …` answers "Bioma"), and "Genoma" when started
/// under its own name.
fn vendor() -> String {
    let name = std::env::args_os()
        .next()
        .and_then(|a| std::path::Path::new(&a).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    vendor_of(&name)
}

fn vendor_of(name: &str) -> String {
    if name.is_empty() || name == "genoma" {
        return "Genoma".into();
    }
    let mut chars = name.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn hint<T: TryFrom<OwnedVariant>>(hints: &HashMap<String, OwnedVariant>, keys: &[&str]) -> Option<T> {
    keys.iter().find_map(|k| hints.get(*k).and_then(|v| T::try_from(v.clone()).ok()))
}

/// The pictures folders of servers no longer running, left by one that
/// did not end cleanly.
fn sweep_stale(images: &Path) {
    let Some(parent) = images.parent() else { return };
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("notifications-").and_then(|p| p.parse::<u32>().ok()) else { continue };
        if pid != std::process::id() && !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "body-markup", "body-images", "icon-static", "persistence", "sound", REPLY]
    }

    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    fn get_server_information(&self) -> (&'static str, String, &'static str, &'static str) {
        ("Genoma", vendor(), env!("CARGO_PKG_VERSION"), "1.2")
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
            .map(|(width, height, rowstride, has_alpha, _, channels, data)| Pixels { width, height, rowstride, has_alpha, channels, data })
            .and_then(|pixels| {
                let path = self.images.join(format!("{id}-{}.png", self.written.fetch_add(1, Ordering::Relaxed)));
                let written = std::fs::create_dir_all(&self.images).map_err(|e| e.to_string()).and_then(|()| pixels.write_png(&path));
                match written {
                    Ok(()) => Some(path.to_string_lossy().into_owned()),
                    Err(e) => {
                        log::info!("notifications: {e}");
                        None
                    }
                }
            })
            .unwrap_or_default();
        let pairs: Vec<(String, String)> = actions.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0].clone(), p[1].clone())).collect();
        let can_reply = pairs.iter().any(|(k, _)| k == REPLY);
        let notification = Notification {
            id,
            app_name,
            app_icon,
            summary,
            body,
            actions: pairs.into_iter().filter(|(k, _)| k != REPLY).collect(),
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
            can_reply,
            reply_placeholder: hint(&hints, &["x-kde-reply-placeholder-text"]).unwrap_or_default(),
            at: now(),
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
    #[zbus(signal)]
    async fn activation_token(emitter: &SignalEmitter<'_>, id: u32, activation_token: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn notification_replied(emitter: &SignalEmitter<'_>, id: u32, text: &str) -> zbus::Result<()>;
}

/// The rule for an application, the first that names it.
pub fn rule_for<'a>(rules: &'a [Rule], app_name: &str, desktop_entry: &str) -> Option<&'a Rule> {
    rules.iter().find(|r| {
        let app = r.app.to_lowercase();
        !app.is_empty() && (app == app_name.to_lowercase() || app == desktop_entry.to_lowercase())
    })
}

/// A notification as a rule changes it; none when the rule mutes it.
pub fn ruled(mut n: Notification, rule: Option<&Rule>) -> Option<Notification> {
    let Some(rule) = rule else { return Some(n) };
    if rule.mute {
        return None;
    }
    if rule.silent {
        n.suppress_sound = true;
    }
    if let Some(u) = rule.urgency {
        n.urgency = u.min(2);
    }
    if let Some(t) = rule.timeout {
        n.expire_timeout = t;
    }
    Some(n)
}

fn kept_to_json(k: &Kept) -> serde_json::Value {
    serde_json::json!({
        "key": k.key, "id": k.id, "app_name": k.app_name, "app_icon": k.app_icon,
        "desktop_entry": k.desktop_entry, "summary": k.summary, "body": k.body,
        "image_path": k.image_path, "image": k.image, "urgency": k.urgency, "at": k.at,
    })
}

fn kept_from_json(v: &serde_json::Value) -> Option<Kept> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_owned();
    Some(Kept {
        key: v.get("key")?.as_str()?.to_owned(),
        id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        app_name: s("app_name"),
        app_icon: s("app_icon"),
        desktop_entry: s("desktop_entry"),
        summary: s("summary"),
        body: s("body"),
        image_path: s("image_path"),
        image: s("image"),
        urgency: v.get("urgency").and_then(|x| x.as_u64()).unwrap_or(1).min(2) as u8,
        at: v.get("at").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

impl Notifications {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Serving(Ok(())) => self.serving.set(true),
            Update::Serving(Err(e)) => self.error.set(e),
            Update::Notify(n) => {
                let (id, image) = (n.id, n.image.clone());
                let rule = rule_for(&self.rules.borrow(), &n.app_name, &n.desktop_entry).cloned();
                let Some(n) = ruled(*n, rule.as_ref()) else {
                    // Muted: whatever it replaced goes too, and the sender
                    // hears it was dismissed.
                    self.close(id, "dismissed");
                    self.release(&image);
                    return;
                };
                if !n.transient {
                    self.remember(&n);
                }
                let mut list = self.list.get_untracked().unwrap_or_default();
                let mut gone = None;
                match list.iter_mut().find(|m| m.id == n.id) {
                    Some(m) => {
                        if m.image != n.image {
                            gone = Some(std::mem::take(&mut m.image));
                        }
                        *m = n;
                    }
                    None => list.push(n),
                }
                self.list.set(list);
                if let Some(image) = gone {
                    self.release(&image);
                }
            }
            Update::Closed(id) => self.remove(id),
        });
    }

    fn remove(&self, id: u32) {
        let mut list = self.list.get_untracked().unwrap_or_default();
        let gone: Vec<String> = list.iter().filter(|n| n.id == id).map(|n| n.image.clone()).collect();
        list.retain(|n| n.id != id);
        if !gone.is_empty() {
            self.list.set(list);
        }
        for image in gone {
            self.release(&image);
        }
    }

    /// A picture no notification and no history entry shows any more,
    /// removed.
    fn release(&self, image: &str) {
        if image.is_empty() {
            return;
        }
        let shown = self.list.get_untracked().unwrap_or_default().iter().any(|n| n.image == image)
            || self.history.get_untracked().unwrap_or_default().iter().any(|k| k.image == image);
        if !shown {
            let _ = std::fs::remove_file(image);
        }
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some(bus.clone());
        sweep_stale(&self.images);
        let open = self.open.clone();
        let images = self.images.clone();
        bus.spawn("notifications", async move {
            let result: zbus::Result<()> = async {
                let conn = bus::session().await?;
                let server = Server { post: post.clone(), next: Arc::new(AtomicU32::new(1)), open, images, written: AtomicU32::new(0) };
                conn.object_server().at(PATH, server).await?;
                // No queue: while another server holds the name, Genoma says
                // so and serves nothing; when that one lets it go (a shell
                // on its way out), Genoma takes it.
                loop {
                    let reply = conn.request_name_with_flags(NAME, RequestNameFlags::DO_NOT_QUEUE).await;
                    let owned = matches!(
                        reply,
                        Ok(zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner)
                    );
                    if owned {
                        return Ok(());
                    }
                    let _ = post.send(Update::Serving(Err("another notification server owns the session's name".into())));
                    bus::until_free(&conn, NAME).await?;
                }
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

    /// The person chose the action `key`. With an activation token (from
    /// the compositor, for the sender's window to take the focus), the
    /// token is said first, as specification 1.2 orders. A resident
    /// notification stays; any other is dismissed after.
    pub fn invoke(&self, id: u32, key: String, token: Option<String>) {
        let resident = self.list.get_untracked().unwrap_or_default().iter().any(|n| n.id == id && n.resident);
        self.signal("action invoked", move |e| {
            Box::pin(async move {
                if let Some(token) = token {
                    Server::activation_token(&e, id, &token).await?;
                }
                Server::action_invoked(&e, id, &key).await
            })
        });
        if !resident {
            self.close(id, "dismissed");
        }
    }

    /// The person answered it in words; dismissed after unless resident.
    pub fn reply(&self, id: u32, text: String) -> Result<(), String> {
        let list = self.list.get_untracked().unwrap_or_default();
        let Some(n) = list.iter().find(|n| n.id == id) else { return Err(format!("no notification {id}")) };
        if !n.can_reply {
            return Err(format!("notification {id} takes no reply"));
        }
        let resident = n.resident;
        self.signal("notification replied", move |e| {
            Box::pin(async move { Server::notification_replied(&e, id, &text).await })
        });
        if !resident {
            self.close(id, "dismissed");
        }
        Ok(())
    }

    /// The rules from now on; those already shown are left as they came.
    pub fn set_rules(&self, rules: Vec<Rule>) {
        *self.rules.borrow_mut() = rules;
    }

    /// Keeps a history of up to `limit` notifications, newest first; with
    /// a `file`, on disk, read back now and written on every change.
    /// Transient ones are not kept, muted ones are not seen at all.
    pub fn keep(&self, limit: usize, file: Option<PathBuf>) -> Result<(), String> {
        let mut history = Vec::new();
        if let Some(file) = &file {
            match std::fs::read_to_string(file) {
                Ok(text) => {
                    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
                    history = value.as_array().map(|a| a.iter().filter_map(kept_from_json).collect()).unwrap_or_default();
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", file.display())),
            }
        }
        history.truncate(limit);
        *self.keeping.borrow_mut() = Some(Keeping { limit, file });
        self.set_history(history);
        self.sweep_kept_images();
        Ok(())
    }

    /// Forgets one entry by key, or all of them.
    pub fn forget(&self, key: Option<&str>) {
        let mut history = self.history.get_untracked().unwrap_or_default();
        let gone: Vec<String> = history.iter().filter(|k| key.is_none_or(|key| k.key == key)).map(|k| k.image.clone()).collect();
        history.retain(|k| key.is_some_and(|key| k.key != key));
        self.set_history(history);
        for image in gone {
            self.release(&image);
        }
    }

    fn remember(&self, n: &Notification) {
        let (limit, file) = match &*self.keeping.borrow() {
            Some(k) => (k.limit, k.file.clone()),
            None => return,
        };
        let mut history = self.history.get_untracked().unwrap_or_default();
        // A replaced one takes its earlier entry's place.
        let replaced = if n.replaced { history.iter().position(|k| k.id == n.id && k.key.starts_with(&format!("{}-", self.run))) } else { None };
        let key = match replaced {
            Some(i) => history.remove(i).key,
            None => {
                self.kept_count.set(self.kept_count.get() + 1);
                format!("{}-{}", self.run, self.kept_count.get())
            }
        };
        // On disk, the picture is copied beside the file: the live one goes
        // with the notification.
        let image = match (&file, n.image.is_empty()) {
            (Some(file), false) => {
                let folder = images_of(file);
                let copy = folder.join(format!("{key}.png"));
                match std::fs::create_dir_all(&folder).and_then(|()| std::fs::copy(&n.image, &copy)) {
                    Ok(_) => copy.to_string_lossy().into_owned(),
                    Err(e) => {
                        log::info!("notifications: keeping {}: {e}", n.image);
                        String::new()
                    }
                }
            }
            _ => n.image.clone(),
        };
        history.insert(
            0,
            Kept {
                key,
                id: n.id,
                app_name: n.app_name.clone(),
                app_icon: n.app_icon.clone(),
                desktop_entry: n.desktop_entry.clone(),
                summary: n.summary.clone(),
                body: n.body.clone(),
                image_path: n.image_path.clone(),
                image,
                urgency: n.urgency,
                at: n.at,
            },
        );
        let dropped: Vec<String> = history.iter().skip(limit).map(|k| k.image.clone()).collect();
        history.truncate(limit);
        self.set_history(history);
        for image in dropped {
            self.release(&image);
        }
    }

    fn set_history(&self, history: Vec<Kept>) {
        if self.history.get_untracked().as_ref() == Some(&history) {
            return;
        }
        let file = self.keeping.borrow().as_ref().and_then(|k| k.file.clone());
        if let Some(file) = file {
            let json = serde_json::Value::Array(history.iter().map(kept_to_json).collect());
            let written = file
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&file, serde_json::to_string_pretty(&json).unwrap_or_default()));
            if let Err(e) = written {
                log::warn!("notifications: {}: {e}", file.display());
            }
        }
        self.history.set(history);
    }

    /// Pictures beside the history file that no entry names, removed.
    fn sweep_kept_images(&self) {
        let Some(file) = self.keeping.borrow().as_ref().and_then(|k| k.file.clone()) else { return };
        let history = self.history.get_untracked().unwrap_or_default();
        let Ok(entries) = std::fs::read_dir(images_of(&file)) else { return };
        for entry in entries.flatten() {
            let path = entry.path().to_string_lossy().into_owned();
            if !history.iter().any(|k| k.image == path) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn images_of(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".images");
    file.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_is_the_program() {
        assert_eq!(vendor_of("bioma"), "Bioma");
        assert_eq!(vendor_of("genoma"), "Genoma");
        assert_eq!(vendor_of(""), "Genoma");
        assert_eq!(vendor_of("my-shell"), "My-shell");
    }

    fn sample(id: u32, app: &str) -> Notification {
        Notification {
            id,
            app_name: app.into(),
            app_icon: String::new(),
            summary: format!("s{id}"),
            body: String::new(),
            actions: Vec::new(),
            urgency: 1,
            expire_timeout: -1,
            desktop_entry: String::new(),
            category: String::new(),
            image_path: String::new(),
            image: String::new(),
            resident: false,
            transient: false,
            value: None,
            sound_name: String::new(),
            sound_file: String::new(),
            suppress_sound: false,
            replaced: false,
            can_reply: false,
            reply_placeholder: String::new(),
            at: 0,
        }
    }

    #[test]
    fn rules() {
        let rules = vec![
            Rule { app: "Spotify".into(), silent: true, urgency: Some(0), ..Rule::default() },
            Rule { app: "discord".into(), mute: true, ..Rule::default() },
        ];
        let mut d = sample(1, "Discord");
        assert!(ruled(d.clone(), rule_for(&rules, &d.app_name, &d.desktop_entry)).is_none(), "muted, case aside");
        d.app_name = "Something".into();
        d.desktop_entry = "discord".into();
        assert!(ruled(d.clone(), rule_for(&rules, &d.app_name, &d.desktop_entry)).is_none(), "by desktop entry too");
        let s = sample(2, "spotify");
        let s = ruled(s.clone(), rule_for(&rules, &s.app_name, &s.desktop_entry)).unwrap();
        assert!(s.suppress_sound);
        assert_eq!(s.urgency, 0);
        let other = sample(3, "Firefox");
        assert_eq!(ruled(other.clone(), rule_for(&rules, "Firefox", "")), Some(other));
    }

    #[test]
    fn pixels_as_png() {
        let dir = std::env::temp_dir().join(format!("genoma-test-pixels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        // 2×2 RGB with rows padded to 8 bytes.
        let pixels = Pixels { width: 2, height: 2, rowstride: 8, has_alpha: false, channels: 3, data: vec![255, 0, 0, 0, 255, 0, 9, 9, 0, 0, 255, 255, 255, 255, 9, 9] };
        pixels.write_png(&path).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..12], &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
        let bad = Pixels { channels: 4, has_alpha: false, ..pixels };
        assert!(bad.write_png(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn history_on_disk() {
        let dir = std::env::temp_dir().join(format!("genoma-test-history-{}", std::process::id()));
        let file = dir.join("history.json");
        let picture = dir.join("live.png");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&picture, b"png").unwrap();

        let n = Notifications::default();
        n.keep(2, Some(file.clone())).unwrap();
        let mut a = sample(1, "A");
        a.image = picture.to_string_lossy().into_owned();
        n.apply(Update::Notify(Box::new(a)));
        n.apply(Update::Notify(Box::new(sample(2, "B"))));
        let mut transient = sample(3, "C");
        transient.transient = true;
        n.apply(Update::Notify(Box::new(transient)));
        let h = n.history.get_untracked().unwrap();
        assert_eq!(h.iter().map(|k| k.summary.as_str()).collect::<Vec<_>>(), ["s2", "s1"], "newest first, transient left out");
        assert!(h[1].image.ends_with(".png") && h[1].image.contains("history.json.images"), "the picture copied beside the file");
        let mut replaced = sample(2, "B");
        replaced.summary = "s2 again".into();
        replaced.replaced = true;
        n.apply(Update::Notify(Box::new(replaced)));
        assert_eq!(n.history.get_untracked().unwrap().len(), 2, "a replacement takes its entry's place");

        // Read back by another run.
        let again = Notifications::default();
        again.keep(10, Some(file.clone())).unwrap();
        let h = again.history.get_untracked().unwrap();
        assert_eq!(h.iter().map(|k| k.summary.as_str()).collect::<Vec<_>>(), ["s2 again", "s1"]);
        let copy = h[1].image.clone();
        assert!(Path::new(&copy).exists());
        again.forget(Some(&h[1].key));
        assert!(!Path::new(&copy).exists(), "a forgotten entry's picture goes");
        again.forget(None);
        assert!(again.history.get_untracked().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
