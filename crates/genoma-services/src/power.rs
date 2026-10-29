//! Battery and power (PRD §9.1): UPower's display device, the sum of the
//! batteries as the desktop shows it, and power-profiles-daemon's profile.
//! Both are read when the service starts and again on each
//! `PropertiesChanged`; nothing polls.

use std::{cell::RefCell, collections::HashMap};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{OwnedVariant, proxy::CacheProperties};

use crate::bus::{self, Bus};

/// UPower's display device as one value, so a change that moves several
/// fields at once wakes its readers once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub present: bool,
    /// A battery that powers the machine (UPower's `PowerSupply` on a
    /// battery), as Quickshell's `isLaptopBattery`: a mouse's does not count.
    pub laptop: bool,
    /// 0 to 1.
    pub level: f64,
    /// "charging", "discharging", "empty", "full", "pending-charge",
    /// "pending-discharge" or "unknown".
    pub state: &'static str,
    /// Seconds; 0 when UPower does not know.
    pub time_to_empty: i64,
    pub time_to_full: i64,
    /// Watts going in or out (UPower's `EnergyRate`), a magnitude; 0 while
    /// it is still measuring, or on a machine that does not report it.
    pub energy_rate: f64,
}

pub struct Power {
    pub battery: Signal<Battery>,
    pub on_battery: Signal<bool>,
    /// "performance", "balanced", "power-saver"; empty without the daemon.
    pub profile: Signal<String>,
    pub profiles: Signal<Vec<String>>,
    /// Why performance is held back, as the daemon says it
    /// ("lap-detected", "high-operator-temperature"); empty when it is not.
    pub degraded: Signal<String>,
    bus: RefCell<Option<Bus>>,
}

/// What the bus thread posts.
pub enum Update {
    Battery(Battery),
    OnBattery(bool),
    Profiles { active: String, offered: Vec<String>, degraded: String },
}

impl Default for Power {
    fn default() -> Self {
        Power {
            battery: Signal::new(Battery::default()),
            on_battery: Signal::new(false),
            profile: Signal::new(String::new()),
            profiles: Signal::new(Vec::new()),
            degraded: Signal::new(String::new()),
            bus: RefCell::new(None),
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait Device {
    #[zbus(property, name = "Type")]
    fn kind(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn power_supply(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn time_to_full(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn energy_rate(&self) -> zbus::Result<f64>;
}

#[zbus::proxy(interface = "org.freedesktop.UPower", default_service = "org.freedesktop.UPower", default_path = "/org/freedesktop/UPower")]
trait UPower {
    #[zbus(property)]
    fn on_battery(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.PowerProfiles",
    default_service = "org.freedesktop.UPower.PowerProfiles",
    default_path = "/org/freedesktop/UPower/PowerProfiles"
)]
trait Profiles {
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_active_profile(&self, profile: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn profiles(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;
    #[zbus(property)]
    fn performance_degraded(&self) -> zbus::Result<String>;
}

fn state_name(state: u32) -> &'static str {
    match state {
        1 => "charging",
        2 => "discharging",
        3 => "empty",
        4 => "full",
        5 => "pending-charge",
        6 => "pending-discharge",
        _ => "unknown",
    }
}

async fn battery(device: &DeviceProxy<'_>) -> zbus::Result<Battery> {
    Ok(Battery {
        present: device.is_present().await?,
        // 2 is a battery.
        laptop: device.kind().await? == 2 && device.power_supply().await?,
        level: (device.percentage().await? / 100.0).clamp(0.0, 1.0),
        state: state_name(device.state().await?),
        time_to_empty: device.time_to_empty().await?,
        time_to_full: device.time_to_full().await?,
        energy_rate: device.energy_rate().await.unwrap_or(0.0).abs(),
    })
}

async fn profiles(proxy: &ProfilesProxy<'_>) -> zbus::Result<Update> {
    let offered = proxy
        .profiles()
        .await?
        .into_iter()
        .filter_map(|p| p.get("Profile").and_then(|v| String::try_from(v.clone()).ok()))
        .collect();
    let degraded = proxy.performance_degraded().await.unwrap_or_default();
    Ok(Update::Profiles { active: proxy.active_profile().await?, offered, degraded })
}

impl Power {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Battery(b) => self.battery.set(b),
            Update::OnBattery(on) => self.on_battery.set(on),
            Update::Profiles { active, offered, degraded } => {
                self.profile.set(active);
                self.profiles.set(offered);
                self.degraded.set(degraded);
            }
        });
    }

    /// Starts watching on the bus thread; updates arrive through `post`.
    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some(bus.clone());
        let to = post.clone();
        bus.spawn("upower", async move {
            let conn = bus::system().await?;
            let device = DeviceProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
            let upower = UPowerProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
            let _ = to.send(Update::Battery(battery(&device).await?));
            let _ = to.send(Update::OnBattery(upower.on_battery().await?));
            let device_props = zbus::fdo::PropertiesProxy::builder(&conn)
                .destination("org.freedesktop.UPower")
                .path("/org/freedesktop/UPower/devices/DisplayDevice")
                .build()
                .await?;
            let root_props = zbus::fdo::PropertiesProxy::builder(&conn)
                .destination("org.freedesktop.UPower")
                .path("/org/freedesktop/UPower")
                .build()
                .await?;
            // One stream of both objects' changes: false is the device, true
            // the daemon's own OnBattery.
            let device_changed = device_props.receive_properties_changed().await?.map(|_| false);
            let root_changed = root_props.receive_properties_changed().await?.map(|_| true);
            let mut changed = futures_util::stream::select(device_changed, root_changed);
            while let Some(root) = changed.next().await {
                let update = if root {
                    Update::OnBattery(upower.on_battery().await?)
                } else {
                    Update::Battery(battery(&device).await?)
                };
                let _ = to.send(update);
            }
            Ok(())
        });
        bus.spawn("power-profiles", async move {
            let conn = bus::system().await?;
            let proxy = ProfilesProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
            let _ = post.send(profiles(&proxy).await?);
            let props = zbus::fdo::PropertiesProxy::builder(&conn)
                .destination("org.freedesktop.UPower.PowerProfiles")
                .path("/org/freedesktop/UPower/PowerProfiles")
                .build()
                .await?;
            let mut changed = props.receive_properties_changed().await?;
            while changed.next().await.is_some() {
                let _ = post.send(profiles(&proxy).await?);
            }
            Ok(())
        });
    }

    /// Asks the daemon for `profile`; the change comes back as a signal.
    pub fn set_profile(&self, profile: String) {
        let Some(bus) = self.bus.borrow().clone() else {
            log::warn!("power: set_profile before the service started");
            return;
        };
        bus.spawn("power-profiles set", async move {
            let conn = bus::system().await?;
            let proxy = ProfilesProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
            proxy.set_active_profile(&profile).await
        });
    }
}
