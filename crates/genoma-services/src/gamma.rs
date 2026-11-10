//! The displays' colour ramps: `wlr-gamma-control`, for a night light of
//! the shell's own. A setting is a colour temperature, a brightness and a
//! gamma, for every display or for one; the runtime turns each display's
//! setting into the table the compositor wants and hands it over.
//!
//! One program at a time holds a display's ramp: with wlsunset or
//! gammastep running, that display reports `failed`. A neutral setting
//! lets go of the display, and the compositor puts its own ramp back.

use std::collections::BTreeMap;

use genoma_core::reactive::Signal;

/// The temperature that changes nothing: daylight, as monitors are set.
pub const NEUTRAL_KELVIN: f64 = 6500.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Setting {
    /// Kelvin, 1000 to 10000; lower is warmer.
    pub temperature: f64,
    /// 0 to 1, a multiplier on every channel.
    pub brightness: f64,
    /// The curve's exponent; 1 is the display's own.
    pub gamma: f64,
}

impl Default for Setting {
    fn default() -> Self {
        Setting { temperature: NEUTRAL_KELVIN, brightness: 1.0, gamma: 1.0 }
    }
}

impl Setting {
    pub fn is_neutral(&self) -> bool {
        (self.temperature - NEUTRAL_KELVIN).abs() < 1.0 && (self.brightness - 1.0).abs() < 1e-3 && (self.gamma - 1.0).abs() < 1e-3
    }

    /// Kept to what a display can show: a brightness of nothing would be a
    /// black screen nobody can see to undo.
    pub fn clamped(self) -> Setting {
        Setting {
            temperature: self.temperature.clamp(1000.0, 10000.0),
            brightness: self.brightness.clamp(0.1, 1.0),
            gamma: self.gamma.clamp(0.1, 10.0),
        }
    }
}

/// The colour of a black body at `kelvin`, as red, green and blue from 0
/// to 1 (Tanner Helland's fit), scaled so that [`NEUTRAL_KELVIN`] is white.
pub fn white_point(kelvin: f64) -> [f64; 3] {
    fn raw(kelvin: f64) -> [f64; 3] {
        let t = kelvin / 100.0;
        let r = if t <= 66.0 { 255.0 } else { 329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2) };
        let g = if t <= 66.0 { 99.470_802_586_1 * t.ln() - 161.119_568_166_1 } else { 288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2) };
        let b = if t >= 66.0 {
            255.0
        } else if t <= 19.0 {
            0.0
        } else {
            138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
        };
        [r, g, b].map(|c| (c / 255.0).clamp(0.0, 1.0))
    }
    let (w, n) = (raw(kelvin), raw(NEUTRAL_KELVIN));
    [0, 1, 2].map(|i| (w[i] / n[i]).clamp(0.0, 1.0))
}

/// The table `zwlr_gamma_control_v1.set_gamma` takes for a display of
/// `size` entries per channel: red, then green, then blue, each `size`
/// native-endian u16.
pub fn ramp(setting: Setting, size: usize) -> Vec<u16> {
    let s = setting.clamped();
    let white = white_point(s.temperature);
    let mut out = Vec::with_capacity(size * 3);
    for channel in white {
        for i in 0..size {
            let x = if size > 1 { i as f64 / (size - 1) as f64 } else { 1.0 };
            let v = x.powf(1.0 / s.gamma) * channel * s.brightness;
            out.push((v.clamp(0.0, 1.0) * u16::MAX as f64).round() as u16);
        }
    }
    out
}

/// How a display's ramp stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Held {
    /// Ours, with this setting.
    Set(Setting),
    /// Another program holds it, or the compositor cannot change it.
    Failed,
}

pub struct Gamma {
    /// Whether the compositor offers the protocol at all.
    pub available: Signal<bool>,
    /// What the interface asks: by display name, `"*"` for every display
    /// without one of its own.
    pub settings: Signal<BTreeMap<String, Setting>>,
    /// What each display has now, by name; a display at its own ramp is
    /// not listed.
    pub held: Signal<BTreeMap<String, Held>>,
}

impl Default for Gamma {
    fn default() -> Self {
        Gamma { available: Signal::new(false), settings: Signal::new(BTreeMap::new()), held: Signal::new(BTreeMap::new()) }
    }
}

impl Gamma {
    /// `setting` on `output`, or on every display without its own.
    pub fn set(&self, output: Option<&str>, setting: Setting) {
        let mut all = self.settings.get_untracked().unwrap_or_default();
        all.insert(output.unwrap_or("*").to_owned(), setting.clamped());
        self.settings.set(all);
    }

    /// `output` back to what every display has, or every display back to
    /// its own ramp.
    pub fn reset(&self, output: Option<&str>) {
        let mut all = self.settings.get_untracked().unwrap_or_default();
        match output {
            Some(name) => {
                all.remove(name);
            }
            None => all.clear(),
        }
        self.settings.set(all);
    }

    /// What `output` should show: its own setting, every display's, or
    /// nothing to change.
    pub fn wanted(&self, output: &str) -> Setting {
        let all = self.settings.get_untracked().unwrap_or_default();
        all.get(output).or_else(|| all.get("*")).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daylight_is_white_and_lower_is_warmer() {
        assert_eq!(white_point(NEUTRAL_KELVIN), [1.0, 1.0, 1.0]);
        let [r, g, b] = white_point(3400.0);
        assert!(r == 1.0 && g < 1.0 && b < g, "{r} {g} {b}");
        let [_, _, b2] = white_point(2000.0);
        assert!(b2 < b);
    }

    #[test]
    fn a_neutral_ramp_is_the_identity() {
        let table = ramp(Setting::default(), 256);
        assert_eq!(table.len(), 768);
        assert_eq!((table[0], table[255], table[256 + 128]), (0, u16::MAX, (128.0 / 255.0 * 65535.0f64).round() as u16));
    }

    #[test]
    fn brightness_scales_and_never_goes_black() {
        let dim = ramp(Setting { brightness: 0.5, ..Setting::default() }, 4);
        assert_eq!(dim[3], (0.5 * 65535.0f64).round() as u16);
        let floor = ramp(Setting { brightness: 0.0, ..Setting::default() }, 4);
        assert_eq!(floor[3], (0.1 * 65535.0f64).round() as u16, "clamped to a tenth");
    }

    #[test]
    fn a_display_takes_its_own_setting_or_everyones() {
        let g = Gamma::default();
        let warm = Setting { temperature: 4000.0, ..Setting::default() };
        g.set(None, warm);
        g.set(Some("DP-1"), Setting { brightness: 0.8, ..Setting::default() });
        assert_eq!(g.wanted("HDMI-A-1"), warm);
        assert_eq!(g.wanted("DP-1").brightness, 0.8);
        g.reset(Some("DP-1"));
        assert_eq!(g.wanted("DP-1"), warm);
        g.reset(None);
        assert!(g.wanted("DP-1").is_neutral());
    }
}
