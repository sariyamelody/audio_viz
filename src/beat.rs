/// beat.rs — Shared beat detection library for visualizers.
///
/// Provides onset detection via sub-band spectral flux with adaptive
/// thresholding, and optional BPM estimation via autocorrelation.
/// Each visualizer owns its own `BeatDetector` instance — no shared state.
///
/// Zero external dependencies; works identically on native and WASM.

use std::collections::VecDeque;

use crate::visualizer_utils::freq_to_bin;

// ── Band configuration ──────────────────────────────────────────────────────

/// A frequency sub-band used for onset detection.
#[derive(Clone, Debug)]
pub struct BandConfig {
    pub lo_hz: f32,
    pub hi_hz: f32,
    pub weight: f32,
}

// ── Detector configuration ──────────────────────────────────────────────────

/// Configuration for constructing a [`BeatDetector`].
#[derive(Clone, Debug)]
pub struct BeatDetectorConfig {
    /// Frequency bands to monitor for onset energy.
    pub bands: Vec<BandConfig>,
    /// Sensitivity multiplier: >1.0 triggers more beats, <1.0 fewer.
    pub sensitivity: f32,
    /// Minimum seconds between consecutive beats (debounce).
    pub cooldown_secs: f32,
    /// EMA smoothing factor for the adaptive onset threshold (0.0–1.0).
    /// Smaller values = slower adaptation = more stable threshold.
    pub avg_alpha: f32,
    /// Absolute onset floor — no beat fires below this value.
    pub min_onset: f32,
    /// Number of onset-history frames kept for BPM estimation.
    /// At 45 FPS, 512 frames ≈ 11.4 seconds of history.
    pub onset_history_len: usize,
}

impl BeatDetectorConfig {
    /// Full-range single-band detector — closest to the legacy RMS approach.
    pub fn simple() -> Self {
        Self {
            bands: vec![BandConfig { lo_hz: 20.0, hi_hz: 16_000.0, weight: 1.0 }],
            sensitivity: 1.0,
            cooldown_secs: 0.18,
            avg_alpha: 0.08,
            min_onset: 0.005,
            onset_history_len: 512,
        }
    }

    /// Three-band detector (bass / mid / high) — good general-purpose preset.
    pub fn standard() -> Self {
        Self {
            bands: vec![
                BandConfig { lo_hz: 20.0,    hi_hz: 250.0,    weight: 1.0 },
                BandConfig { lo_hz: 250.0,   hi_hz: 4_000.0,  weight: 0.6 },
                BandConfig { lo_hz: 4_000.0, hi_hz: 14_000.0, weight: 0.3 },
            ],
            sensitivity: 1.0,
            cooldown_secs: 0.16,
            avg_alpha: 0.10,
            min_onset: 0.004,
            onset_history_len: 512,
        }
    }

    /// Bass-only detector — tuned for kick drum / bass transients.
    pub fn bass_only() -> Self {
        Self {
            bands: vec![BandConfig { lo_hz: 20.0, hi_hz: 200.0, weight: 1.0 }],
            sensitivity: 1.0,
            cooldown_secs: 0.14,
            avg_alpha: 0.09,
            min_onset: 0.003,
            onset_history_len: 512,
        }
    }
}

// ── Beat detector ───────────────────────────────────────────────────────────

pub struct BeatDetector {
    // Config (mutable for runtime sensitivity / cooldown changes)
    bands: Vec<BandConfig>,
    sensitivity: f32,
    cooldown_secs: f32,
    avg_alpha: f32,
    min_onset: f32,

    // Precomputed FFT bin ranges per band
    band_bins: Vec<(usize, usize)>,

    // Per-band state
    prev_band_energy: Vec<f32>,
    band_onsets: Vec<f32>,

    // Adaptive threshold
    onset_avg: f32,

    // Beat state
    time_since_beat: f32,
    beat_active: bool,
    beat_intensity: f32,

    // BPM estimation
    onset_history: VecDeque<f32>,
    onset_history_len: usize,
    bpm_timer: f32,
    estimated_bpm: f32,
}

impl BeatDetector {
    /// Create a new detector from the given configuration.
    ///
    /// Call [`update`] once per frame in your visualizer's `tick()`, then query
    /// [`is_beat`], [`beat_intensity`], etc.
    pub fn new(config: BeatDetectorConfig) -> Self {
        let n_bands = config.bands.len();
        Self {
            bands: config.bands,
            sensitivity: config.sensitivity,
            cooldown_secs: config.cooldown_secs,
            avg_alpha: config.avg_alpha,
            min_onset: config.min_onset,

            band_bins: Vec::new(), // computed lazily on first update
            prev_band_energy: vec![0.0; n_bands],
            band_onsets: vec![0.0; n_bands],

            onset_avg: 0.0,
            time_since_beat: 1.0, // start "ready" so first beat can fire
            beat_active: false,
            beat_intensity: 0.0,

            onset_history: VecDeque::with_capacity(config.onset_history_len),
            onset_history_len: config.onset_history_len,
            bpm_timer: 0.0,
            estimated_bpm: 0.0,
        }
    }

