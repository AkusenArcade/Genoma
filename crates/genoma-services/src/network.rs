//! Network (PRD §9.1): NetworkManager's connectivity, its wired and Wi-Fi
//! devices and the networks a Wi-Fi device sees, with the actions Bioma's
//! connectivity cell takes (join, leave, forget, the radio, a scan).
//!
//! The bus thread keeps one subscription to everything NetworkManager says
//! about its objects. On a change it waits a moment for the rest of the burst
//! (a device coming up moves a dozen properties), reads the state again and
//! posts it whole; the main thread sets each signal, and one that did not
//! change wakes nobody. Nothing polls. The scanner, when it is asked for, asks
//! NetworkManager for a scan every ten seconds, as Bioma's does while its list
//! is open.

use std::{cell::RefCell, collections::HashMap, time::Duration};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{
    MatchRule, MessageStream, OwnedVariant, Variant,
    message::Type,
    proxy::CacheProperties,
    wire::OwnedObjectPath,
};

use crate::bus::{self, Bus};

const NM: &str = "org.freedesktop.NetworkManager";
/// What a burst of changes is given to finish before the state is read.
const SETTLE: Duration = Duration::from_millis(60);
const SCAN_EVERY: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub interface: String,
    /// "ethernet", "wifi" or "other".
    pub kind: &'static str,
    /// NetworkManager's device state, as a word: "activated", "disconnected"…
    pub state: &'static str,
    pub connected: bool,
    /// A cable with a link (ethernet only): plugged in, whether or not a
    /// connection came up on it.
    pub carrier: bool,
    /// Mb/s, ethernet only; 0 when unknown.
    pub speed: u32,
    /// The profile it is connected with ("Wired connection 1"), or empty.
    pub connection: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessPoint {
    pub ssid: String,
    /// 0 to 1.
    pub strength: f64,
    pub secured: bool,
    pub connected: bool,
    /// MHz.
    pub frequency: u32,
    /// Whether a saved profile knows it: joining needs no password.
    pub known: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// "unknown", "none", "portal", "limited", "full".
    pub connectivity: &'static str,
    pub can_check: bool,
    pub wifi_enabled: bool,
    pub wifi_hardware: bool,
    pub devices: Vec<Device>,
    /// What the Wi-Fi device sees, one entry per network (the strongest of
    /// its access points), the connected one first, then by strength.
    pub networks: Vec<AccessPoint>,
}

pub enum Update {
    State(State),
    /// A connection attempt failed: NetworkManager's reason, as a word.
    Failed(String),
}

pub struct Network {
    pub available: Signal<bool>,
    pub connectivity: Signal<&'static str>,
    pub can_check: Signal<bool>,
    pub wifi_enabled: Signal<bool>,
    pub wifi_hardware: Signal<bool>,
    pub devices: Signal<Vec<Device>>,
    pub networks: Signal<Vec<AccessPoint>>,
    /// The last failure, until the next attempt.
    pub failure: Signal<String>,
    bus: RefCell<Option<Bus>>,
    scanner: RefCell<Option<zruntime::Task<(), zruntime::Shared>>>,
}

