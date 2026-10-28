//! Bluetooth (PRD §9.1): BlueZ's adapter and devices over D-Bus, as Bioma's
//! `services/Bluetooth.qml` reads them from `Quickshell.Bluetooth`, with the
//! actions its cell takes.
//!
//! Over zbus rather than `bluer`: `bluer` brings a tokio runtime for a handful
//! of D-Bus objects, and the bus thread is already there. One
//! `GetManagedObjects` gives the adapters, the devices and their batteries
//! together; it is called again after any change BlueZ signals, once the
//! burst has settled. BlueZ has no "connecting" state of its own, so a device
//! is busy for as long as a call on it is in flight, as Quickshell does.

use std::{cell::RefCell, collections::HashMap, time::Duration};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{MatchRule, MessageStream, OwnedVariant, message::Type, wire::OwnedObjectPath};

use crate::bus::{self, Bus};

const BLUEZ: &str = "org.bluez";
const SETTLE: Duration = Duration::from_millis(60);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Adapter {
    pub path: String,
    pub name: String,
    pub address: String,
    pub powered: bool,
    pub discovering: bool,
    pub discoverable: bool,
    pub pairable: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub address: String,
    /// The name a person gave it, or its own (`Alias`).
    pub name: String,
    /// The name it gives itself (`Name`), often empty.
    pub device_name: String,
    /// freedesktop's icon name, `audio-headset`, `input-mouse`…
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub blocked: bool,
    /// 0 to 1, when the device reports one.
    pub battery: Option<f64>,
    /// A call on it is in flight: connecting, disconnecting or pairing.
    pub busy: &'static str,
}

pub enum Update {
    State { adapter: Option<Adapter>, devices: Vec<(String, Device)> },
    /// A call on the device at `address` began ("connecting", "pairing"…)
    /// or ended (empty), and how it ended.
    Busy { address: String, busy: &'static str, failure: Option<String> },
}

pub struct Bluetooth {
    pub adapter: Signal<Option<Adapter>>,
    /// By address.
    pub devices: Signal<Vec<Device>>,
    /// The last failed call: the device's address and BlueZ's error.
    pub failure: Signal<Option<(String, String)>>,
    paths: RefCell<HashMap<String, String>>,
    busy: RefCell<HashMap<String, &'static str>>,
    bus: RefCell<Option<Bus>>,
    post: RefCell<Option<Sender<Update>>>,
}

impl Default for Bluetooth {
    fn default() -> Self {
        Bluetooth {
            adapter: Signal::new(None),
            devices: Signal::new(Vec::new()),
            failure: Signal::new(None),
            paths: RefCell::default(),
            busy: RefCell::default(),
            bus: RefCell::new(None),
            post: RefCell::new(None),
        }
    }
}

type Interfaces = HashMap<String, HashMap<String, OwnedVariant>>;

fn text(props: &HashMap<String, OwnedVariant>, key: &str) -> String {
    props.get(key).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default()
}

fn flag(props: &HashMap<String, OwnedVariant>, key: &str) -> bool {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false)
}

async fn read(conn: &zbus::Connection) -> zbus::Result<Update> {
    let manager = zbus::fdo::ObjectManagerProxy::builder(conn).destination(BLUEZ).path("/").build().await?;
    let objects: HashMap<OwnedObjectPath, Interfaces> = manager
        .get_managed_objects()
        .await?
        .into_iter()
        .map(|(path, ifaces)| {
            let ifaces = ifaces
                .into_iter()
                .map(|(name, props)| (name.to_string(), props.into_iter().map(|(k, v)| (k.to_string(), v)).collect()))
                .collect();
            (path, ifaces)
        })
        .collect();
    let mut adapter = None;
    let mut devices = Vec::new();
    let mut paths: Vec<&OwnedObjectPath> = objects.keys().collect();
    paths.sort_by_key(|p| p.as_str().to_owned());
    for path in paths {
        let ifaces = &objects[path];
        if let Some(a) = ifaces.get("org.bluez.Adapter1") {
            // The first adapter is the default one, as Quickshell's.
            if adapter.is_none() {
                adapter = Some(Adapter {
                    path: path.as_str().to_owned(),
                    name: text(a, "Alias"),
                    address: text(a, "Address"),
                    powered: flag(a, "Powered"),
                    discovering: flag(a, "Discovering"),
                    discoverable: flag(a, "Discoverable"),
                    pairable: flag(a, "Pairable"),
                });
            }
        }
        if let Some(d) = ifaces.get("org.bluez.Device1") {
            let battery = ifaces
                .get("org.bluez.Battery1")
                .and_then(|b| b.get("Percentage"))
                .and_then(|v| u8::try_from(v.clone()).ok())
                .map(|p| p as f64 / 100.0);
            devices.push((
                path.as_str().to_owned(),
                Device {
                    address: text(d, "Address"),
                    name: text(d, "Alias"),
                    device_name: text(d, "Name"),
                    icon: text(d, "Icon"),
                    paired: flag(d, "Paired"),
                    trusted: flag(d, "Trusted"),
                    connected: flag(d, "Connected"),
                    blocked: flag(d, "Blocked"),
                    battery,
                    busy: "",
                },
            ));
        }
    }
    Ok(Update::State { adapter, devices })
}