    /// Process one frame of FFT data.  Call once per frame in `tick()`.
    pub fn update(&mut self, fft: &[f32], dt: f32) {
        let n_bins = fft.len();
        if n_bins == 0 {
            self.beat_active = false;
            self.time_since_beat += dt;
            return;
        }

        // Lazily compute bin ranges on first call (or if FFT size changed)
        if self.band_bins.len() != self.bands.len() {
            self.band_bins = self.bands.iter().map(|b| {
                let lo = freq_to_bin(b.lo_hz, n_bins);
                let hi = freq_to_bin(b.hi_hz, n_bins).max(lo + 1);
                (lo, hi.min(n_bins))
            }).collect();
        }

        // ── Sub-band spectral flux ──────────────────────────────────────
        let mut onset = 0.0f32;
        for (i, (band, &(lo, hi))) in self.bands.iter().zip(self.band_bins.iter()).enumerate() {
            let energy = band_rms(&fft[lo..hi]);
            // Half-wave rectified flux: only positive changes (transients)
            let flux = (energy - self.prev_band_energy[i]).max(0.0);
            self.band_onsets[i] = flux;
            onset += flux * band.weight;
            self.prev_band_energy[i] = energy;
        }

        // ── Adaptive threshold ──────────────────────────────────────────
        let alpha = self.avg_alpha;
        self.onset_avg = alpha * self.onset_avg + (1.0 - alpha) * onset;

        let threshold = self.onset_avg * (1.5 / self.sensitivity.max(0.01));

        // ── Beat decision ───────────────────────────────────────────────
        self.time_since_beat += dt;
        self.beat_active = onset > threshold
            && onset > self.min_onset
            && self.time_since_beat > self.cooldown_secs;

        if self.beat_active {
            self.beat_intensity = if threshold > 1e-9 {
                ((onset - threshold) / threshold).min(2.0)
            } else {
                1.0
            };
            self.time_since_beat = 0.0;
        } else {
            self.beat_intensity = 0.0;
        }

        // ── BPM history ─────────────────────────────────────────────────
        self.onset_history.push_back(onset);
        if self.onset_history.len() > self.onset_history_len {
            self.onset_history.pop_front();
        }
        self.bpm_timer += dt;
        if self.bpm_timer >= 2.0 && self.onset_history.len() >= self.onset_history_len / 2 {
            self.estimated_bpm = estimate_bpm(&self.onset_history, dt);
            self.bpm_timer = 0.0;
        }
    }

    // ── Queries ─────────────────────────────────────────────────────────────

    /// `true` on the frame a beat onset was detected.
    #[inline]
    pub fn is_beat(&self) -> bool {
        self.beat_active
    }

    /// Strength of the current beat relative to the adaptive threshold.
    /// 0.0 when no beat; typically 0.0–1.0, can exceed 1.0 for strong beats.
    #[inline]
    pub fn beat_intensity(&self) -> f32 {
        self.beat_intensity
    }

    /// Seconds elapsed since the last detected beat.
    #[inline]
    pub fn time_since_beat(&self) -> f32 {
        self.time_since_beat
    }

    /// Per-band onset (spectral flux) values for the current frame.
    #[inline]
    pub fn band_onsets(&self) -> &[f32] {
        &self.band_onsets
    }

    /// Estimated tempo in BPM from autocorrelation of onset history.
    /// Returns 0.0 if insufficient data or no clear periodicity detected.
    #[inline]
    pub fn estimated_bpm(&self) -> f32 {
        self.estimated_bpm
    }

    // ── Runtime tuning ──────────────────────────────────────────────────────

    /// Override sensitivity at runtime (e.g. from a config slider).
    /// >1.0 = more beats, <1.0 = fewer.
    #[inline]
    pub fn set_sensitivity(&mut self, s: f32) {
        self.sensitivity = s;
    }

    /// Override the minimum cooldown between beats (seconds).
    #[inline]
    pub fn set_cooldown(&mut self, secs: f32) {
        self.cooldown_secs = secs;
    }
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// RMS of an FFT magnitude slice (inlined, no dependency on visualizer_utils).
#[inline]
fn band_rms(slice: &[f32]) -> f32 {
    if slice.is_empty() {
        return 0.0;
    }
    let sum = slice.iter().fold(0.0f32, |acc, &v| acc + v * v);
    (sum / slice.len() as f32).sqrt()
}

/// Estimate BPM via autocorrelation of the onset signal.
///
/// Searches lag range corresponding to 60–200 BPM. Returns 0.0 if no clear
/// periodicity is found.
fn estimate_bpm(history: &VecDeque<f32>, dt: f32) -> f32 {
    let n = history.len();
    if n < 64 || dt <= 0.0 {
        return 0.0;
    }

    let fps = 1.0 / dt;
    // Lag range: 60 BPM → fps frames/beat, 200 BPM → fps*60/200 frames/beat
    let lag_min = (fps * 60.0 / 200.0).round() as usize; // ~13 at 45 fps
    let lag_max = (fps * 60.0 / 60.0).round() as usize;  // ~45 at 45 fps
    let lag_max = lag_max.min(n / 2);

    if lag_min >= lag_max {
        return 0.0;
    }

    // Compute mean for zero-centering
    let mean = history.iter().sum::<f32>() / n as f32;

    let mut best_lag = 0usize;
    let mut best_corr = f32::NEG_INFINITY;

    for lag in lag_min..=lag_max {
        let mut corr = 0.0f32;
        let samples = n - lag;
        for i in 0..samples {
            corr += (history[i] - mean) * (history[i + lag] - mean);
        }
        corr /= samples as f32;

        // Weight toward ~120 BPM (perceptual center of musical tempo range)
        let bpm_at_lag = fps * 60.0 / lag as f32;
        let center_weight = 1.0 - ((bpm_at_lag - 120.0) / 120.0).abs() * 0.15;
        corr *= center_weight;

        if corr > best_corr {
            best_corr = corr;
            best_lag = lag;
        }
    }

    if best_lag == 0 || best_corr <= 0.0 {
        return 0.0;
    }

    let bpm = fps * 60.0 / best_lag as f32;
    bpm
}
