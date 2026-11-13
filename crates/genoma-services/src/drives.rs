//! Removable drives through udisks2 (`@genoma/drives`): what is plugged
//! in, its volumes, where they are mounted and how much room is left,
//! whether it is being written to, and mounting, unmounting and ejecting.
//!
//! udisks is read whole (`GetManagedObjects`) once at the start and again
//! after any of its signals, once a burst has settled: plugging a stick in
//! is a dozen objects arriving. The write rate is the kernel's own count
//! (`/sys/block/<disk>/stat`), so a copy finished for the program that made
//! it but still in the page cache is still being written, and the stick is
//! not yet safe to pull. It and the free space are read once a second, and
//! only while a removable volume is mounted; otherwise nothing is read.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{
    MatchRule, MessageStream, OwnedObjectPath, Variant as Value,
    message::Type,
    zvariant::OwnedVariant,
};

use crate::bus::{self, Bus};

const UDISKS: &str = "org.freedesktop.UDisks2";
const SETTLE: Duration = Duration::from_millis(250);
const SAMPLE: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Volume {
    /// "/dev/sdb1".
    pub device: String,
    pub label: String,
    /// The filesystem: "vfat", "exfat", "ext4"…
    pub kind: String,
    /// Bytes.
    pub size: u64,
    pub mounts: Vec<String>,
    /// Bytes left on its first mount, while mounted.
    pub free: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drive {
    /// udisks' object path: stable while it stays plugged in.
    pub id: String,
    /// Vendor and model, else the first label, else the device.
    pub name: String,
    /// The whole disk, "/dev/sdb": what is powered off and counted.
    pub device: String,
    pub can_power_off: bool,
    /// A loop device set up by the user, counted with `include_loops`.
    pub is_loop: bool,
    /// Bytes a second being written to it now; 0 while nothing is mounted.
    pub rate: f64,
    pub volumes: Vec<Volume>,
    /// "mounting", "unmounting", "ejecting", or "".
    pub busy: &'static str,
    /// Why the last action on it failed, or "".
    pub error: String,
}

pub enum Update {
    /// The drives, and every block device's udisks path.
    Drives(Vec<Drive>, HashMap<String, String>),
    /// Rates and free space, by drive id and by volume device.
    Usage(HashMap<String, f64>, HashMap<String, u64>),
    Busy(String, &'static str, String),
}

pub struct Drives {
    pub drives: Signal<Vec<Drive>>,
    loops: Arc<AtomicBool>,
    bus: RefCell<Option<(Bus, Sender<Update>)>>,
    /// Block device → udisks object path, from the last read.
    blocks: RefCell<HashMap<String, String>>,
}

impl Default for Drives {
    fn default() -> Self {
        Drives { drives: Signal::new(Vec::new()), loops: Arc::default(), bus: RefCell::new(None), blocks: RefCell::default() }
    }
}

type Props = HashMap<String, OwnedVariant>;
type Interfaces = HashMap<String, Props>;

fn get<T: TryFrom<OwnedVariant>>(props: &Props, key: &str) -> Option<T> {
    props.get(key).and_then(|v| T::try_from(v.clone()).ok())
}

fn string(props: &Props, key: &str) -> String {
    get::<String>(props, key).unwrap_or_default().trim().to_owned()
}

/// udisks keeps paths as NUL-terminated bytes.
fn bytes(props: &Props, key: &str) -> String {
    let raw: Vec<u8> = get(props, key).unwrap_or_default();
    String::from_utf8_lossy(&raw.into_iter().filter(|b| *b != 0).collect::<Vec<_>>()).into_owned()
}

fn removable(drive: &Props) -> bool {
    get::<bool>(drive, "Removable").unwrap_or(false)
        || get::<bool>(drive, "MediaRemovable").unwrap_or(false)
        || matches!(string(drive, "ConnectionBus").as_str(), "usb" | "sdio" | "ieee1394")
}

/// The removable drives and what is on them, and every block device's path.
pub fn model(objects: &HashMap<String, Interfaces>, loops: bool, uid: u32) -> (Vec<Drive>, HashMap<String, String>) {
    let mut drives: HashMap<String, Drive> = HashMap::new();
    let mut blocks = HashMap::new();
    for (path, ifaces) in objects {
        let Some(drive) = ifaces.get("org.freedesktop.UDisks2.Drive") else { continue };
        if !removable(drive) {
            continue;
        }
        let name = [string(drive, "Vendor"), string(drive, "Model")].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
        let can_power_off = get(drive, "CanPowerOff").unwrap_or(false);
        drives.insert(path.clone(), Drive { id: path.clone(), name, can_power_off, ..Drive::default() });
    }
    for (path, ifaces) in objects {
        let Some(block) = ifaces.get("org.freedesktop.UDisks2.Block") else { continue };
        if get(block, "HintIgnore").unwrap_or(false) {
            continue;
        }
        let device = Some(bytes(block, "PreferredDevice")).filter(|d| !d.is_empty()).unwrap_or_else(|| bytes(block, "Device"));
        blocks.insert(device.clone(), path.clone());
        let mut owner = get::<OwnedObjectPath>(block, "Drive").map(|p| p.as_str().to_owned()).unwrap_or_default();
        // A loop device has no drive: it is its own.
        if let Some(lp) = ifaces.get("org.freedesktop.UDisks2.Loop") {
            if loops && get::<u32>(lp, "SetupByUID") == Some(uid) {
                owner = path.clone();
                let backing = bytes(lp, "BackingFile");
                let name = backing.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(&device).to_owned();
                drives.entry(owner.clone()).or_insert_with(|| Drive { id: owner.clone(), name, device: device.clone(), is_loop: true, ..Drive::default() });
            }
        }
        let Some(drive) = drives.get_mut(&owner) else { continue };
        if !ifaces.contains_key("org.freedesktop.UDisks2.Partition") && owner != *path {
            // The whole disk: what is counted and powered off.
            drive.device = device.clone();
        }
        let Some(fs) = ifaces.get("org.freedesktop.UDisks2.Filesystem") else { continue };
        let mounts: Vec<Vec<u8>> = get(fs, "MountPoints").unwrap_or_default();
        let mounts = mounts
            .into_iter()
            .map(|m| String::from_utf8_lossy(&m.into_iter().filter(|b| *b != 0).collect::<Vec<_>>()).into_owned())
            .filter(|m| !m.is_empty())
            .collect();
        let label = Some(string(block, "IdLabel")).filter(|l| !l.is_empty()).unwrap_or_else(|| string(block, "HintName"));
        drive.volumes.push(Volume { device, label, kind: string(block, "IdType"), size: get(block, "Size").unwrap_or(0), mounts, free: None });
    }
    let mut list: Vec<Drive> = drives
        .into_values()
        .filter_map(|mut d| {
            // A card reader with no card has nothing to open.
            if d.volumes.is_empty() {
                return None;
            }
            d.volumes.sort_by(|a, b| a.device.cmp(&b.device));
            if d.device.is_empty() {
                d.device = d.volumes[0].device.clone();
            }
            if d.name.is_empty() {
                d.name = d.volumes.iter().map(|v| v.label.clone()).find(|l| !l.is_empty()).unwrap_or_else(|| d.device.rsplit('/').next().unwrap_or("").to_owned());
            }
            Some(d)
        })
        .collect();
    list.sort_by(|a, b| a.device.cmp(&b.device));
    (list, blocks)
}

async fn read(conn: &zbus::Connection, loops: bool) -> zbus::Result<(Vec<Drive>, HashMap<String, String>)> {
    let manager = zbus::fdo::ObjectManagerProxy::builder(conn).destination(UDISKS).path("/org/freedesktop/UDisks2").build().await?;
    let objects: HashMap<String, Interfaces> = manager
        .get_managed_objects()
        .await?
        .into_iter()
        .map(|(path, ifaces)| {
            let ifaces = ifaces.into_iter().map(|(name, props)| (name.to_string(), props.into_iter().map(|(k, v)| (k.to_string(), v)).collect())).collect();
            (path.as_str().to_owned(), ifaces)
        })
        .collect();
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    Ok(model(&objects, loops, uid))
}

/// Field 7 of `/sys/block/<disk>/stat`: sectors written, always 512 bytes.
fn sectors_written(device: &str) -> Option<u64> {
    let name = std::fs::canonicalize(device).ok()?.file_name()?.to_str()?.to_owned();
    std::fs::read_to_string(format!("/sys/block/{name}/stat")).ok()?.split_whitespace().nth(6)?.parse().ok()
}

fn free_bytes(mount: &str) -> Option<u64> {
    let path = std::ffi::CString::new(mount).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a zeroed struct for the kernel to fill.
    (unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0).then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// udisks' refusal in words a person can act on.
fn reason(e: &zbus::Error) -> String {
    if let zbus::Error::MethodError(name, said, _) = e {
        let name = name.as_str();
        if name.ends_with("DeviceBusy") {
            return "In use: close what has a file open on it".into();
        }
        if name.contains("NotAuthorized") {
            return "Not allowed".into();
        }
        if let Some(said) = said {
            return said.rsplit(": ").next().unwrap_or(said).trim().to_owned();
        }
        return name.rsplit('.').next().unwrap_or(name).to_owned();
    }
    e.to_string()
}

async fn call(conn: &zbus::Connection, path: &str, interface: &str, method: &str) -> zbus::Result<()> {
    let proxy = zbus::Proxy::new(conn, UDISKS, path.to_owned(), interface.to_owned()).await?;
    let options: HashMap<&str, Value> = HashMap::new();
    proxy.call_method(method, &(options,)).await.map(|_| ())
}

/// What an action does, step by step, once the paths are known.
enum Step {
    Mount(String),
    Unmount(String),
    PowerOff(String),
    DeleteLoop(String),
}

impl Drives {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Drives(fresh, blocks) => {
                *self.blocks.borrow_mut() = blocks;
                let old = self.drives.get_untracked().unwrap_or_default();
                let list: Vec<Drive> = fresh
                    .into_iter()
                    .map(|mut d| {
                        if let Some(was) = old.iter().find(|o| o.id == d.id) {
                            (d.busy, d.error, d.rate) = (was.busy, was.error.clone(), was.rate);
                            for v in &mut d.volumes {
                                if !v.mounts.is_empty() {
                                    v.free = was.volumes.iter().find(|o| o.device == v.device).and_then(|o| o.free);
                                }
                            }
                        }
                        d
                    })
                    .collect();
                if old != list {
                    self.drives.set(list);
                }
            }
            Update::Usage(rates, free) => {
                let old = self.drives.get_untracked().unwrap_or_default();
                let mut list = old.clone();
                for d in &mut list {
                    d.rate = rates.get(&d.id).copied().unwrap_or(0.0);
                    for v in &mut d.volumes {
                        v.free = free.get(&v.device).copied();
                    }
                }
                if old != list {
                    self.drives.set(list);
                }
            }
            Update::Busy(id, busy, error) => {
                let mut list = self.drives.get_untracked().unwrap_or_default();
                if let Some(d) = list.iter_mut().find(|d| d.id == id) {
                    (d.busy, d.error) = (busy, error);
                    self.drives.set(list);
                }
            }
        });
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some((bus.clone(), post.clone()));
        let (pause, loops) = (bus.clone(), self.loops.clone());
        bus.spawn("drives", async move {
            let conn = bus::system().await?;
            let rule = MatchRule::builder().msg_type(Type::Signal).sender(UDISKS).build()?;
            let mut stream = pause.drained("drives signals", MessageStream::for_match_rule(rule, &conn, None).await?);
            let (mut drives, blocks) = read(&conn, loops.load(Ordering::Relaxed)).await?;
            let _ = post.send(Update::Drives(drives.clone(), blocks));
            let mut written: HashMap<String, (u64, Instant)> = HashMap::new();
            loop {
                let mounted = drives.iter().any(|d| d.volumes.iter().any(|v| !v.mounts.is_empty()));
                let tick = pause.clone();
                let tick = Box::pin(async move {
                    if mounted { tick.sleep(SAMPLE).await } else { std::future::pending::<()>().await }
                });
                match futures_util::future::select(stream.next(), tick).await {
                    futures_util::future::Either::Left((None, _)) => break,
                    futures_util::future::Either::Left((Some(_), _)) => {
                        pause.sleep(SETTLE).await;
                        while let Some(Some(_)) = futures_util::FutureExt::now_or_never(stream.next()) {}
                        match read(&conn, loops.load(Ordering::Relaxed)).await {
                            Ok((fresh, blocks)) => {
                                drives = fresh;
                                let _ = post.send(Update::Drives(drives.clone(), blocks));
                            }
                            Err(e) => log::warn!("drives: {e}"),
                        }
                    }
                    futures_util::future::Either::Right(_) => {}
                }
                // The rate and the room left, while anything is mounted.
                let now = Instant::now();
                let mut rates = HashMap::new();
                let mut free = HashMap::new();
                for d in &drives {
                    let mounted: Vec<&Volume> = d.volumes.iter().filter(|v| !v.mounts.is_empty()).collect();
                    if mounted.is_empty() {
                        written.remove(&d.id);
                        continue;
                    }
                    for v in mounted {
                        if let Some(bytes) = free_bytes(&v.mounts[0]) {
                            free.insert(v.device.clone(), bytes);
                        }
                    }
                    let count = sectors_written(&d.device);
                    if let (Some(count), Some((last, at))) = (count, written.get(&d.id)) {
                        let seconds = now.duration_since(*at).as_secs_f64();
                        if seconds > 0.0 {
                            rates.insert(d.id.clone(), (count.saturating_sub(*last) * 512) as f64 / seconds);
                        }
                    }
                    if let Some(count) = count {
                        written.insert(d.id.clone(), (count, now));
                    }
                }
                let _ = post.send(Update::Usage(rates, free));
            }
            Ok(())
        });
    }

    /// Counts loop devices set up by this user as drives: a stick to try
    /// an interface with, made of a file.
    pub fn include_loops(&self, on: bool) {
        if self.loops.swap(on, Ordering::Relaxed) == on {
            return;
        }
        let Some((bus, post)) = self.bus.borrow().clone() else { return };
        let loops = self.loops.clone();
        bus.spawn("drives", async move {
            let conn = bus::system().await?;
            let (drives, blocks) = read(&conn, loops.load(Ordering::Relaxed)).await?;
            let _ = post.send(Update::Drives(drives, blocks));
            Ok(())
        });
    }

    fn drive_of_volume(&self, device: &str) -> Option<Drive> {
        self.drives.get_untracked().unwrap_or_default().into_iter().find(|d| d.volumes.iter().any(|v| v.device == device))
    }

    /// Runs `steps` on the bus thread, the drive marked `busy` until they
    /// end, and their failure kept on it.
    fn act(&self, drive: &Drive, busy: &'static str, steps: Vec<Step>) -> Result<(), String> {
        if !drive.busy.is_empty() {
            return Err(format!("{} is {}", drive.name, drive.busy));
        }
        let Some((bus, post)) = self.bus.borrow().clone() else { return Err("drives: the service has not started".into()) };
        let id = drive.id.clone();
        self.apply(Update::Busy(id.clone(), busy, String::new()));
        let loops = self.loops.clone();
        bus.spawn("drives action", async move {
            let conn = bus::system().await?;
            let mut error = String::new();
            for step in steps {
                let result = match &step {
                    Step::Mount(p) => call(&conn, p, "org.freedesktop.UDisks2.Filesystem", "Mount").await,
                    Step::Unmount(p) => call(&conn, p, "org.freedesktop.UDisks2.Filesystem", "Unmount").await,
                    Step::PowerOff(p) => call(&conn, p, "org.freedesktop.UDisks2.Drive", "PowerOff").await,
                    Step::DeleteLoop(p) => call(&conn, p, "org.freedesktop.UDisks2.Loop", "Delete").await,
                };
                if let Err(e) = result {
                    error = reason(&e);
                    break;
                }
            }
            let _ = post.send(Update::Busy(id, "", error));
            // The signals bring the new state; ask once more in case they
            // came before the mark went.
            if let Ok((drives, blocks)) = read(&conn, loops.load(Ordering::Relaxed)).await {
                let _ = post.send(Update::Drives(drives, blocks));
            }
            Ok(())
        });
        Ok(())
    }

    fn block_path(&self, device: &str) -> Result<String, String> {
        self.blocks.borrow().get(device).cloned().ok_or_else(|| format!("no volume {device}"))
    }

    pub fn mount(&self, device: &str) -> Result<(), String> {
        let drive = self.drive_of_volume(device).ok_or_else(|| format!("no volume {device}"))?;
        self.act(&drive, "mounting", vec![Step::Mount(self.block_path(device)?)])
    }

    pub fn unmount(&self, device: &str) -> Result<(), String> {
        let drive = self.drive_of_volume(device).ok_or_else(|| format!("no volume {device}"))?;
        self.act(&drive, "unmounting", vec![Step::Unmount(self.block_path(device)?)])
    }

    /// Unmounts every mounted volume, which waits for the last of the
    /// writing, then powers the drive off (a loop device is deleted), so it
    /// leaves the list when it really is safe to pull.
    pub fn eject(&self, id: &str) -> Result<(), String> {
        let drive = self.drives.get_untracked().unwrap_or_default().into_iter().find(|d| d.id == id).ok_or_else(|| format!("no drive {id}"))?;
        let mut steps = Vec::new();
        for v in drive.volumes.iter().filter(|v| !v.mounts.is_empty()) {
            steps.push(Step::Unmount(self.block_path(&v.device)?));
        }
        if drive.is_loop {
            steps.push(Step::DeleteLoop(drive.id.clone()));
        } else if drive.can_power_off {
            steps.push(Step::PowerOff(drive.id.clone()));
        }
        self.act(&drive, "ejecting", steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v<T: Into<Value<'static>>>(value: T) -> OwnedVariant {
        OwnedVariant::try_from(value.into()).unwrap()
    }

    fn props(list: Vec<(&str, OwnedVariant)>) -> Props {
        list.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    }

    fn path(p: &str) -> OwnedVariant {
        v(OwnedObjectPath::try_from(p.to_owned()).unwrap())
    }

    #[test]
    fn a_stick_and_its_volumes() {
        let drive = "/org/freedesktop/UDisks2/drives/Stick";
        let mut objects: HashMap<String, Interfaces> = HashMap::new();
        objects.insert(drive.into(), HashMap::from([("org.freedesktop.UDisks2.Drive".into(), props(vec![
            ("Vendor", v("SanDisk ")), ("Model", v("Cruzer")), ("Removable", v(true)), ("CanPowerOff", v(true)),
        ]))]));
        objects.insert("/org/freedesktop/UDisks2/drives/Nvme".into(), HashMap::from([("org.freedesktop.UDisks2.Drive".into(), props(vec![
            ("Model", v("Internal")), ("ConnectionBus", v("")),
        ]))]));
        objects.insert("/b/sdb".into(), HashMap::from([("org.freedesktop.UDisks2.Block".into(), props(vec![
            ("Device", v(b"/dev/sdb\0".to_vec())), ("Drive", path(drive)), ("Size", v(8_000_000_000u64)),
        ]))]));
        objects.insert("/b/sdb1".into(), HashMap::from([
            ("org.freedesktop.UDisks2.Block".into(), props(vec![
                ("Device", v(b"/dev/sdb1\0".to_vec())), ("Drive", path(drive)), ("IdLabel", v("PHOTOS")), ("IdType", v("vfat")), ("Size", v(7_900_000_000u64)),
            ])),
            ("org.freedesktop.UDisks2.Partition".into(), Props::new()),
            ("org.freedesktop.UDisks2.Filesystem".into(), props(vec![("MountPoints", v(vec![b"/run/media/me/PHOTOS\0".to_vec()]))])),
        ]));
        let (list, blocks) = model(&objects, false, 1000);
        assert_eq!(list.len(), 1, "the internal drive is left out");
        let d = &list[0];
        assert_eq!((d.name.as_str(), d.device.as_str(), d.can_power_off), ("SanDisk Cruzer", "/dev/sdb", true));
        assert_eq!(d.volumes.len(), 1);
        assert_eq!((d.volumes[0].label.as_str(), d.volumes[0].kind.as_str()), ("PHOTOS", "vfat"));
        assert_eq!(d.volumes[0].mounts, ["/run/media/me/PHOTOS"]);
        assert_eq!(blocks.get("/dev/sdb1").map(String::as_str), Some("/b/sdb1"));
    }
}
