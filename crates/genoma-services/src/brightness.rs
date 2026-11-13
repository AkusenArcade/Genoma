//! Display brightness (`@genoma/brightness`), from two places:
//!
//! - **backlight**: `/sys/class/backlight`, the panel the kernel drives (a
//!   laptop's). Written through logind's `Session.SetBrightness`, which
//!   needs no root and no helper.
//! - **DDC/CI**: external monitors over I²C, through `ddcutil` (when it is
//!   installed and the user may open `/dev/i2c-*`).
//!
//! DDC is slow: detection takes seconds, a read a tenth of one and a write
//! a third. So monitors are detected once, on a thread of their own, and
//! addressed by bus from then on; a value set while a write is under way
//! replaces the one waiting, so a control being dragged neither queues
//! every step nor waits for each. Nothing polls: each display is read once,
//! and after that the interface's own writes move the value
//! ([`Brightness::refresh`] reads again, after a change made elsewhere).

use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::Path,
    process::Command,
    sync::mpsc,
};

use calloop::channel::Sender;
use genoma_core::reactive::{Signal, batch};
use zbus::proxy::CacheProperties;

use crate::bus::{self, Bus};

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    /// "backlight" or "ddc".
    pub kind: &'static str,
    /// The DRM connector it is on ("eDP-1", "DP-2"), or, where nothing says,
    /// the backlight device's name or "display-<n>".
    pub connector: String,
    /// The monitor's model, over DDC; empty for a backlight.
    pub model: String,
    /// 0…1, or none until it has been read.
    pub value: Option<f64>,
    /// The hardware's own top step: what `value` is a fraction of.
    pub max: u32,
    /// The backlight device ("amdgpu_bl1"), or the I²C bus ("7").
    pub address: String,
}

/// What the threads post.
pub enum Update {
    Detecting(bool),
    /// The monitors DDC found, values unread.
    Found(Vec<Display>),
    /// A read: connector, raw value, max.
    Read(String, u32, u32),
    Error(String),
}

pub struct Brightness {
    pub displays: Signal<Vec<Display>>,
    /// DDC detection is under way: more displays may come.
    pub detecting: Signal<bool>,
    /// The last write's failure, or "".
    pub error: Signal<String>,
    started: RefCell<Option<(Option<Bus>, Sender<Update>)>>,
    /// The latest DDC target per bus, for the writer thread.
    writes: RefCell<Option<mpsc::Sender<(String, u32)>>>,
}

impl Default for Brightness {
    fn default() -> Self {
        Brightness {
            displays: Signal::new(Vec::new()),
            detecting: Signal::new(false),
            error: Signal::new(String::new()),
            started: RefCell::new(None),
            writes: RefCell::new(None),
        }
    }
}

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1/session/auto")]
trait LoginSession {
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

fn number(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The backlight that drives the panel. A machine can have several and only
/// one moves the panel: the kernel hangs that one under its DRM connector
/// (`…/card1-eDP-1/amdgpu_bl1`), which says it is connected. A hybrid
/// laptop's `nvidia_0` beside it took every write while the panel stayed
/// put. So: a connected connector's, else any but `acpi_video`, else
/// `acpi_video`; the first among equals.
pub fn backlight() -> Option<Display> {
    let mut best: Option<(u8, Display)> = None;
    let mut entries: Vec<_> = fs::read_dir("/sys/class/backlight").ok()?.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let device = path.file_name()?.to_string_lossy().into_owned();
        let (Some(value), Some(max)) = (number(&path.join("brightness")), number(&path.join("max_brightness"))) else { continue };
        if max == 0 {
            continue;
        }
        let parent = fs::canonicalize(&path).ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let connected = parent.as_ref().and_then(|p| fs::read_to_string(p.join("status")).ok()).is_some_and(|s| s.trim() == "connected");
        let connector = parent
            .as_ref()
            .and_then(|p| p.file_name()?.to_str().map(str::to_owned))
            .and_then(|name| connector_of(&name))
            .unwrap_or_else(|| device.clone());
        let rank = if connected { 3 } else if device.starts_with("acpi_video") { 1 } else { 2 };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            let display = Display { kind: "backlight", connector, model: String::new(), value: Some(value as f64 / max as f64), max, address: device };
            best = Some((rank, display));
        }
    }
    best.map(|(_, d)| d)
}

/// "card1-eDP-1" → "eDP-1".
fn connector_of(drm_name: &str) -> Option<String> {
    let rest = drm_name.strip_prefix("card")?;
    let (_, connector) = rest.split_once('-')?;
    (!connector.is_empty()).then(|| connector.to_owned())
}

