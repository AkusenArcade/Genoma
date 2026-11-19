//! The band contract, taken from Sinestesia's `src/dsp.rs` by way of
//! Bioma's `tools/sinestesia-bands`.
//!
//! Every figure here is that program's, not a new choice: a Hann window over
//! 2048 samples, logarithmic bins from 30 Hz to 16 kHz, the peak bin inside
//! each band, a dB mapping where −70 dB is nothing and 0 dB is full, and an
//! asymmetric one-pole smoothing that rises quickly and falls slowly: worked
//! out against real audio there. Each channel is analysed on its own.

use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

pub const FFT_SIZE: usize = 2048;

const FREQ_MIN: f32 = 30.0;
const FREQ_MAX: f32 = 16_000.0;

/// Rising and falling coefficients of the temporal smoothing. Asymmetric on
/// purpose: a transient has to arrive at once and leave slowly, or the band
/// reads as noise rather than as sound.
const ATTACK: f32 = 0.45;
const DECAY: f32 = 0.18;

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    fft_buf: Vec<Complex<f32>>,
    band_edges: Vec<usize>,
    smoothed: Vec<f32>,
    gain: f32,
}

impl Analyzer {
    pub fn new(bands: usize, sample_rate: u32, gain: f32) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|n| {
                let x = std::f32::consts::PI * 2.0 * n as f32 / (FFT_SIZE as f32 - 1.0);
                0.5 - 0.5 * x.cos()
            })
            .collect();

        Self {
            fft,
            window,
            fft_buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            band_edges: band_edges(bands, sample_rate),
            smoothed: vec![0.0; bands],
            gain,
        }
    }

    /// One frame: the last `FFT_SIZE` samples in, the smoothed bands out.
    pub fn analyze(&mut self, samples: &[f32]) -> &[f32] {
        for ((slot, sample), w) in self.fft_buf.iter_mut().zip(samples).zip(&self.window) {
            *slot = Complex::new(sample * w, 0.0);
        }
        self.fft.process(&mut self.fft_buf);

        for b in 0..self.smoothed.len() {
            let lo = self.band_edges[b];
            let hi = self.band_edges[b + 1].max(lo + 1);

            // The peak bin, not the mean: a mean spreads a sharp partial across
            // a whole band and the band stops following the music.
            let mut peak = 0.0f32;
            for bin in lo..hi {
                let m = self.fft_buf[bin].norm();
                if m > peak {
                    peak = m;
                }
            }

            let mag = peak / (FFT_SIZE as f32 * 0.5);
            let db = 20.0 * (mag + 1e-9).log10();
            let target = (((db + 70.0) / 70.0).clamp(0.0, 1.0) * self.gain).clamp(0.0, 1.0);

            let coeff = if target > self.smoothed[b] { ATTACK } else { DECAY };
            self.smoothed[b] += (target - self.smoothed[b]) * coeff;
        }

        &self.smoothed
    }
}

// ---- The stereo image ---------------------------------------------------------
//
// Sinestesia's `ImagingAnalyzer` (src/dsp.rs), figure for figure: where in
// the stereo stage each band's sound comes from, and how much of it has no
// direction at all. It keeps the phase the band analysis throws away: the
// same level on both channels is a centred source when they are in phase
// and a wide one in antiphase, and only the cross-spectrum L · conj(R)
// tells the two apart.

/// The interaural time difference that puts a source fully at one side:
/// about what a head gives for a source at 90°. Beyond it the image stays
/// at the edge.
const ITD_FULL: f32 = 0.0006;
/// The duplex theory's crossover: below, the time difference carries the
/// direction; above, the level (the head's shadow).
const DUPLEX_LO_HZ: f32 = 700.0;
const DUPLEX_HI_HZ: f32 = 1600.0;
/// Half the stereo stage: the two speakers of the standard triangle stand
/// at ±30° from the listening direction (ITU-R BS.775).
pub const STAGE_HALF_ANGLE: f32 = std::f32::consts::PI / 6.0;

/// The weight of the time difference: whole at low frequencies, none at
/// high ones, where the phase between the ears is ambiguous.
fn duplex_itd_weight(freq: f32) -> f32 {
    ((DUPLEX_HI_HZ - freq) / (DUPLEX_HI_HZ - DUPLEX_LO_HZ)).clamp(0.0, 1.0)
}

/// Each band's place in the stage, smoothed.
pub struct Imaging {
    /// Radians: 0 straight ahead, ±STAGE_HALF_ANGLE the right (+) or left
    /// (−) speaker. The front arc only, and within the stage: from two
    /// channels front and back cannot be told apart, and a panned source
    /// does not exist beyond the speakers.
    pub azimuth: Vec<f32>,
    /// 0 a localised source, 1 a decorrelated field (ambience, reverb):
    /// the energy that has no direction, not "the sound behind".
    pub diffuseness: Vec<f32>,
    /// The band's energy, 0…1, mapped as the bands are.
    pub energy: Vec<f32>,
}

pub struct ImagingAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    left: Vec<Complex<f32>>,
    right: Vec<Complex<f32>>,
    band_edges: Vec<usize>,
    band_freqs: Vec<f32>,
    smoothed: Imaging,
    gain: f32,
}

