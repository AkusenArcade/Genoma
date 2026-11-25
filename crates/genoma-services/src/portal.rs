//! The desktop portal (`@genoma/portal`, xdg-desktop-portal on the session
//! bus): what the desktop asks of every application, read the way the
//! applications read it, so a shell follows the same preferences they do.
//!
//! Settings: `org.freedesktop.appearance` — the colour scheme, the accent
//! colour, high contrast and reduced motion — read once and followed on
//! `SettingChanged`. Nothing polls. The icon theme's name is followed the
//! same way, from the desktop's own settings (`org.gnome.desktop.interface`,
//! where every portal backend publishes it).
//!
//! File chooser: the desktop's own dialog to open or save files, the one
//! the applications show. Each choice is a request whose `Response` is
//! subscribed to before it is made (its path follows from the
//! `handle_token`), and the answer is handed to whoever asked.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{OwnedObjectPath, Variant as Value, zvariant::OwnedVariant};

use crate::bus::{self, Bus};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";
const INTERFACE: &str = "org.gnome.desktop.interface";
const ICON_THEME: &str = "icon-theme";
const CHOOSER: &str = "org.freedesktop.portal.FileChooser";
const SCREENSHOT: &str = "org.freedesktop.portal.Screenshot";
const REQUEST: &str = "org.freedesktop.portal.Request";

/// What the desktop prefers, as the portal says it.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    /// "dark", "light", or "default" (no preference).
    pub color_scheme: &'static str,
    /// "#rrggbb", or none where the desktop sets no accent.
    pub accent: Option<String>,
    pub high_contrast: bool,
    pub reduced_motion: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { color_scheme: "default", accent: None, high_contrast: false, reduced_motion: false }
    }
}

/// A group of files the dialog can be narrowed to: "Images", with glob
/// patterns ("*.png") and MIME types ("image/png").
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub name: String,
    pub patterns: Vec<String>,
    pub mime: Vec<String>,
}

/// What a file chooser is asked for.
#[derive(Clone, Debug, Default)]
pub struct Ask {
    /// Save rather than open.
    pub save: bool,
    pub title: String,
    /// The label of the button that accepts, else the dialog's own.
    pub accept: Option<String>,
    /// Open: more than one file.
    pub multiple: bool,
    /// Open: folders rather than files.
    pub folders: bool,
    pub filters: Vec<Filter>,
    /// The folder it starts in.
    pub folder: Option<String>,
    /// Save: the name it proposes.
    pub name: Option<String>,
}

/// A request to the portal that answers with files.
enum Asked {
    File(Ask),
    /// A picture of the screen; interactive lets the person pick what.
    Screenshot(bool),
}

/// The paths chosen, none when the dialog was dismissed, or why it failed.
pub type Chosen = Result<Option<Vec<String>>, String>;

type Waiting = Box<dyn FnOnce(Chosen)>;

pub enum Update {
    Available(bool),
    Setting(String, OwnedVariant),
    IconTheme(String),
    Chosen(u64, Chosen),
}

pub struct Portal {
    /// Whether a portal answers on the session bus.
    pub available: Signal<bool>,
    pub appearance: Signal<Appearance>,
    /// The icon theme's name, "" until it is known.
    pub icon_theme: Signal<String>,
    bus: RefCell<Option<(Bus, Sender<Update>)>>,
    /// Choices asked for and not answered yet, by request number.
    waiting: RefCell<HashMap<u64, Waiting>>,
    /// Asked for before the service started (during the load that
    /// required it), made once it does.
    queued: RefCell<Vec<(u64, Asked)>>,
    next: Cell<u64>,
}

impl Default for Portal {
    fn default() -> Self {
        Portal {
            available: Signal::new(false),
            appearance: Signal::new(Appearance::default()),
            icon_theme: Signal::new(String::new()),
            bus: RefCell::new(None),
            waiting: RefCell::default(),
            queued: RefCell::default(),
            next: Cell::new(1),
        }
    }
}

