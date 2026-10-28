//! The machine's vital signs: processor, memory and graphics card load, each
//! 0 to 1, sampled at one interval (Bioma's `vitals.interval`, 2 s).
//!
//! Values are rounded to the per cent before they are set, so a load that
//! wanders inside one per cent wakes nobody.

use std::{fs, path::PathBuf, time::Duration};

use genoma_core::reactive::{Signal, batch};

pub const INTERVAL: Duration = Duration::from_secs(2);

pub struct Vitals {
    pub cpu: Signal<f64>,
    pub ram: Signal<f64>,
    pub gpu: Signal<Option<f64>>,
    /// Where the card's clock sits between its lowest and highest state, 0…1:
    /// the satellite's rate. A card at a third of its load can sit at full
    /// clock, so the two stay apart (Bioma's PRD §9.2).
    pub gpu_clock: Signal<f64>,
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

impl Vitals {
    pub fn new() -> Vitals {
        let v = Vitals {
            cpu: Signal::new(0.0),
            ram: Signal::new(0.0),
            gpu: Signal::new(None),
            gpu_clock: Signal::new(0.0),
            last_cpu: std::cell::Cell::new(cpu_times()),
            gpu_path: gpu_path(),
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
        batch(|| {
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

impl Default for Vitals {
    fn default() -> Self {
        Vitals::new()
    }
}