impl Bluetooth {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::State { adapter, devices } => {
                let busy = self.busy.borrow();
                let mut paths = self.paths.borrow_mut();
                paths.clear();
                let mut list = Vec::with_capacity(devices.len());
                for (path, mut d) in devices {
                    d.busy = busy.get(&d.address).copied().unwrap_or("");
                    paths.insert(d.address.clone(), path);
                    list.push(d);
                }
                self.adapter.set(adapter);
                self.devices.set(list);
            }
            Update::Busy { address, busy, failure } => {
                if busy.is_empty() {
                    self.busy.borrow_mut().remove(&address);
                } else {
                    self.busy.borrow_mut().insert(address.clone(), busy);
                }
                let mut list = self.devices.get_untracked().unwrap_or_default();
                if let Some(d) = list.iter_mut().find(|d| d.address == address) {
                    d.busy = busy;
                }
                self.devices.set(list);
                if let Some(e) = failure {
                    self.failure.set(Some((address, e)));
                }
            }
        });
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some(bus.clone());
        *self.post.borrow_mut() = Some(post.clone());
        let pause = bus.clone();
        bus.spawn("bluetooth", async move {
            let conn = bus::system().await?;
            let rule = MatchRule::builder().msg_type(Type::Signal).sender(BLUEZ).build()?;
            let mut stream = MessageStream::for_match_rule(rule, &conn, None).await?;
            let _ = post.send(read(&conn).await?);
            while stream.next().await.is_some() {
                pause.sleep(SETTLE).await;
                while let Some(Some(_)) = futures_util::FutureExt::now_or_never(stream.next()) {}
                match read(&conn).await {
                    Ok(u) => {
                        let _ = post.send(u);
                    }
                    Err(e) => log::warn!("bluetooth: {e}"),
                }
            }
            Ok(())
        });
    }

    fn adapter_path(&self) -> Option<String> {
        self.adapter.get_untracked().flatten().map(|a| a.path)
    }

    /// A call on the adapter, its failure logged.
    fn on_adapter(&self, name: &'static str, call: impl FnOnce(String) -> AdapterCall) {
        let (Some(bus), Some(path)) = (self.bus.borrow().clone(), self.adapter_path()) else {
            log::warn!("{name}: no adapter");
            return;
        };
        let call = call(path.clone());
        bus.spawn(name, async move {
            let conn = bus::system().await?;
            let props = zbus::fdo::PropertiesProxy::builder(&conn).destination(BLUEZ).path(path.clone()).build().await?;
            let adapter = zbus::Proxy::new(&conn, BLUEZ, path, "org.bluez.Adapter1").await?;
            match call {
                AdapterCall::Powered(on) => {
                    props.set("org.bluez.Adapter1".try_into()?, "Powered", &zbus::Variant::from(on)).await?
                }
                AdapterCall::Discovery(true) => adapter.call_method("StartDiscovery", &()).await.map(|_| ())?,
                AdapterCall::Discovery(false) => adapter.call_method("StopDiscovery", &()).await.map(|_| ())?,
                AdapterCall::Remove(device) => adapter.call_method("RemoveDevice", &(device,)).await.map(|_| ())?,
            }
            Ok(())
        });
    }

    pub fn set_powered(&self, on: bool) {
        self.on_adapter("bluetooth power", |_| AdapterCall::Powered(on));
    }

    /// Searching for devices: while a list is open, and only then.
    pub fn set_discovering(&self, on: bool) {
        self.on_adapter("bluetooth discovery", |_| AdapterCall::Discovery(on));
    }

    /// Forgets the device: unpaired and removed from the adapter.
    pub fn forget(&self, address: &str) {
        let Some(path) = self.paths.borrow().get(address).cloned() else { return };
        let Ok(path) = OwnedObjectPath::try_from(path) else { return };
        self.on_adapter("bluetooth forget", |_| AdapterCall::Remove(path));
    }

    /// A call on the device at `address`, the device busy with `busy` until
    /// it answers.
    pub fn on_device(&self, address: &str, call: DeviceCall) {
        let (Some(bus), Some(post)) = (self.bus.borrow().clone(), self.post.borrow().clone()) else { return };
        let Some(path) = self.paths.borrow().get(address).cloned() else { return };
        let busy = match call {
            DeviceCall::Connect => "connecting",
            DeviceCall::Disconnect => "disconnecting",
            DeviceCall::Pair => "pairing",
            DeviceCall::CancelPair | DeviceCall::Trusted(_) => "",
        };
        if !busy.is_empty() {
            self.failure.set(None);
            self.apply(Update::Busy { address: address.to_owned(), busy, failure: None });
        }
        let address = address.to_owned();
        bus.spawn("bluetooth device", async move {
            let result: zbus::Result<()> = async {
                let conn = bus::system().await?;
                let device = zbus::Proxy::new(&conn, BLUEZ, path.clone(), "org.bluez.Device1").await?;
                match call {
                    DeviceCall::Connect => device.call_method("Connect", &()).await.map(|_| ()),
                    DeviceCall::Disconnect => device.call_method("Disconnect", &()).await.map(|_| ()),
                    DeviceCall::Pair => device.call_method("Pair", &()).await.map(|_| ()),
                    DeviceCall::CancelPair => device.call_method("CancelPairing", &()).await.map(|_| ()),
                    DeviceCall::Trusted(on) => {
                        let props = zbus::fdo::PropertiesProxy::builder(&conn).destination(BLUEZ).path(path).build().await?;
                        props.set("org.bluez.Device1".try_into()?, "Trusted", &zbus::Variant::from(on)).await.map_err(Into::into)
                    }
                }
            }
            .await;
            if !busy.is_empty() {
                let failure = result.as_ref().err().map(|e| e.to_string());
                let _ = post.send(Update::Busy { address, busy: "", failure });
            } else if let Err(e) = result {
                log::warn!("bluetooth: {e}");
            }
            Ok(())
        });
    }
}

enum AdapterCall {
    Powered(bool),
    Discovery(bool),
    Remove(OwnedObjectPath),
}

pub enum DeviceCall {
    Connect,
    Disconnect,
    Pair,
    CancelPair,
    Trusted(bool),
}
