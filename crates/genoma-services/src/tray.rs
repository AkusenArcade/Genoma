//! Tray (PRD §9.1): StatusNotifierItem, as Bioma's `services/Tray.qml` reads
//! it from `Quickshell.Services.SystemTray`, with DBusMenu for the menus.
//!
//! Genoma is a host: it registers with the watcher and follows the items the
//! watcher lists. When no one serves `org.kde.StatusNotifierWatcher`, Genoma
//! serves it too, as Quickshell does, since an item needs a watcher to be
//! seen at all. An item is read again when it signals a new title, icon,
//! status or tooltip; a menu is read when it is opened and again when its
//! application says its layout changed.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{
    MatchRule, MessageStream, OwnedVariant, message::Type, names::BusName, object_server::SignalEmitter,
    proxy::CacheProperties, wire::OwnedObjectPath,
};

use crate::bus::{self, Bus};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    /// What the watcher lists it as: `bus name` + `object path`. Actions
    /// name it by this.
    pub key: String,
    pub id: String,
    pub title: String,
    /// "Passive", "Active" or "NeedsAttention".
    pub status: String,
    /// "ApplicationStatus", "Communications", "SystemServices", "Hardware".
    pub category: String,
    pub icon_name: String,
    pub icon_theme_path: String,
    pub attention_icon_name: String,
    /// Whether it sends its icon as pixels rather than a name.
    pub has_pixmap: bool,
    /// Those pixels as a PNG file, or empty: the largest size sent, written
    /// once under `$XDG_RUNTIME_DIR/genoma/tray/`, named by its content.
    pub pixmap: String,
    pub tooltip_title: String,
    pub tooltip_description: String,
    /// The primary action is the menu: `activate` must not be called.
    pub only_menu: bool,
    /// The DBusMenu object, or empty.
    pub menu: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    /// "", "checkmark" or "radio".
    pub toggle_type: String,
    /// 1 on, 0 off, -1 neither.
    pub toggle_state: i32,
    pub icon_name: String,
    pub children: Vec<MenuEntry>,
}

pub enum Update {
    Item(Box<Item>),
    Gone(String),
    Menu(String, MenuEntry),
    /// Whether Genoma serves the watcher itself.
    Watcher(bool),
}

pub struct Tray {
    /// In the order they registered.
    pub items: Signal<Vec<Item>>,
    /// The menus that have been opened, by item key.
    pub menus: Signal<HashMap<String, MenuEntry>>,
    pub serving_watcher: Signal<bool>,
    bus: RefCell<Option<Bus>>,
    post: RefCell<Option<Sender<Update>>>,
    open_menus: Arc<Mutex<HashSet<String>>>,
}

impl Default for Tray {
    fn default() -> Self {
        Tray {
            items: Signal::new(Vec::new()),
            menus: Signal::new(HashMap::new()),
            serving_watcher: Signal::new(false),
            bus: RefCell::new(None),
            post: RefCell::new(None),
            open_menus: Arc::default(),
        }
    }
}

// ---- The watcher, when no one else serves it --------------------------------

struct Watcher {
    items: Arc<Mutex<Vec<String>>>,
    hosts: Vec<String>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// `service` is a bus name, or an object path on the caller's own
    /// connection (Ayatana and Electron register that way).
    async fn register_status_notifier_item(
        &mut self,
        service: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let sender = header.sender().map(|s| s.as_str().to_owned()).unwrap_or_default();
        let key = if service.starts_with('/') {
            format!("{sender}{service}")
        } else {
            format!("{service}/StatusNotifierItem")
        };
        let added = {
            let mut items = self.items.lock().unwrap();
            let new = !items.contains(&key);
            if new {
                items.push(key.clone());
            }
            new
        };
        if added {
            let _ = Self::status_notifier_item_registered(&emitter, &key).await;
        }
    }

