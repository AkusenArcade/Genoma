//! The machine's vital signs: processor, memory and graphics card load, each
//! 0 to 1, sampled at one interval (Bioma's `vitals.interval`, 2 s).
//!
//! Values are rounded to the per cent before they are set, so a load that
//! wanders inside one per cent wakes nobody.
//!
//! The sensors are the kernel's hwmon (`/sys/class/hwmon`): temperatures,
//! fans and power draw, whole degrees, revolutions and watts for the same
//! reason. Which files there are is read once; each sample reads only the
//! values.

use std::{fs, path::PathBuf, time::Duration};

use genoma_core::reactive::{Signal, batch};

pub const INTERVAL: Duration = Duration::from_secs(2);

/// One hwmon reading.
#[derive(Clone, Debug, PartialEq)]
pub struct Sensor {
    /// "temperature" (°C), "fan" (RPM) or "power" (W).
    pub kind: &'static str,
    /// The driver's name for the chip: "k10temp", "amdgpu", "nvme", "nct6799".
    pub chip: String,
    /// The chip's own label ("Tctl", "edge"), or the file's ("fan2").
    pub label: String,
    pub value: f64,
    /// Where the chip says it runs hot and where it is in danger, when it says.
    pub high: Option<f64>,
    pub critical: Option<f64>,
}

/// Where one reading is, found once.
struct Source {
    kind: &'static str,
    chip: String,
    label: String,
    input: PathBuf,
    /// What the file's number is divided by: millidegrees, microwatts.
    scale: f64,
    high: Option<f64>,
    critical: Option<f64>,
}

pub struct Vitals {
    pub cpu: Signal<f64>,
    pub ram: Signal<f64>,
    pub gpu: Signal<Option<f64>>,
    /// Where the card's clock sits between its lowest and highest state, 0…1:
    /// the satellite's rate. A card at a third of its load can sit at full
    /// clock, so the two stay apart (Bioma's PRD §9.2).
    pub gpu_clock: Signal<f64>,
    /// The processor's clock between its lowest and highest, 0…1: the beat's
    /// rate. Averaged over the cores (/proc/cpuinfo) and over the last five
    /// samples, against cpu0's range, as Bioma's SystemMonitor does.
    pub cpu_clock: Signal<f64>,
    /// Every hwmon temperature, fan and power reading, chip by chip.
    pub sensors: Signal<Vec<Sensor>>,
    /// The processor's temperature, °C: k10temp's Tctl (or Tdie),
    /// coretemp's package, zenpower's Tdie, or an ARM `cpu_thermal`.
    pub cpu_temp: Signal<Option<f64>>,
    /// The temperature of the card `gpu` reads, °C: its `edge`.
    pub gpu_temp: Signal<Option<f64>>,
    sources: Vec<Source>,
    /// Indexes into `sources`.
    cpu_source: Option<usize>,
    gpu_source: Option<usize>,
    clock_samples: std::cell::RefCell<Vec<f64>>,
    cpu_range: Option<(f64, f64)>,
    last_cpu: std::cell::Cell<Option<(u64, u64)>>,
    gpu_path: Option<PathBuf>,
}

fn percent(v: f64) -> f64 {
    (v.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

/// busy and total jiffies from the first line of /proc/stat.
fn cpu_times() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let fields: Vec<u64> =
        stat.lines().next()?.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    let total: u64 = fields.iter().take(8).sum();
    let idle = fields.get(3)? + fields.get(4).unwrap_or(&0);
    Some((total - idle, total))
}

fn ram() -> Option<f64> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| -> Option<f64> {
        info.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse().ok()
    };
    Some(1.0 - field("MemAvailable:")? / field("MemTotal:")?)
}

/// The card with the most VRAM, as Bioma picks it: card0 is often a small
/// integrated GPU and the interesting one the discrete card.
fn gpu_path() -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let device = entry.path().join("device");
        let busy = device.join("gpu_busy_percent");
        if !busy.exists() {
            continue;
        }
        let vram = fs::read_to_string(device.join("mem_info_vram_total"))
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        if best.as_ref().is_none_or(|(v, _)| vram > *v) {
            best = Some((vram, busy));
        }
    }
    best.map(|(_, p)| p)
}

