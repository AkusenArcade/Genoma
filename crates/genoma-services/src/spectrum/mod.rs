//! The sound as bands (`@genoma/spectrum`): what leaves the machine, or the
//! microphone, in logarithmic bands per channel, for a visualiser or a
//! shader's values.
//!
//! The capture and the transform run in a process of their own,
//! `genoma spectrum`, as Bioma's `tools/sinestesia-bands` did: one capture
//! whoever reads it, and none while nobody does. The service starts it when
//! the first holder asks (`hold`) and stops it when the last lets go; it
//! reads one line a frame, two hexadecimal digits a band, the left
//! channel's bands then the right's. Held for the stereo image as well
//! (`--imaging`), each band's direction, diffuseness and energy follow.

pub mod capture;
pub mod dsp;

use std::{
    cell::Cell,
    io::Write,
    time::{Duration, Instant},
};

use genoma_core::reactive::{Signal, batch};

/// `genoma spectrum`'s options.
pub struct Options {
    pub bands: usize,
    pub fps: u32,
    pub gain: f32,
    pub input: bool,
    /// The stereo image too: two transforms more a frame.
    pub imaging: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { bands: BANDS, fps: 60, gain: 1.0, input: false, imaging: false }
    }
}

/// Bands per channel the service asks for.
pub const BANDS: usize = 64;