/// A `file://` URI as a path; any other URI is left as it is.
fn path_of(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else { return uri.to_owned() };
    // file://host/path: the host, usually empty or localhost, goes.
    let rest = if rest.starts_with('/') { rest } else { rest.find('/').map_or(rest, |at| &rest[at..]) };
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut k = 0;
    while k < bytes.len() {
        let hex = bytes.get(k + 1..k + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[k], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                k += 3;
            }
            (b, _) => {
                out.push(b);
                k += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The request's options, as `OpenFile` and `SaveFile` take them.
fn options(ask: &Ask, token: &str) -> HashMap<&'static str, Value<'static>> {
    let mut o: HashMap<&'static str, Value<'static>> = HashMap::new();
    o.insert("handle_token", Value::from(token.to_owned()));
    o.insert("modal", Value::from(false));
    if let Some(label) = &ask.accept {
        o.insert("accept_label", Value::from(label.clone()));
    }
    if !ask.save {
        o.insert("multiple", Value::from(ask.multiple));
        o.insert("directory", Value::from(ask.folders));
    }
    if !ask.filters.is_empty() {
        // a(sa(us)): 0 a glob pattern, 1 a MIME type.
        let filters: Vec<(String, Vec<(u32, String)>)> = ask
            .filters
            .iter()
            .map(|f| (f.name.clone(), f.patterns.iter().map(|p| (0, p.clone())).chain(f.mime.iter().map(|m| (1, m.clone()))).collect()))
            .collect();
        o.insert("filters", Value::from(filters));
    }
    if let Some(folder) = &ask.folder {
        // A NUL-terminated byte string.
        let mut bytes = folder.as_bytes().to_vec();
        bytes.push(0);
        o.insert("current_folder", Value::from(bytes));
    }
    if let (true, Some(name)) = (ask.save, &ask.name) {
        o.insert("current_name", Value::from(name.clone()));
    }
    o
}

fn ask_portal(bus: &Bus, post: Sender<Update>, id: u64, ask: Asked) {
    // Unique among this connection's requests.
    let token = format!("genoma{id}");
    bus.spawn("portal request", async move {
        let chosen = choose(ask, token).await.unwrap_or_else(|e| Err(e.to_string()));
        let _ = post.send(Update::Chosen(id, chosen));
        Ok(())
    });
}

/// Asks the portal and waits for the answer.
async fn choose(ask: Asked, token: String) -> zbus::Result<Chosen> {
    let conn = bus::session().await?;
    let sender = conn.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
    let expected = format!("{PATH}/request/{sender}/{token}");
    // Subscribed before the call, so a quick answer is not missed.
    let request = zbus::Proxy::new(&conn, PORTAL, expected.clone(), REQUEST).await?;
    let mut responses = request.receive_signal("Response").await?;
    let call = match &ask {
        Asked::File(ask) => {
            let chooser = zbus::Proxy::new(&conn, PORTAL, PATH, CHOOSER).await?;
            let method = if ask.save { "SaveFile" } else { "OpenFile" };
            chooser.call(method, &("", ask.title.as_str(), options(ask, &token))).await
        }
        Asked::Screenshot(interactive) => {
            let screenshot = zbus::Proxy::new(&conn, PORTAL, PATH, SCREENSHOT).await?;
            let options: HashMap<&str, Value<'_>> =
                HashMap::from([("handle_token", Value::from(token.as_str())), ("interactive", Value::from(*interactive)), ("modal", Value::from(false))]);
            screenshot.call("Screenshot", &("", options)).await
        }
    };
    let handle: OwnedObjectPath = match call {
        Ok(handle) => handle,
        Err(e) => return Ok(Err(e.to_string())),
    };
    // A portal older than handle_token answers on a path of its own.
    let message = if handle.as_str() == expected {
        responses.next().await
    } else {
        zbus::Proxy::new(&conn, PORTAL, handle, REQUEST).await?.receive_signal("Response").await?.next().await
    };
    let Some(message) = message else { return Ok(Err("the portal went away".into())) };
    let (code, results): (u32, HashMap<String, OwnedVariant>) = message.body().deserialize()?;
    Ok(match code {
        0 => Ok(Some(match ask {
            Asked::File(_) => results.get("uris").and_then(|v| Vec::<String>::try_from(v.clone()).ok()).unwrap_or_default(),
            Asked::Screenshot(_) => results.get("uri").and_then(|v| String::try_from(v.clone()).ok()).into_iter().collect(),
        }
        .iter()
        .map(|u| path_of(u))
        .collect())),
        // 1 cancelled, 2 ended some other way (its window closed): both
        // are no choice, not a failure.
        _ => Ok(None),
    })
}

/// One `org.freedesktop.appearance` key into the model; others are left.
pub fn set(appearance: &mut Appearance, key: &str, value: &OwnedVariant) {
    match key {
        "color-scheme" => {
            appearance.color_scheme = match u32::try_from(value.clone()) {
                Ok(1) => "dark",
                Ok(2) => "light",
                _ => "default",
            }
        }
        // Three doubles 0…1; any outside means no accent.
        "accent-color" => {
            appearance.accent = <(f64, f64, f64)>::try_from(value.clone()).ok().and_then(|(r, g, b)| {
                let inside = |c: f64| (0.0..=1.0).contains(&c);
                let byte = |c: f64| (c * 255.0).round() as u8;
                (inside(r) && inside(g) && inside(b)).then(|| format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b)))
            })
        }
        "contrast" => appearance.high_contrast = u32::try_from(value.clone()) == Ok(1),
        "reduced-motion" => appearance.reduced_motion = u32::try_from(value.clone()) == Ok(1),
        _ => {}
    }
}

fn tell(post: &Sender<Update>, namespace: &str, key: String, value: OwnedVariant) {
    if namespace == APPEARANCE {
        let _ = post.send(Update::Setting(key, value));
    } else if namespace == INTERFACE
        && key == ICON_THEME
        && let Ok(name) = String::try_from(value)
    {
        let _ = post.send(Update::IconTheme(name));
    }
}

impl Portal {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Available(on) => self.available.set(on),
            Update::Setting(key, value) => {
                let old = self.appearance.get_untracked().unwrap_or_default();
                let mut new = old.clone();
                set(&mut new, &key, &value);
                if new != old {
                    self.appearance.set(new);
                }
            }
            Update::IconTheme(name) => {
                let before = self.icon_theme.get_untracked().unwrap_or_default();
                if name != before {
                    // Looked up again in the new theme, by whoever hears of
                    // it here. The first name is the one they were found in.
                    if !before.is_empty() {
                        crate::icons::forget();
                    }
                    self.icon_theme.set(name);
                }
            }
            Update::Chosen(id, chosen) => {
                let waiting = self.waiting.borrow_mut().remove(&id);
                if let Some(done) = waiting {
                    done(chosen);
                }
            }
        });
    }

    /// Opens the desktop's file chooser; `done` gets the answer, on the
    /// main loop, once the dialog closes.
    pub fn choose(&self, ask: Ask, done: impl FnOnce(Chosen) + 'static) {
        self.ask(Asked::File(ask), done);
    }

    /// A picture of the screen through the portal, which saves it where
    /// the desktop keeps screenshots; `interactive` lets the person choose
    /// what. `done` gets its path.
    pub fn screenshot(&self, interactive: bool, done: impl FnOnce(Chosen) + 'static) {
        self.ask(Asked::Screenshot(interactive), done);
    }

    fn ask(&self, ask: Asked, done: impl FnOnce(Chosen) + 'static) {
        let id = self.next.get();
        self.next.set(id + 1);
        self.waiting.borrow_mut().insert(id, Box::new(done));
        let started = self.bus.borrow().clone();
        match started {
            Some((bus, post)) => ask_portal(&bus, post, id, ask),
            None => self.queued.borrow_mut().push((id, ask)),
        }
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some((bus.clone(), post.clone()));
        for (id, ask) in self.queued.take() {
            ask_portal(bus, post.clone(), id, ask);
        }
        let pause = bus.clone();
        bus.spawn("portal settings", async move {
            let conn = bus::session().await?;
            let settings = zbus::Proxy::new(&conn, PORTAL, PATH, SETTINGS).await?;
            // Subscribed first, so nothing changes unseen between the two.
            let changes = pause.drained("portal setting changes", settings.receive_signal("SettingChanged").await?);
            let all: HashMap<String, HashMap<String, OwnedVariant>> = match settings.call("ReadAll", &(vec![APPEARANCE, INTERFACE],)).await {
                Ok(all) => all,
                Err(e) => {
                    log::info!("portal: {e}");
                    return Ok(());
                }
            };
            let _ = post.send(Update::Available(true));
            for (namespace, keys) in all {
                for (key, value) in keys {
                    tell(&post, &namespace, key, value);
                }
            }
            let mut changes = changes;
            while let Some(message) = changes.next().await {
                if let Ok((namespace, key, value)) = message.body().deserialize::<(String, String, OwnedVariant)>() {
                    tell(&post, &namespace, key, value);
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_keys() {
        let v = |x: zbus::Variant<'static>| OwnedVariant::try_from(x).unwrap();
        let mut a = Appearance::default();
        set(&mut a, "color-scheme", &v(1u32.into()));
        set(&mut a, "accent-color", &v((0.227451f64, 0.580392f64, 0.290196f64).into()));
        set(&mut a, "contrast", &v(1u32.into()));
        assert_eq!(a, Appearance { color_scheme: "dark", accent: Some("#3a944a".into()), high_contrast: true, reduced_motion: false });
        set(&mut a, "accent-color", &v((-1.0f64, 0.0f64, 0.0f64).into()));
        set(&mut a, "color-scheme", &v(0u32.into()));
        assert_eq!((a.accent, a.color_scheme), (None, "default"), "outside 0…1 is no accent");
    }

    #[test]
    fn uris_as_paths() {
        assert_eq!(path_of("file:///home/a/My%20Pictures/citt%C3%A0.png"), "/home/a/My Pictures/città.png");
        assert_eq!(path_of("file://localhost/tmp/x"), "/tmp/x");
        assert_eq!(path_of("file:///tmp/50%"), "/tmp/50%", "a stray % stays");
        assert_eq!(path_of("sftp://host/x"), "sftp://host/x");
    }

    #[test]
    fn request_options() {
        let ask = Ask {
            title: "Pick".into(),
            multiple: true,
            filters: vec![Filter { name: "Images".into(), patterns: vec!["*.png".into()], mime: vec!["image/jpeg".into()] }],
            folder: Some("/tmp".into()),
            name: Some("ignored on open".into()),
            ..Ask::default()
        };
        let o = options(&ask, "t1");
        assert_eq!(o["handle_token"], Value::from("t1"));
        assert_eq!(o["multiple"], Value::from(true));
        assert_eq!(o["filters"].value_signature().to_string(), "a(sa(us))");
        assert_eq!(o["current_folder"], Value::from(b"/tmp\0".to_vec()));
        assert!(!o.contains_key("current_name"));
        let save = options(&Ask { save: true, name: Some("a.txt".into()), ..Ask::default() }, "t2");
        assert_eq!(save["current_name"], Value::from("a.txt"));
        assert!(!save.contains_key("multiple"));
    }
}