fn number(path: &std::path::Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Every temperature, fan and power input under `/sys/class/hwmon`, in the
/// kernel's order, with the device each chip belongs to.
fn sources() -> Vec<(Source, Option<PathBuf>)> {
    let mut chips: Vec<PathBuf> = fs::read_dir("/sys/class/hwmon").map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    // hwmon2 before hwmon10.
    chips.sort_by_key(|p| p.file_name().and_then(|n| n.to_str()?.trim_start_matches("hwmon").parse::<u32>().ok()).unwrap_or(u32::MAX));
    let mut out = Vec::new();
    for dir in chips {
        let chip = fs::read_to_string(dir.join("name")).map(|n| n.trim().to_owned()).unwrap_or_default();
        let device = fs::canonicalize(dir.join("device")).ok();
        let mut inputs: Vec<(String, u32, &'static str)> = fs::read_dir(&dir)
            .map(|d| {
                d.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().into_string().ok()?;
                        let (stem, kind) = if let Some(s) = name.strip_suffix("_input") {
                            (s.to_owned(), None)
                        } else {
                            (name.strip_suffix("_average")?.to_owned(), Some("power"))
                        };
                        let (prefix, n) = stem.split_at(stem.find(|c: char| c.is_ascii_digit())?);
                        let kind = kind.or(match prefix {
                            "temp" => Some("temperature"),
                            "fan" => Some("fan"),
                            "power" => Some("power"),
                            _ => None,
                        })?;
                        Some((name, n.parse().ok()?, kind))
                    })
                    .collect()
            })
            .unwrap_or_default();
        inputs.sort_by_key(|(_, n, kind)| (*kind, *n));
        // amdgpu gives power1_average or power1_input, rarely both.
        inputs.dedup_by(|a, b| a.2 == "power" && a.2 == b.2 && a.1 == b.1);
        for (file, n, kind) in inputs {
            let stem = match kind {
                "temperature" => format!("temp{n}"),
                "fan" => format!("fan{n}"),
                _ => format!("power{n}"),
            };
            let scale = match kind {
                "temperature" => 1000.0,
                "power" => 1_000_000.0,
                _ => 1.0,
            };
            let label = fs::read_to_string(dir.join(format!("{stem}_label"))).map(|l| l.trim().to_owned()).unwrap_or_else(|_| stem.clone());
            // nvme writes 65261.85 °C (0xffff kelvin) for "no limit".
            let limit = |suffix: &str| number(&dir.join(format!("{stem}_{suffix}"))).map(|v| v / scale).filter(|v| *v > 0.0 && *v < 200.0);
            let (high, critical) = if kind == "temperature" { (limit("max"), limit("crit")) } else { (None, None) };
            let source = Source { kind, chip: chip.clone(), label, input: dir.join(file), scale, high, critical };
            out.push((source, device.clone()));
        }
    }
    out
}

/// The processor's temperature among the sources, best first.
fn cpu_source(sources: &[Source]) -> Option<usize> {
    let wanted = [("k10temp", "Tctl"), ("k10temp", "Tdie"), ("zenpower", "Tdie"), ("coretemp", "Package id 0"), ("cpu_thermal", "temp1")];
    wanted.iter().find_map(|(chip, label)| sources.iter().position(|s| s.kind == "temperature" && s.chip == *chip && s.label == *label))
}

/// cpu0's lowest and highest clock, MHz.
fn cpu_range() -> Option<(f64, f64)> {
    let read = |f: &str| -> Option<f64> {
        let khz: f64 = fs::read_to_string(format!("/sys/devices/system/cpu/cpu0/cpufreq/{f}")).ok()?.trim().parse().ok()?;
        Some(khz / 1000.0)
    };
    Some((read("cpuinfo_min_freq")?, read("cpuinfo_max_freq")?))
}

impl Vitals {
    pub fn new() -> Vitals {
        let gpu_path = gpu_path();
        let found = sources();
        // The card `gpu` reads: its hwmon is under the same device.
        let card = gpu_path.as_ref().and_then(|p| fs::canonicalize(p.parent()?).ok());
        let gpu_source = card.and_then(|card| {
            let on_card: Vec<usize> =
                found.iter().enumerate().filter(|(_, (s, d))| s.kind == "temperature" && d.as_ref() == Some(&card)).map(|(i, _)| i).collect();
            on_card.iter().copied().find(|i| found[*i].0.label == "edge").or(on_card.first().copied())
        });
        let sources: Vec<Source> = found.into_iter().map(|(s, _)| s).collect();
        let v = Vitals {
            cpu: Signal::new(0.0),
            ram: Signal::new(0.0),
            gpu: Signal::new(None),
            gpu_clock: Signal::new(0.0),
            cpu_clock: Signal::new(0.0),
            sensors: Signal::new(Vec::new()),
            cpu_temp: Signal::new(None),
            gpu_temp: Signal::new(None),
            cpu_source: cpu_source(&sources),
            gpu_source,
            sources,
            clock_samples: Default::default(),
            cpu_range: cpu_range(),
            last_cpu: std::cell::Cell::new(cpu_times()),
            gpu_path,
        };
        v.sample();
        v
    }