/// `genoma spectrum`: captures and writes a line of bands a frame on stdout
/// until it closes.
pub fn run(options: Options) {
    let ring = capture::Ring::new(dsp::FFT_SIZE * 2);
    let source = if options.input { capture::Source::Input } else { capture::Source::Output };
    let _capture = capture::start(ring.clone(), source);
    let mut left = dsp::Analyzer::new(options.bands, capture::SAMPLE_RATE, options.gain);
    let mut right = dsp::Analyzer::new(options.bands, capture::SAMPLE_RATE, options.gain);
    let mut image = options.imaging.then(|| dsp::ImagingAnalyzer::new(options.bands, capture::SAMPLE_RATE, options.gain));
    let mut samples = vec![0.0f32; dsp::FFT_SIZE];
    let mut other = vec![0.0f32; dsp::FFT_SIZE];
    let mut line = String::with_capacity(options.bands * 10 + 1);
    let frame = Duration::from_secs_f64(1.0 / options.fps.max(1) as f64);
    let mut next = Instant::now();
    let stdout = std::io::stdout();
    loop {
        line.clear();
        ring.snapshot(capture::Channel::Left, &mut samples);
        write_bands(&mut line, left.analyze(&samples));
        ring.snapshot(capture::Channel::Right, &mut other);
        write_bands(&mut line, right.analyze(&other));
        if let Some(image) = &mut image {
            let i = image.analyze(&samples, &other);
            // The azimuth from the left speaker (0) to the right (1).
            let placed: Vec<f32> = i.azimuth.iter().map(|a| (a / dsp::STAGE_HALF_ANGLE + 1.0) * 0.5).collect();
            write_bands(&mut line, &placed);
            write_bands(&mut line, &i.diffuseness);
            write_bands(&mut line, &i.energy);
        }
        line.push('\n');
        // A closed pipe is how the reader says it has stopped.
        let mut out = stdout.lock();
        if out.write_all(line.as_bytes()).is_err() || out.flush().is_err() {
            return;
        }
        drop(out);
        next += frame;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

fn write_bands(line: &mut String, bands: &[f32]) {
    for value in bands {
        let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        line.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        line.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
}

/// The bands as the shell reads them.
pub struct Spectrum {
    /// Each channel's bands, low to high, 0…1; empty while not held.
    pub left: Signal<Vec<f32>>,
    pub right: Signal<Vec<f32>>,
    /// The loudest band of either: how much is sounding.
    pub level: Signal<f64>,
    /// Whether the capture runs: somebody holds it.
    pub wanted: Signal<bool>,
    holders: Cell<u32>,
    /// What the capture listens to: the microphone rather than the output.
    pub input: Cell<bool>,
    /// The stereo image, each band's: its direction in radians (0 ahead,
    /// ±dsp::STAGE_HALF_ANGLE the right or left speaker), its diffuseness
    /// and its energy, 0…1. Empty unless a holder asked for the image.
    pub azimuth: Signal<Vec<f32>>,
    pub diffuseness: Signal<Vec<f32>>,
    pub energy: Signal<Vec<f32>>,
    /// Whether a holder wants the image: the capture runs with it then.
    pub imaging: Signal<bool>,
    imaging_holders: Cell<u32>,
    /// Whether the running capture gives the image: how its lines are cut.
    pub imaging_running: Cell<bool>,
}

impl Default for Spectrum {
    fn default() -> Self {
        Spectrum {
            left: Signal::new(Vec::new()),
            right: Signal::new(Vec::new()),
            level: Signal::new(0.0),
            wanted: Signal::new(false),
            holders: Cell::new(0),
            input: Cell::new(false),
            azimuth: Signal::new(Vec::new()),
            diffuseness: Signal::new(Vec::new()),
            energy: Signal::new(Vec::new()),
            imaging: Signal::new(false),
            imaging_holders: Cell::new(0),
            imaging_running: Cell::new(false),
        }
    }
}

impl Spectrum {
    /// One more reader: the capture starts with the first. One that wants
    /// the stereo image (`imaging`) has the capture give it too.
    pub fn hold(&self, imaging: bool) {
        self.holders.set(self.holders.get() + 1);
        if imaging {
            self.imaging_holders.set(self.imaging_holders.get() + 1);
            if self.imaging.get_untracked() != Some(true) {
                self.imaging.set(true);
            }
        }
        if self.wanted.get_untracked() != Some(true) {
            self.wanted.set(true);
        }
    }

    /// One reader fewer: the capture stops with the last, and the bands
    /// empty; the image goes with the last reader that wanted it.
    pub fn release(&self, imaging: bool) {
        if imaging {
            let left = self.imaging_holders.get().saturating_sub(1);
            self.imaging_holders.set(left);
            if left == 0 && self.imaging.get_untracked() != Some(false) {
                batch(|| {
                    self.imaging.set(false);
                    self.azimuth.set(Vec::new());
                    self.diffuseness.set(Vec::new());
                    self.energy.set(Vec::new());
                });
            }
        }
        let left = self.holders.get().saturating_sub(1);
        self.holders.set(left);
        if left == 0 && self.wanted.get_untracked() != Some(false) {
            batch(|| {
                self.wanted.set(false);
                self.left.set(Vec::new());
                self.right.set(Vec::new());
                self.level.set(0.0);
            });
        }
    }

    /// One line of `genoma spectrum`.
    pub fn line(&self, line: &str) {
        let bytes = line.trim().as_bytes();
        if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
            return;
        }
        let value = |pair: &[u8]| -> f32 {
            let hex = |b: u8| (b as char).to_digit(16).unwrap_or(0) as f32;
            (hex(pair[0]) * 16.0 + hex(pair[1])) / 255.0
        };
        let all: Vec<f32> = bytes.chunks_exact(2).map(value).collect();
        let blocks = if self.imaging_running.get() { 5 } else { 2 };
        if !all.len().is_multiple_of(blocks) {
            return;
        }
        let n = all.len() / blocks;
        let block = |i: usize| all[i * n..(i + 1) * n].to_vec();
        let level = all[..2 * n].iter().copied().fold(0.0f32, f32::max) as f64;
        batch(|| {
            self.left.set(block(0));
            self.right.set(block(1));
            self.level.set(level);
            if blocks == 5 && self.imaging.get_untracked() == Some(true) {
                self.azimuth.set(block(2).iter().map(|v| (v * 2.0 - 1.0) * dsp::STAGE_HALF_ANGLE).collect());
                self.diffuseness.set(block(3));
                self.energy.set(block(4));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_read_as_both_channels() {
        let s = Spectrum::default();
        s.hold(false);
        // Two bands a channel: 00 ff, then 80 00.
        s.line("00ff8000");
        assert_eq!(s.left.get_untracked().unwrap(), vec![0.0, 1.0]);
        let right = s.right.get_untracked().unwrap();
        assert!((right[0] - 128.0 / 255.0).abs() < 1e-6 && right[1] == 0.0);
        assert_eq!(s.level.get_untracked(), Some(1.0));
        s.release(false);
        assert!(s.left.get_untracked().unwrap().is_empty() && s.wanted.get_untracked() == Some(false));
    }

    #[test]
    fn a_line_with_the_image_gives_it_back_in_radians() {
        let s = Spectrum::default();
        s.hold(true);
        s.imaging_running.set(true);
        // One band: left, right, then azimuth at the right speaker,
        // diffuseness none, energy full.
        s.line("8080ff00ff");
        let az = s.azimuth.get_untracked().unwrap();
        assert!((az[0] - dsp::STAGE_HALF_ANGLE).abs() < 1e-6, "{az:?}");
        assert_eq!(s.diffuseness.get_untracked().unwrap(), vec![0.0]);
        assert_eq!(s.energy.get_untracked().unwrap(), vec![1.0]);
        s.release(true);
        assert!(s.azimuth.get_untracked().unwrap().is_empty() && s.imaging.get_untracked() == Some(false));
    }

    #[test]
    fn bands_are_written_two_hex_digits_each() {
        let mut line = String::new();
        write_bands(&mut line, &[0.0, 1.0, 0.5]);
        assert_eq!(line, "00ff80");
    }
}