/// `ddcutil detect --brief`: one block per display, `Display N`, then
/// `I2C bus: /dev/i2c-7`, `DRM connector: card1-DP-2`, `Monitor:
/// MFG:MODEL:SERIAL` (any part may be empty). Blocks marked `Invalid
/// display` have no number and are left out.
pub fn parse_detect(text: &str) -> Vec<Display> {
    let mut out = Vec::new();
    let mut current: Option<(u32, Display)> = None;
    let mut finish = |current: &mut Option<(u32, Display)>| {
        if let Some((n, mut d)) = current.take() {
            if d.address.is_empty() {
                return;
            }
            if d.connector.is_empty() {
                d.connector = format!("display-{n}");
            }
            out.push(d);
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(n) = line.strip_prefix("Display ").and_then(|r| r.trim().parse::<u32>().ok()) {
            finish(&mut current);
            current = Some((n, Display { kind: "ddc", connector: String::new(), model: String::new(), value: None, max: 100, address: String::new() }));
        } else if !line.starts_with([' ', '\t']) && !trimmed.is_empty() {
            finish(&mut current);
        } else if let Some((_, d)) = current.as_mut() {
            if let Some(bus) = trimmed.strip_prefix("I2C bus:") {
                d.address = bus.trim().trim_start_matches("/dev/i2c-").to_owned();
            } else if let Some(drm) = trimmed.strip_prefix("DRM connector:") {
                d.connector = connector_of(drm.trim()).unwrap_or_default();
            } else if let Some(monitor) = trimmed.strip_prefix("Monitor:") {
                d.model = monitor.trim().split(':').nth(1).unwrap_or("").trim().to_owned();
            }
        }
    }
    finish(&mut current);
    out
}

/// `getvcp 10 --brief`: `VCP 10 C <current> <max>`.
pub fn parse_getvcp(text: &str) -> Option<(u32, u32)> {
    let words: Vec<&str> = text.lines().find(|l| l.trim_start().starts_with("VCP"))?.split_whitespace().collect();
    let (current, max) = (words.get(3)?.parse().ok()?, words.get(4)?.parse().ok()?);
    (max > 0).then_some((current, max))
}

fn ddcutil(args: &[&str]) -> Result<String, String> {
    let out = Command::new("ddcutil").args(args).output().map_err(|e| format!("ddcutil: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = if said.trim().is_empty() { String::from_utf8_lossy(&out.stdout).into_owned() } else { said.into_owned() };
        Err(said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ddcutil failed").trim().to_owned())
    }
}

fn read_bus(post: &Sender<Update>, connector: &str, bus: &str) {
    match ddcutil(&["--bus", bus, "getvcp", "10", "--brief"]) {
        Ok(text) => {
            if let Some((current, max)) = parse_getvcp(&text) {
                let _ = post.send(Update::Read(connector.to_owned(), current, max));
            }
        }
        Err(e) => log::warn!("brightness: {connector}: {e}"),
    }
}

fn ddcutil_installed() -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("ddcutil").is_file()))
}