impl ImagingAnalyzer {
    pub fn new(bands: usize, sample_rate: u32, gain: f32) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|n| {
                let x = std::f32::consts::PI * 2.0 * n as f32 / (FFT_SIZE as f32 - 1.0);
                0.5 - 0.5 * x.cos()
            })
            .collect();
        let band_edges = band_edges(bands, sample_rate);
        let bin_hz = (sample_rate as f32 / 2.0) / (FFT_SIZE as f32 / 2.0);
        let band_freqs = (0..bands)
            .map(|b| {
                let lo = band_edges[b];
                let hi = band_edges[b + 1].max(lo + 1);
                (lo + hi) as f32 * 0.5 * bin_hz
            })
            .collect();
        Self {
            fft,
            window,
            left: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            right: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            band_edges,
            band_freqs,
            smoothed: Imaging { azimuth: vec![0.0; bands], diffuseness: vec![0.0; bands], energy: vec![0.0; bands] },
            gain,
        }
    }

    /// One frame: the last `FFT_SIZE` samples of each channel in, each
    /// band's direction, diffuseness and energy out.
    pub fn analyze(&mut self, left: &[f32], right: &[f32]) -> &Imaging {
        for ((slot, sample), w) in self.left.iter_mut().zip(left).zip(&self.window) {
            *slot = Complex::new(sample * w, 0.0);
        }
        for ((slot, sample), w) in self.right.iter_mut().zip(right).zip(&self.window) {
            *slot = Complex::new(sample * w, 0.0);
        }
        self.fft.process(&mut self.left);
        self.fft.process(&mut self.right);

        for b in 0..self.band_freqs.len() {
            let lo = self.band_edges[b];
            let hi = self.band_edges[b + 1].max(lo + 1);

            // Auto-spectra and the complex cross-spectrum over the band: its
            // modulus says how much the channels are one signal, its phase
            // how far one lags the other.
            let (mut sll, mut srr) = (0.0f32, 0.0f32);
            let mut slr = Complex::new(0.0f32, 0.0f32);
            let mut peak = 0.0f32;
            for k in lo..hi {
                let (l, r) = (self.left[k], self.right[k]);
                let (pl, pr) = (l.norm_sqr(), r.norm_sqr());
                sll += pl;
                srr += pr;
                slr += l * r.conj();
                peak = peak.max(((pl + pr) * 0.5).sqrt());
            }

            // Coherence in modulus: 1 one signal (delayed or not), 0
            // uncorrelated. The real part is cos(ωτ) and swings with the
            // frequency: on binaural material it flipped band to band.
            let coh = (slr.norm() / (sll * srr).sqrt().max(1e-12)).clamp(0.0, 1.0);

            // The level difference, normalised: −1 all left, +1 all right,
            // exactly a pan-pot's gain ratio.
            let (al, ar) = (sll.sqrt(), srr.sqrt());
            let pan_ild = (ar - al) / (ar + al + 1e-12);

            // The time difference: the cross-spectrum's phase is ωτ; a
            // positive phase is R late, a source on the left.
            let freq = self.band_freqs[b].max(1.0);
            let tau = slr.arg() / (std::f32::consts::TAU * freq);
            let pan_itd = -(tau / ITD_FULL).clamp(-1.0, 1.0) * duplex_itd_weight(freq) * coh;

            // Time-intensity trading: a pan-pot gives only level, spaced
            // microphones only time, the techniques between both.
            let pan = (pan_itd + pan_ild).clamp(-1.0, 1.0);

            let mag = peak / (FFT_SIZE as f32 * 0.5);
            let db = 20.0 * (mag + 1e-9).log10();
            let energy = (((db + 70.0) / 70.0).clamp(0.0, 1.0) * self.gain).clamp(0.0, 1.0);
            let coeff = if energy > self.smoothed.energy[b] { ATTACK } else { DECAY };
            self.smoothed.energy[b] += (energy - self.smoothed.energy[b]) * coeff;

            // A faint band's direction is the noise's: it moves slowly, so
            // the image does not flicker in a quiet passage.
            let k = 0.10 + 0.30 * self.smoothed.energy[b];

            // The tangent law: tan θ / tan θ₀ = pan. A pan halfway is not
            // heard at 15°.
            let azimuth = (pan * STAGE_HALF_ANGLE.tan()).atan();
            self.smoothed.azimuth[b] += (azimuth - self.smoothed.azimuth[b]) * k;

            // Coherence measures diffuseness only while both channels carry
            // sound: a source panned hard right has no left to correlate,
            // and is the most localised case there is.
            let diffuse = (1.0 - coh) * (1.0 - pan.abs());
            self.smoothed.diffuseness[b] += (diffuse - self.smoothed.diffuseness[b]) * k;
        }

        &self.smoothed
    }
}