impl Default for Network {
    fn default() -> Self {
        Network {
            available: Signal::new(false),
            connectivity: Signal::new("unknown"),
            can_check: Signal::new(false),
            wifi_enabled: Signal::new(false),
            wifi_hardware: Signal::new(false),
            devices: Signal::new(Vec::new()),
            networks: Signal::new(Vec::new()),
            failure: Signal::new(String::new()),
            bus: RefCell::new(None),
            scanner: RefCell::new(None),
        }
    }
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager", default_service = "org.freedesktop.NetworkManager", default_path = "/org/freedesktop/NetworkManager")]
trait Manager {
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn connectivity_check_available(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn connectivity_check_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_wireless_enabled(&self, on: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> zbus::Result<bool>;
    fn check_connectivity(&self) -> zbus::Result<u32>;
    fn activate_connection(
        &self,
        connection: &OwnedObjectPath,
        device: &OwnedObjectPath,
        specific: &OwnedObjectPath,
    ) -> zbus::Result<OwnedObjectPath>;
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Variant<'_>>>,
        device: &OwnedObjectPath,
        specific: &OwnedObjectPath,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Device", default_service = "org.freedesktop.NetworkManager")]
trait NmDevice {
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn active_connection(&self) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn available_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn disconnect(&self) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Device.Wired", default_service = "org.freedesktop.NetworkManager")]
trait Wired {
    #[zbus(property)]
    fn carrier(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn speed(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Device.Wireless", default_service = "org.freedesktop.NetworkManager")]
trait Wireless {
    #[zbus(property)]
    fn access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
    fn request_scan(&self, options: HashMap<&str, Variant<'_>>) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.AccessPoint", default_service = "org.freedesktop.NetworkManager")]
trait Ap {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn frequency(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Connection.Active", default_service = "org.freedesktop.NetworkManager")]
trait Active {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Settings.Connection", default_service = "org.freedesktop.NetworkManager")]
trait Saved {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedVariant>>>;
    fn delete(&self) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Settings", default_service = "org.freedesktop.NetworkManager", default_path = "/org/freedesktop/NetworkManager/Settings")]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

fn none() -> OwnedObjectPath {
    OwnedObjectPath::try_from("/").expect("the root path")
}

fn connectivity_name(c: u32) -> &'static str {
    match c {
        1 => "none",
        2 => "portal",
        3 => "limited",
        4 => "full",
        _ => "unknown",
    }
}

fn device_state_name(s: u32) -> &'static str {
    match s {
        10 => "unmanaged",
        20 => "unavailable",
        30 => "disconnected",
        40 => "prepare",
        50 => "config",
        60 => "need-auth",
        70 => "ip-config",
        80 => "ip-check",
        90 => "secondaries",
        100 => "activated",
        110 => "deactivating",
        120 => "failed",
        _ => "unknown",
    }
}

/// NetworkManager's reason for a failed device, the ones a person can act on
/// by name, the rest by number.
fn reason_name(r: u32) -> String {
    match r {
        7 => "no-secrets".into(),
        8 => "supplicant-disconnect".into(),
        9 => "supplicant-config-failed".into(),
        10 => "supplicant-failed".into(),
        11 => "supplicant-timeout".into(),
        53 => "ssid-not-found".into(),
        other => format!("reason-{other}"),
    }
}

async fn proxy<'a, P: From<zbus::Proxy<'a>> + zbus::proxy::Defaults>(
    conn: &zbus::Connection,
    path: &OwnedObjectPath,
) -> zbus::Result<P> {
    zbus::proxy::Builder::<P>::new(conn).path(path.clone()).cache_properties(CacheProperties::No).build().await
}

/// The SSIDs saved profiles know, by profile path.
async fn saved_ssids(conn: &zbus::Connection) -> zbus::Result<Vec<(OwnedObjectPath, String)>> {
    let settings = SettingsProxy::builder(conn).cache_properties(CacheProperties::No).build().await?;
    let mut out = Vec::new();
    for path in settings.list_connections().await? {
        let saved: SavedProxy<'_> = proxy(conn, &path).await?;
        let Ok(s) = saved.get_settings().await else { continue };
        let ssid = s
            .get("802-11-wireless")
            .and_then(|w| w.get("ssid"))
            .and_then(|v| Vec::<u8>::try_from(v.clone()).ok());
        if let Some(ssid) = ssid {
            out.push((path, String::from_utf8_lossy(&ssid).into_owned()));
        }
    }
    Ok(out)
}

async fn read(conn: &zbus::Connection) -> zbus::Result<State> {
    let nm = ManagerProxy::builder(conn).cache_properties(CacheProperties::No).build().await?;
    let mut state = State {
        connectivity: connectivity_name(nm.connectivity().await?),
        can_check: nm.connectivity_check_available().await.unwrap_or(false)
            && nm.connectivity_check_enabled().await.unwrap_or(false),
        wifi_enabled: nm.wireless_enabled().await?,
        wifi_hardware: nm.wireless_hardware_enabled().await?,
        ..State::default()
    };
    let known: Vec<String> = saved_ssids(conn).await.unwrap_or_default().into_iter().map(|(_, s)| s).collect();
    for path in nm.devices().await? {
        let dev: NmDeviceProxy<'_> = proxy(conn, &path).await?;
        let kind = match dev.device_type().await? {
            1 => "ethernet",
            2 => "wifi",
            _ => "other",
        };
        if kind == "other" {
            continue;
        }
        let raw_state = dev.state().await?;
        let active = dev.active_connection().await?;
        let connection = if active.as_str() == "/" {
            String::new()
        } else {
            let a: ActiveProxy<'_> = proxy(conn, &active).await?;
            a.id().await.unwrap_or_default()
        };
        let mut device = Device {
            interface: dev.interface().await?,
            kind,
            state: device_state_name(raw_state),
            connected: raw_state == 100,
            connection,
            ..Device::default()
        };
        if kind == "ethernet" {
            let wired: WiredProxy<'_> = proxy(conn, &path).await?;
            device.carrier = wired.carrier().await.unwrap_or(false);
            device.speed = wired.speed().await.unwrap_or(0);
        } else {
            let wireless: WirelessProxy<'_> = proxy(conn, &path).await?;
            let current = wireless.active_access_point().await.unwrap_or_else(|_| none());
            let mut by_ssid: HashMap<String, AccessPoint> = HashMap::new();
            for ap_path in wireless.access_points().await.unwrap_or_default() {
                let ap: ApProxy<'_> = proxy(conn, &ap_path).await?;
                let Ok(ssid) = ap.ssid().await else { continue };
                let ssid = String::from_utf8_lossy(&ssid).into_owned();
                // A hidden network has no name to list it by.
                if ssid.is_empty() {
                    continue;
                }
                let entry = AccessPoint {
                    strength: ap.strength().await.unwrap_or(0) as f64 / 100.0,
                    secured: ap.flags().await.unwrap_or(0) & 1 != 0
                        || ap.wpa_flags().await.unwrap_or(0) != 0
                        || ap.rsn_flags().await.unwrap_or(0) != 0,
                    connected: ap_path == current,
                    frequency: ap.frequency().await.unwrap_or(0),
                    known: known.contains(&ssid),
                    ssid: ssid.clone(),
                };
                let keep = by_ssid.get(&ssid).is_none_or(|old| {
                    entry.connected || (!old.connected && entry.strength > old.strength)
                });
                if keep {
                    by_ssid.insert(ssid, entry);
                }
            }
            let mut networks: Vec<AccessPoint> = by_ssid.into_values().collect();
            networks.sort_by(|a, b| {
                b.connected.cmp(&a.connected).then(b.strength.total_cmp(&a.strength)).then(a.ssid.cmp(&b.ssid))
            });
            if state.networks.is_empty() {
                state.networks = networks;
            }
        }
        state.devices.push(device);
    }
    // Connected first: a laptop on a dock has its own port empty and the
    // dock's carrying the connection (Bioma's Network.qml).
    state.devices.sort_by(|a, b| b.connected.cmp(&a.connected));
    Ok(state)
}

/// The first Wi-Fi device's path.
async fn wifi_device(conn: &zbus::Connection) -> zbus::Result<Option<OwnedObjectPath>> {
    let nm = ManagerProxy::builder(conn).cache_properties(CacheProperties::No).build().await?;
    for path in nm.devices().await? {
        let dev: NmDeviceProxy<'_> = proxy(conn, &path).await?;
        if dev.device_type().await? == 2 {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

impl Network {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::State(s) => {
                self.available.set(true);
                self.connectivity.set(s.connectivity);
                self.can_check.set(s.can_check);
                self.wifi_enabled.set(s.wifi_enabled);
                self.wifi_hardware.set(s.wifi_hardware);
                self.devices.set(s.devices);
                self.networks.set(s.networks);
            }
            Update::Failed(reason) => self.failure.set(reason),
        });
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some(bus.clone());
        let pause = bus.clone();
        bus.spawn("network", async move {
            let conn = bus::system().await?;
            // Everything NetworkManager says about its objects: property
            // changes, devices and access points added and removed, and a
            // device's state change with its reason.
            let rule = MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(NM)
                .path_namespace("/org/freedesktop/NetworkManager")
                .build()?;
            let mut stream = MessageStream::for_match_rule(rule, &conn, None).await?;
            let _ = post.send(Update::State(read(&conn).await?));
            while let Some(message) = stream.next().await {
                let Ok(message) = message else { continue };
                let header = message.header();
                if header.member().is_some_and(|m| m.as_str() == "StateChanged")
                    && header.interface().is_some_and(|i| i.as_str() == "org.freedesktop.NetworkManager.Device")
                {
                    if let Ok((new, _, reason)) = message.body().deserialize::<(u32, u32, u32)>() {
                        if new == 120 {
                            let _ = post.send(Update::Failed(reason_name(reason)));
                        } else if new == 40 {
                            let _ = post.send(Update::Failed(String::new()));
                        }
                    }
                }
                // The rest of the burst, then one read.
                pause.sleep(SETTLE).await;
                while let Some(Ok(Some(_))) = futures_util::FutureExt::now_or_never(stream.next()).map(|m| m.transpose()) {}
                match read(&conn).await {
                    Ok(state) => {
                        let _ = post.send(Update::State(state));
                    }
                    Err(e) => log::warn!("network: {e}"),
                }
            }
            Ok(())
        });
    }

    fn act(&self, name: &'static str, work: impl std::future::Future<Output = zbus::Result<()>> + Send + 'static) {
        match self.bus.borrow().as_ref() {
            Some(bus) => bus.spawn(name, work),
            None => log::warn!("{name}: the network service has not started"),
        }
    }

    pub fn set_wifi_enabled(&self, on: bool) {
        self.act("network wifi", async move {
            let conn = bus::system().await?;
            ManagerProxy::builder(&conn).build().await?.set_wireless_enabled(on).await
        });
    }

    pub fn check_connectivity(&self) {
        self.act("network check", async move {
            let conn = bus::system().await?;
            ManagerProxy::builder(&conn).build().await?.check_connectivity().await.map(|_| ())
        });
    }

    /// While on, a scan every ten seconds: what a list of networks open on
    /// screen wants, and nothing else does.
    pub fn set_scanning(&self, on: bool) {
        if !on {
            self.scanner.borrow_mut().take();
            return;
        }
        if self.scanner.borrow().is_some() {
            return;
        }
        let Some(bus) = self.bus.borrow().clone() else { return };
        let pause = bus.clone();
        let task = bus.task("network scan", async move {
            let scan = async {
                let conn = bus::system().await?;
                let Some(path) = wifi_device(&conn).await? else { return Ok::<_, zbus::Error>(()) };
                let wireless: WirelessProxy<'_> = proxy(&conn, &path).await?;
                loop {
                    // NetworkManager refuses a scan too soon after the last;
                    // the next one comes anyway.
                    let _ = wireless.request_scan(HashMap::new()).await;
                    pause.sleep(SCAN_EVERY).await;
                }
            };
            if let Err(e) = scan.await {
                log::warn!("network scan: {e}");
            }
        });
        *self.scanner.borrow_mut() = Some(task);
    }

    /// Joins `ssid`: through its saved profile when there is one and no
    /// password is given, otherwise with a new profile (and `password`).
    pub fn join(&self, ssid: String, password: Option<String>) {
        self.failure.set(String::new());
        self.act("network join", async move {
            let conn = bus::system().await?;
            let nm = ManagerProxy::builder(&conn).build().await?;
            let Some(device) = wifi_device(&conn).await? else { return Ok(()) };
            if password.is_none() {
                if let Some((saved, _)) = saved_ssids(&conn).await?.into_iter().find(|(_, s)| *s == ssid) {
                    nm.activate_connection(&saved, &device, &none()).await?;
                    return Ok(());
                }
            }
            let mut wireless: HashMap<&str, Variant<'_>> = HashMap::new();
            wireless.insert("ssid", Variant::from(ssid.as_bytes().to_vec()));
            let mut settings: HashMap<&str, HashMap<&str, Variant<'_>>> = HashMap::new();
            settings.insert("802-11-wireless", wireless);
            if let Some(psk) = &password {
                let mut security: HashMap<&str, Variant<'_>> = HashMap::new();
                security.insert("key-mgmt", Variant::from("wpa-psk"));
                security.insert("psk", Variant::from(psk.as_str()));
                settings.insert("802-11-wireless-security", security);
            }
            nm.add_and_activate_connection(settings, &device, &none()).await?;
            Ok(())
        });
    }

    /// Leaves the Wi-Fi network, keeping its profile.
    pub fn leave(&self) {
        self.act("network leave", async move {
            let conn = bus::system().await?;
            let Some(device) = wifi_device(&conn).await? else { return Ok(()) };
            let dev: NmDeviceProxy<'_> = proxy(&conn, &device).await?;
            dev.disconnect().await
        });
    }

    /// Removes every saved profile for `ssid`.
    pub fn forget(&self, ssid: String) {
        self.act("network forget", async move {
            let conn = bus::system().await?;
            for (path, s) in saved_ssids(&conn).await? {
                if s == ssid {
                    let saved: SavedProxy<'_> = proxy(&conn, &path).await?;
                    saved.delete().await?;
                }
            }
            Ok(())
        });
    }
}