impl Brightness {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Detecting(on) => self.detecting.set(on),
            Update::Found(found) => {
                let mut list: Vec<Display> = self.displays.get_untracked().unwrap_or_default().into_iter().filter(|d| d.kind != "ddc").collect();
                list.extend(found);
                self.displays.set(list);
            }
            Update::Read(connector, value, max) => {
                let list = self.displays.get_untracked().unwrap_or_default();
                let list = list
                    .into_iter()
                    .map(|d| if d.connector == connector { Display { value: Some(value as f64 / max as f64), max, ..d } } else { d })
                    .collect();
                self.displays.set(list);
            }
            Update::Error(said) => self.error.set(said),
        });
    }

    /// Reads the backlight now and starts DDC detection on its own thread;
    /// what it finds arrives through `post`. Without a bus the backlight is
    /// shown but cannot be written.
    pub fn start(&self, bus: Option<Bus>, post: Sender<Update>) {
        if let Some(panel) = backlight() {
            self.displays.set(vec![panel]);
        }
        *self.started.borrow_mut() = Some((bus, post.clone()));
        if !ddcutil_installed() {
            return;
        }
        let (writes, wanted) = mpsc::channel::<(String, u32)>();
        *self.writes.borrow_mut() = Some(writes);
        self.detecting.set(true);
        let spawned = std::thread::Builder::new().name("genoma-ddc".into()).spawn(move || {
            let found = match ddcutil(&["detect", "--brief"]) {
                Ok(text) => parse_detect(&text),
                Err(e) => {
                    log::warn!("brightness: {e}");
                    Vec::new()
                }
            };
            let buses: HashMap<String, String> = found.iter().map(|d| (d.address.clone(), d.connector.clone())).collect();
            let _ = post.send(Update::Found(found.clone()));
            for d in &found {
                read_bus(&post, &d.connector, &d.address);
            }
            let _ = post.send(Update::Detecting(false));
            // Writes: the latest per bus, one bus after another.
            while let Ok(first) = wanted.recv() {
                let mut latest: Vec<(String, u32)> = vec![first];
                while let Ok((bus, value)) = wanted.try_recv() {
                    match latest.iter_mut().find(|(b, _)| *b == bus) {
                        Some(slot) => slot.1 = value,
                        None => latest.push((bus, value)),
                    }
                }
                for (bus, value) in latest {
                    let result = ddcutil(&["--bus", &bus, "setvcp", "10", &value.to_string()]);
                    let said = match result {
                        Ok(_) => String::new(),
                        Err(e) => format!("{}: {e}", buses.get(&bus).map_or(bus.as_str(), String::as_str)),
                    };
                    let _ = post.send(Update::Error(said));
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("brightness: {e}");
            self.detecting.set(false);
        }
    }

    /// The display `connector` names, or the first (the backlight first).
    fn display(&self, connector: Option<&str>) -> Option<Display> {
        let list = self.displays.get_untracked().unwrap_or_default();
        match connector {
            Some(c) => list.into_iter().find(|d| d.connector == c),
            None => list.into_iter().next(),
        }
    }

    /// 0…1, or none while the display has not answered (or is not there).
    pub fn value(&self, connector: Option<&str>) -> Option<f64> {
        let list = self.displays.get();
        match connector {
            Some(c) => list.into_iter().find(|d| d.connector == c)?.value,
            None => list.into_iter().next()?.value,
        }
    }

    /// Sets `connector`'s brightness (the first display's without one) to
    /// `fraction`, 0…1. The value moves at once; the hardware follows.
    pub fn set(&self, fraction: f64, connector: Option<&str>) -> Result<(), String> {
        let display = self.display(connector).ok_or_else(|| match connector {
            Some(c) => format!("no display {c} with a brightness"),
            None => "no display with a brightness".to_owned(),
        })?;
        let fraction = fraction.clamp(0.0, 1.0);
        let raw = (fraction * display.max as f64).round() as u32;
        let list: Vec<Display> = self
            .displays
            .get_untracked()
            .unwrap_or_default()
            .into_iter()
            .map(|d| if d.connector == display.connector { Display { value: Some(raw as f64 / d.max as f64), ..d } } else { d })
            .collect();
        self.displays.set(list);
        match display.kind {
            "backlight" => {
                let started = self.started.borrow();
                let Some((Some(bus), post)) = started.as_ref() else { return Err("no system bus to reach logind".into()) };
                let (post, device) = (post.clone(), display.address.clone());
                bus.spawn("brightness", async move {
                    let conn = bus::system().await?;
                    let session = LoginSessionProxy::builder(&conn).cache_properties(CacheProperties::No).build().await?;
                    let said = match session.set_brightness("backlight", &device, raw).await {
                        Ok(()) => String::new(),
                        Err(e) => format!("{device}: {e}"),
                    };
                    let _ = post.send(Update::Error(said));
                    Ok(())
                });
                Ok(())
            }
            _ => {
                let writes = self.writes.borrow();
                let writes = writes.as_ref().ok_or("ddcutil is not running")?;
                writes.send((display.address, raw)).map_err(|_| "the DDC thread has stopped".to_owned())
            }
        }
    }

    /// Reads every display again: after a change made elsewhere (a
    /// monitor's own buttons, a laptop's brightness keys).
    pub fn refresh(&self) {
        let list = self.displays.get_untracked().unwrap_or_default();
        if let Some(panel) = backlight() {
            let list: Vec<Display> = list.iter().cloned().map(|d| if d.kind == "backlight" { panel.clone() } else { d }).collect();
            self.displays.set(list);
        }
        let started = self.started.borrow();
        let Some((_, post)) = started.as_ref() else { return };
        let ddc: Vec<(String, String)> = list.into_iter().filter(|d| d.kind == "ddc").map(|d| (d.connector, d.address)).collect();
        if ddc.is_empty() {
            return;
        }
        let post = post.clone();
        let _ = std::thread::Builder::new().name("genoma-ddc-read".into()).spawn(move || {
            for (connector, bus) in ddc {
                read_bus(&post, &connector, &bus);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddcutil_detect_and_getvcp_read() {
        let text = "Display 1\n   I2C bus:  /dev/i2c-7\n   DRM connector:  card1-DP-2\n   Monitor:  GSM:LG ULTRAGEAR:\n\n\
                    Invalid display\n   I2C bus:  /dev/i2c-9\n   DRM connector:  card1-HDMI-A-2\n\n\
                    Display 2\n   I2C bus:  /dev/i2c-8\n   Monitor:  DEL::\n";
        let found = parse_detect(text);
        assert_eq!(found.len(), 2, "the invalid display is left out");
        assert_eq!((found[0].connector.as_str(), found[0].address.as_str(), found[0].model.as_str()), ("DP-2", "7", "LG ULTRAGEAR"));
        assert_eq!((found[1].connector.as_str(), found[1].model.as_str()), ("display-2", ""));
        assert_eq!(parse_getvcp("VCP 10 C 40 100\n"), Some((40, 100)));
        assert_eq!(parse_getvcp("VCP 10 ERR\n"), None);
        assert_eq!(connector_of("card0-eDP-1").as_deref(), Some("eDP-1"));
    }
}