    async fn register_status_notifier_host(&mut self, service: &str, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.hosts.push(service.to_owned());
        let _ = Self::status_notifier_host_registered(&emitter).await;
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        !self.hosts.is_empty()
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Serves the watcher when its name is free, and drops an item whose
/// connection went away.
async fn serve_watcher(bus: &Bus, conn: &zbus::Connection) -> zbus::Result<bool> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    if dbus.name_has_owner(BusName::try_from(WATCHER)?).await? {
        return Ok(false);
    }
    let items: Arc<Mutex<Vec<String>>> = Arc::default();
    conn.object_server().at(WATCHER_PATH, Watcher { items: items.clone(), hosts: Vec::new() }).await?;
    conn.request_name(WATCHER).await?;
    let conn2 = conn.clone();
    let rule = MatchRule::builder().msg_type(Type::Signal).sender("org.freedesktop.DBus").member("NameOwnerChanged").build()?;
    let mut gone = MessageStream::for_match_rule(rule, conn, None).await?;
    bus.spawn("tray watcher", async move {
        while let Some(Ok(m)) = gone.next().await {
            let Ok((name, _, new)) = m.body().deserialize::<(String, String, String)>() else { continue };
            if !new.is_empty() {
                continue;
            }
            let removed: Vec<String> = {
                let mut list = items.lock().unwrap();
                let (out, keep): (Vec<String>, Vec<String>) =
                    list.drain(..).partition(|k| k.starts_with(&format!("{name}/")));
                *list = keep;
                out
            };
            if let Ok(emitter) = SignalEmitter::new(&conn2, WATCHER_PATH) {
                for key in removed {
                    let _ = Watcher::status_notifier_item_unregistered(&emitter, &key).await;
                }
            }
        }
        Ok(())
    });
    Ok(true)
}

// ---- The host -----------------------------------------------------------------

/// `bus name/object/path` into its two halves.
fn split(key: &str) -> (String, String) {
    match key.find('/') {
        Some(i) => (key[..i].to_owned(), key[i..].to_owned()),
        None => (key.to_owned(), "/StatusNotifierItem".to_owned()),
    }
}

fn text(m: &HashMap<String, OwnedVariant>, k: &str) -> String {
    m.get(k).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default()
}

async fn read_item(conn: &zbus::Connection, key: &str) -> zbus::Result<Item> {
    let (service, path) = split(key);
    let props = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(service)
        .path(path)
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let all: HashMap<String, OwnedVariant> =
        props.get_all(ITEM.try_into()?).await?.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    // ToolTip is (icon name, pixmaps, title, description).
    let (tooltip_title, tooltip_description) = all
        .get("ToolTip")
        .and_then(|v| <(String, Vec<(i32, i32, Vec<u8>)>, String, String)>::try_from(v.clone()).ok())
        .map(|(_, _, t, d)| (t, d))
        .unwrap_or_default();
    let pixmaps = all
        .get("IconPixmap")
        .and_then(|v| Vec::<(i32, i32, Vec<u8>)>::try_from(v.clone()).ok())
        .unwrap_or_default();
    let has_pixmap = !pixmaps.is_empty();
    let pixmap = pixmap_file(pixmaps).unwrap_or_default();
    Ok(Item {
        key: key.to_owned(),
        id: text(&all, "Id"),
        title: text(&all, "Title"),
        status: text(&all, "Status"),
        category: text(&all, "Category"),
        icon_name: text(&all, "IconName"),
        icon_theme_path: text(&all, "IconThemePath"),
        attention_icon_name: text(&all, "AttentionIconName"),
        has_pixmap,
        pixmap,
        tooltip_title,
        tooltip_description,
        only_menu: all.get("ItemIsMenu").and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false),
        menu: all
            .get("Menu")
            .and_then(|v| OwnedObjectPath::try_from(v.clone()).ok())
            .map(|p| p.as_str().to_owned())
            .unwrap_or_default(),
    })
}

/// The largest of an item's pixmaps as a PNG, written if not there yet.
/// The pixels are ARGB32 in network order, as StatusNotifierItem sends them.
fn pixmap_file(pixmaps: Vec<(i32, i32, Vec<u8>)>) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let (w, h, argb) = pixmaps
        .into_iter()
        .filter(|(w, h, data)| *w > 0 && *h > 0 && data.len() == (*w as usize) * (*h as usize) * 4)
        .max_by_key(|(w, h, _)| w * h)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (w, h, &argb).hash(&mut hasher);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("genoma/tray");
    let path = dir.join(format!("{:016x}.png", hasher.finish()));
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        let rgba: Vec<u8> = argb.chunks_exact(4).flat_map(|p| [p[1], p[2], p[3], p[0]]).collect();
        let file = std::fs::File::create(&path).ok()?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if encoder.write_header().and_then(|mut writer| writer.write_image_data(&rgba)).is_err() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
    }
    Some(path.to_string_lossy().into_owned())
}

type Layout = (i32, HashMap<String, OwnedVariant>, Vec<OwnedVariant>);

fn entry((id, props, children): Layout) -> MenuEntry {
    let b = |k: &str, default: bool| props.get(k).and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(default);
    MenuEntry {
        id,
        label: text(&props, "label").replace('_', ""),
        enabled: b("enabled", true),
        visible: b("visible", true),
        separator: text(&props, "type") == "separator",
        toggle_type: text(&props, "toggle-type"),
        toggle_state: props.get("toggle-state").and_then(|v| i32::try_from(v.clone()).ok()).unwrap_or(-1),
        icon_name: text(&props, "icon-name"),
        children: children.into_iter().filter_map(|c| Layout::try_from(c).ok()).map(entry).collect(),
    }
}