/// The FFT bin each band starts at, on a logarithmic scale. Music is heard
/// logarithmically, and a linear split gives forty bands of cymbal and two of
/// everything a bass line does.
fn band_edges(bands: usize, sample_rate: u32) -> Vec<usize> {
    let nyquist = sample_rate as f32 / 2.0;
    let bin_hz = nyquist / (FFT_SIZE as f32 / 2.0);
    let log_min = FREQ_MIN.log10();
    let log_max = FREQ_MAX.log10();

    (0..=bands)
        .map(|i| {
            let t = i as f32 / bands as f32;
            let freq = 10f32.powf(log_min + t * (log_max - log_min));
            ((freq / bin_hz).round() as usize).min(FFT_SIZE / 2 - 1)
        })
        .collect()
}

/// The centre frequency of a band, for anything that has to say what it is
/// looking at.
pub fn band_center_hz(band: usize, bands: usize) -> f32 {
    let t = (band as f32 + 0.5) / bands as f32;
    FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imaging_of(left: impl Fn(usize) -> f32, right: impl Fn(usize) -> f32, at: f32) -> (f32, f32, f32) {
        const RATE: u32 = 48_000;
        const BANDS: usize = 64;
        let mut a = ImagingAnalyzer::new(BANDS, RATE, 1.0);
        let l: Vec<f32> = (0..FFT_SIZE).map(&left).collect();
        let r: Vec<f32> = (0..FFT_SIZE).map(&right).collect();
        for _ in 0..80 {
            a.analyze(&l, &r);
        }
        let image = a.analyze(&l, &r);
        let band = (0..BANDS)
            .min_by(|&x, &y| (band_center_hz(x, BANDS) - at).abs().partial_cmp(&(band_center_hz(y, BANDS) - at).abs()).unwrap())
            .unwrap();
        (image.azimuth[band], image.diffuseness[band], image.energy[band])
    }

    /// A tone panned hard right is heard at the right speaker; the same on
    /// both channels, straight ahead and localised; independent noise on
    /// each, a field without a direction.
    #[test]
    fn the_image_puts_each_sound_where_it_is() {
        let tone = |n: usize| (std::f32::consts::TAU * 2000.0 * n as f32 / 48_000.0).sin() * 0.5;
        // At 2 kHz, where the level carries the direction.
        let (az, diff, energy) = imaging_of(|_| 0.0, tone, 2000.0);
        assert!((az - STAGE_HALF_ANGLE).abs() < 0.02, "hard right: {az}");
        assert!(diff < 0.05 && energy > 0.5, "localised and loud: {diff} {energy}");
        let (az, diff, _) = imaging_of(tone, tone, 2000.0);
        assert!(az.abs() < 0.02 && diff < 0.05, "centred: {az} {diff}");
        // Two different pseudo-random sequences.
        let noise = |seed: u32| move |n: usize| {
            let mut x = (n as u32).wrapping_mul(2_654_435_761).wrapping_add(seed);
            x ^= x >> 15;
            x = x.wrapping_mul(2_246_822_519);
            x ^= x >> 13;
            (x as f32 / u32::MAX as f32 - 0.5) * 0.5
        };
        // Coherence is measured over a band's bins in one frame, so a narrow
        // band reads independent noise as half coherent; a wide one, high
        // up, has the bins to tell.
        let (_, diff, _) = imaging_of(noise(1), noise(99_991), 12_000.0);
        assert!(diff > 0.6, "uncorrelated: {diff}");
    }

    /// A tone lands in the band that holds it and nowhere else. This is the
    /// test that says the binning and the dB mapping are right without anyone
    /// having to listen to anything.
    #[test]
    fn a_tone_lights_its_own_band() {
        const RATE: u32 = 48_000;
        const BANDS: usize = 64;
        let mut analyzer = Analyzer::new(BANDS, RATE, 1.0);

        let tone: Vec<f32> = (0..FFT_SIZE)
            .map(|n| {
                (std::f32::consts::TAU * 1000.0 * n as f32 / RATE as f32).sin() * 0.5
            })
            .collect();

        // Several frames, because the smoothing rises over a few of them.
        let mut last = vec![0.0; BANDS];
        for _ in 0..40 {
            last = analyzer.analyze(&tone).to_vec();
        }

        let expected = (0..BANDS)
            .min_by(|&a, &b| {
                let da = (band_center_hz(a, BANDS) - 1000.0).abs();
                let db = (band_center_hz(b, BANDS) - 1000.0).abs();
                da.partial_cmp(&db).unwrap()
            })
            .unwrap();

        let loudest = (0..BANDS)
            .max_by(|&a, &b| last[a].partial_cmp(&last[b]).unwrap())
            .unwrap();

        assert!(
            (loudest as i32 - expected as i32).abs() <= 1,
            "1 kHz landed in band {loudest}, expected about {expected}"
        );
        assert!(last[loudest] > 0.5, "a half-scale tone should be loud, got {}", last[loudest]);

        // And silence goes back down to nothing.
        let quiet = vec![0.0; FFT_SIZE];
        for _ in 0..200 {
            analyzer.analyze(&quiet);
        }
        let resting = analyzer.analyze(&quiet).to_vec();
        assert!(
            resting.iter().all(|v| *v < 0.01),
            "silence should empty the band, got {:?}",
            resting.iter().cloned().fold(0.0f32, f32::max)
        );
    }
}