    /// Reads every source once; called by the event loop at `INTERVAL`.
    pub fn sample(&self) {
        let now = cpu_times();
        let cpu = match (self.last_cpu.get(), now) {
            (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => Some((b1 - b0) as f64 / (t1 - t0) as f64),
            _ => None,
        };
        self.last_cpu.set(now);
        let gpu = self
            .gpu_path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|p| percent(p / 100.0));
        // pp_dpm_sclk: one state a line, "1: 1545Mhz *", the current one starred.
        let clock = self.gpu_path.as_ref().and_then(|p| fs::read_to_string(p.with_file_name("pp_dpm_sclk")).ok()).and_then(|t| {
            let states: Vec<(f64, bool)> = t
                .lines()
                .filter_map(|l| {
                    let mhz = l.split_whitespace().nth(1)?.trim_end_matches("Mhz").parse().ok()?;
                    Some((mhz, l.contains('*')))
                })
                .collect();
            let (min, max) = states.iter().fold((f64::MAX, 0.0f64), |(a, b), (m, _)| (a.min(*m), b.max(*m)));
            let current = states.iter().find(|(_, c)| *c)?.0;
            (max > min).then(|| percent((current - min) / (max - min)))
        });
        let cpu_clock = self.cpu_range.and_then(|(min, max)| {
            let info = fs::read_to_string("/proc/cpuinfo").ok()?;
            let mhz: Vec<f64> = info
                .lines()
                .filter(|l| l.starts_with("cpu MHz"))
                .filter_map(|l| l.split(':').nth(1)?.trim().parse().ok())
                .collect();
            if mhz.is_empty() || max <= min {
                return None;
            }
            let mut samples = self.clock_samples.borrow_mut();
            samples.push(mhz.iter().sum::<f64>() / mhz.len() as f64);
            if samples.len() > 5 {
                samples.remove(0);
            }
            let average = samples.iter().sum::<f64>() / samples.len() as f64;
            Some(percent((average - min) / (max - min)))
        });
        // Whole units; a fan rounded to ten revolutions, which it wanders by.
        let values: Vec<Option<f64>> = self
            .sources
            .iter()
            .map(|s| {
                let raw = number(&s.input)? / s.scale;
                // A Super I/O input with nothing on it reads -62, -128 or 127.
                if s.kind == "temperature" && !(-40.0..150.0).contains(&raw) {
                    return None;
                }
                Some(if s.kind == "fan" { (raw / 10.0).round() * 10.0 } else { raw.round() })
            })
            .collect();
        let sensors: Vec<Sensor> = self
            .sources
            .iter()
            .zip(&values)
            .filter_map(|(s, value)| {
                Some(Sensor { kind: s.kind, chip: s.chip.clone(), label: s.label.clone(), value: (*value)?, high: s.high, critical: s.critical })
            })
            .collect();
        let (cpu_temp, gpu_temp) = (self.cpu_source.and_then(|i| values[i]), self.gpu_source.and_then(|i| values[i]));
        batch(|| {
            if self.sensors.get_untracked().as_ref() != Some(&sensors) {
                self.sensors.set(sensors);
            }
            if self.cpu_temp.get_untracked() != Some(cpu_temp) {
                self.cpu_temp.set(cpu_temp);
            }
            if self.gpu_temp.get_untracked() != Some(gpu_temp) {
                self.gpu_temp.set(gpu_temp);
            }
            if let Some(clock) = cpu_clock {
                self.cpu_clock.set(clock);
            }
            if let Some(clock) = clock {
                self.gpu_clock.set(clock);
            }
            if let Some(cpu) = cpu {
                self.cpu.set(percent(cpu));
            }
            if let Some(ram) = ram() {
                self.ram.set(percent(ram));
            }
            self.gpu.set(gpu);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(chip: &str, label: &str) -> Source {
        Source { kind: "temperature", chip: chip.into(), label: label.into(), input: PathBuf::new(), scale: 1000.0, high: None, critical: None }
    }

    #[test]
    fn the_processor_temperature_is_the_package_one() {
        let list = [source("nvme", "Composite"), source("k10temp", "Tccd1"), source("k10temp", "Tctl")];
        assert_eq!(cpu_source(&list), Some(2));
        assert_eq!(cpu_source(&[source("coretemp", "Core 0"), source("coretemp", "Package id 0")]), Some(1));
        assert_eq!(cpu_source(&[source("nvme", "Composite")]), None);
    }

    #[test]
    fn the_machine_sensors_read_back() {
        // Whatever this machine has: every reading in its unit, sorted by chip.
        let v = Vitals::new();
        for s in v.sensors.get_untracked().unwrap() {
            assert!(["temperature", "fan", "power"].contains(&s.kind));
            assert!(s.value >= 0.0 && s.value < 100_000.0, "{s:?}");
            if s.kind == "temperature" {
                assert!(s.value < 200.0, "a temperature in °C: {s:?}");
            }
        }
    }
}

impl Default for Vitals {
    fn default() -> Self {
        Vitals::new()
    }
}