async fn read_menu(conn: &zbus::Connection, item: &Item) -> zbus::Result<MenuEntry> {
    let (service, _) = split(&item.key);
    let menu = zbus::Proxy::new(conn, service, item.menu.clone(), MENU).await?;
    // Some applications fill a menu only when told it is about to show.
    let _ = menu.call_method("AboutToShow", &(0i32,)).await;
    let reply = menu.call_method("GetLayout", &(0i32, -1i32, Vec::<String>::new())).await?;
    let (_, layout): (u32, Layout) = reply.body().deserialize()?;
    Ok(entry(layout))
}

impl Tray {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Item(item) => {
                let mut items = self.items.get_untracked().unwrap_or_default();
                match items.iter_mut().find(|i| i.key == item.key) {
                    Some(i) => *i = *item,
                    None => items.push(*item),
                }
                self.items.set(items);
            }
            Update::Gone(key) => {
                let mut items = self.items.get_untracked().unwrap_or_default();
                items.retain(|i| i.key != key);
                self.items.set(items);
                let mut menus = self.menus.get_untracked().unwrap_or_default();
                if menus.remove(&key).is_some() {
                    self.menus.set(menus);
                }
            }
            Update::Menu(key, tree) => {
                let mut menus = self.menus.get_untracked().unwrap_or_default();
                menus.insert(key, tree);
                self.menus.set(menus);
            }
            Update::Watcher(serving) => self.serving_watcher.set(serving),
        });
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some(bus.clone());
        *self.post.borrow_mut() = Some(post.clone());
        let open = self.open_menus.clone();
        let thread = bus.clone();
        bus.spawn("tray", async move {
            let conn = bus::session().await?;
            let serving = serve_watcher(&thread, &conn).await.unwrap_or(false);
            let _ = post.send(Update::Watcher(serving));
            // The watcher's signals and every item's.
            let rule = MatchRule::builder().msg_type(Type::Signal).build()?;
            let mut stream = MessageStream::for_match_rule(rule, &conn, None).await?;
            let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
            conn.request_name(host.as_str()).await?;
            // Another's watcher, held by a shell that may be on its way out
            // (the switch from Bioma on Quickshell): once it goes, Genoma
            // serves the watcher, and the items register with it again.
            if !serving {
                let (conn, post, host, waiter) = (conn.clone(), post.clone(), host.clone(), thread.clone());
                thread.spawn("tray watcher takeover", async move {
                    bus::until_free(&conn, WATCHER).await?;
                    if serve_watcher(&waiter, &conn).await? {
                        let _ = post.send(Update::Watcher(true));
                        let watcher = zbus::Proxy::new(&conn, WATCHER, WATCHER_PATH, WATCHER).await?;
                        watcher.call_method("RegisterStatusNotifierHost", &(host.as_str(),)).await?;
                    }
                    Ok(())
                });
            }
            let watcher = zbus::Proxy::new(&conn, WATCHER, WATCHER_PATH, WATCHER).await?;
            // A watcher on its way out may not answer: the items come
            // through the takeover above.
            if let Err(e) = watcher.call_method("RegisterStatusNotifierHost", &(host.as_str(),)).await {
                log::warn!("tray: the watcher did not take the host: {e}");
            }
            let listed: Vec<String> = watcher.get_property("RegisteredStatusNotifierItems").await.unwrap_or_default();
            // An item's signals come from its unique name.
            let mut owners: HashMap<String, Vec<String>> = HashMap::new();
            let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
            let mut items: HashMap<String, Item> = HashMap::new();
            let add = async |key: String, owners: &mut HashMap<String, Vec<String>>, items: &mut HashMap<String, Item>| {
                let (service, _) = split(&key);
                let owner = match BusName::try_from(service.as_str()) {
                    Ok(name) => dbus.get_name_owner(name).await.map(|o| o.to_string()).unwrap_or(service.clone()),
                    Err(_) => service.clone(),
                };
                owners.entry(owner).or_default().push(key.clone());
                if let Ok(item) = read_item(&conn, &key).await {
                    items.insert(key.clone(), item.clone());
                    let _ = post.send(Update::Item(Box::new(item)));
                }
            };
            for key in listed {
                add(key, &mut owners, &mut items).await;
            }
            while let Some(message) = stream.next().await {
                let Ok(message) = message else { continue };
                let header = message.header();
                let member = header.member().map(|m| m.as_str().to_owned()).unwrap_or_default();
                let interface = header.interface().map(|i| i.as_str().to_owned()).unwrap_or_default();
                let sender = header.sender().map(|s| s.as_str().to_owned()).unwrap_or_default();
                match (interface.as_str(), member.as_str()) {
                    (WATCHER, "StatusNotifierItemRegistered") => {
                        if let Ok(key) = message.body().deserialize::<String>() {
                            if !items.contains_key(&key) {
                                add(key, &mut owners, &mut items).await;
                            }
                        }
                    }
                    (WATCHER, "StatusNotifierItemUnregistered") => {
                        if let Ok(key) = message.body().deserialize::<String>() {
                            items.remove(&key);
                            let _ = post.send(Update::Gone(key));
                        }
                    }
                    ("org.freedesktop.DBus", "NameOwnerChanged") => {
                        let Ok((_, old, new)) = message.body().deserialize::<(String, String, String)>() else { continue };
                        if new.is_empty() {
                            for key in owners.remove(&old).unwrap_or_default() {
                                items.remove(&key);
                                let _ = post.send(Update::Gone(key));
                            }
                        }
                    }
                    (ITEM, _) => {
                        for key in owners.get(&sender).cloned().unwrap_or_default() {
                            if let Ok(item) = read_item(&conn, &key).await {
                                items.insert(key.clone(), item.clone());
                                let _ = post.send(Update::Item(Box::new(item)));
                            }
                        }
                    }
                    (MENU, "LayoutUpdated" | "ItemsPropertiesUpdated") => {
                        let keys: Vec<String> = owners.get(&sender).cloned().unwrap_or_default();
                        for key in keys {
                            let is_open = open.lock().unwrap().contains(&key);
                            if let (true, Some(item)) = (is_open, items.get(&key)) {
                                if item.menu.as_str() == header.path().map(|p| p.as_str()).unwrap_or_default() {
                                    if let Ok(tree) = read_menu(&conn, item).await {
                                        let _ = post.send(Update::Menu(key, tree));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        });
    }

    fn item(&self, key: &str) -> Option<Item> {
        self.items.get_untracked().unwrap_or_default().into_iter().find(|i| i.key == key)
    }

    fn on_bus(&self, name: &'static str, work: impl std::future::Future<Output = zbus::Result<()>> + Send + 'static) {
        match self.bus.borrow().as_ref() {
            Some(bus) => bus.spawn(name, work),
            None => log::warn!("{name}: the tray has not started"),
        }
    }

    /// The item's own action, at a place on screen (`Activate`,
    /// `SecondaryActivate` or `ContextMenu`).
    pub fn act(&self, key: &str, method: &'static str, x: i32, y: i32) {
        let (service, path) = split(key);
        self.on_bus("tray action", async move {
            let conn = bus::session().await?;
            zbus::Proxy::new(&conn, service, path, ITEM).await?.call_method(method, &(x, y)).await.map(|_| ())
        });
    }

    /// `orientation` is "vertical" or "horizontal".
    pub fn scroll(&self, key: &str, delta: i32, orientation: String) {
        let (service, path) = split(key);
        self.on_bus("tray scroll", async move {
            let conn = bus::session().await?;
            zbus::Proxy::new(&conn, service, path, ITEM).await?.call_method("Scroll", &(delta, orientation)).await.map(|_| ())
        });
    }

    /// Reads the item's menu into `menus`, and keeps it current while open.
    pub fn open_menu(&self, key: &str) {
        let (Some(item), Some(post)) = (self.item(key), self.post.borrow().clone()) else { return };
        if item.menu.is_empty() {
            return;
        }
        self.open_menus.lock().unwrap().insert(key.to_owned());
        let key = key.to_owned();
        self.on_bus("tray menu", async move {
            let conn = bus::session().await?;
            let tree = read_menu(&conn, &item).await?;
            let _ = post.send(Update::Menu(key, tree));
            Ok(())
        });
    }

    pub fn close_menu(&self, key: &str) {
        self.open_menus.lock().unwrap().remove(key);
    }

    /// A click on the menu entry `id`.
    pub fn menu_click(&self, key: &str, id: i32) {
        let Some(item) = self.item(key) else { return };
        let (service, _) = split(key);
        self.on_bus("tray menu click", async move {
            let conn = bus::session().await?;
            let menu = zbus::Proxy::new(&conn, service, item.menu, MENU).await?;
            menu.call_method("Event", &(id, "clicked", zbus::Variant::from(0i32), 0u32)).await.map(|_| ())
        });
    }
}
