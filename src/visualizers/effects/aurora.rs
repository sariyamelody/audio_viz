/// aurora.rs — Sinusoidal curtains of light driven by frequency band energies.
///
/// Several bands each control a shimmering horizontal wave that fills a
/// vertical slice of the screen.  Bands ripple up and down independently,
/// overlapping and blending like the aurora borealis.
///
/// Config:
///   band_count   — 2–8: number of independent frequency bands / aurora bands
///   wave_speed   — 0.1–3.0: how fast the curtains sway
///   color_scheme — arctic / tropical / fire / neon / spectrum
///   density      — sparse / normal / dense: how many character columns are lit

// ── Index: aurora_color@29 · AuroraViz@42 · new@61 · impl@98 · config@102 · set_config@149 · tick@173 · render@194 · register@297
use std::f32::consts::PI;

use crate::beat::{BeatDetector, BeatDetectorConfig};
use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, TermSize, Visualizer, FFT_SIZE,
};
use crate::visualizer_utils::{
    freq_to_bin, palette_lookup, rms, smooth_asymmetric,
    PALETTE_ARCTIC, PALETTE_TROPICAL, PALETTE_FIRE, PALETTE_NEON,
};

const CONFIG_VERSION: u64 = 1;

fn aurora_color(frac: f32, band_frac: f32, scheme: &str) -> u8 {
    let f = (frac * 0.5 + band_frac * 0.5).clamp(0.0, 1.0);
    match scheme {
        "arctic"   => palette_lookup(f, PALETTE_ARCTIC),
        "tropical" => palette_lookup(f, PALETTE_TROPICAL),
        "fire"     => palette_lookup(f, PALETTE_FIRE),
        "neon"     => palette_lookup(f, PALETTE_NEON),
        _          => specgrad(f),
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct AuroraViz {
    t:          f32,
    /// Smoothed per-band energies (up to 8).
    bands:      [f32; 8],
    beat:       BeatDetector,
    beat_flash: f32,
    /// FFT bin boundaries: [lo, hi) for each band.
    bin_lo:    [usize; 8],
    bin_hi:    [usize; 8],
    source:    String,
    // config
    gain:         f32,
    band_count:   usize,
    wave_speed:   f32,
    color_scheme: String,
    density:      String, // "sparse" | "normal" | "dense"
}

impl AuroraViz {
    pub fn new(source: &str) -> Self {
        let n_bins = FFT_SIZE / 2 + 1;

        // Split 30–12 kHz evenly in log space among 8 potential bands
        let f_lo = 30.0f32;
        let f_hi = 12_000.0f32;
        let mut bin_lo = [0usize; 8];
        let mut bin_hi = [0usize; 8];
        for i in 0..8 {
            let t_lo = i as f32 / 8.0;
            let t_hi = (i + 1) as f32 / 8.0;
            let freq_lo = f_lo * (f_hi / f_lo).powf(t_lo);
            let freq_hi = f_lo * (f_hi / f_lo).powf(t_hi);
            bin_lo[i] = freq_to_bin(freq_lo, n_bins);
            bin_hi[i] = freq_to_bin(freq_hi, n_bins).max(bin_lo[i] + 1);
        }

        Self {
            t:            0.0,
            bands:        [0.0; 8],
            beat:         BeatDetector::new(BeatDetectorConfig::standard()),
            beat_flash:   0.0,
            bin_lo,
            bin_hi,
            source:       source.to_string(),
            gain:         1.0,
            band_count:   4,
            wave_speed:   1.0,
            color_scheme: "arctic".to_string(),
            density:      "normal".to_string(),
        }
    }

}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for AuroraViz {
    fn name(&self)        -> &str { "aurora" }
    fn description(&self) -> &str { "Sinusoidal curtains of light driven by frequency bands" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "aurora",
            "version": CONFIG_VERSION,
            "config": [
                {
                    "name": "gain",
                    "display_name": "Gain",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.0,
                    "max": 4.0
                },
                {
                    "name": "band_count",
                    "display_name": "Band Count",
                    "type": "int",
                    "value": 4,
                    "min": 2,
                    "max": 8
                },
                {
                    "name": "wave_speed",
                    "display_name": "Wave Speed",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.1,
                    "max": 3.0
                },
                {
                    "name": "color_scheme",
                    "display_name": "Color Scheme",
                    "type": "enum",
                    "value": "arctic",
                    "variants": ["arctic", "tropical", "fire", "neon", "spectrum"]
                },
                {
                    "name": "density",
                    "display_name": "Density",
                    "type": "enum",
                    "value": "normal",
                    "variants": ["sparse", "normal", "dense"]
                }
            ]
        }).to_string()
    }

    fn set_config(&mut self, json: &str) -> Result<String, String> {
        let merged = merge_config(&self.get_default_config(), json);
        let val: serde_json::Value = serde_json::from_str(&merged)
            .map_err(|e| format!("JSON parse error: {e}"))?;
        if let Some(config) = val["config"].as_array() {
            for entry in config {
                match entry["name"].as_str().unwrap_or("") {
                    "band_count" => {
                        let v = entry["value"].as_i64()
                            .or_else(|| entry["value"].as_f64().map(|f| f as i64))
                            .unwrap_or(4);
                        self.band_count = (v as usize).clamp(2, 8);
                    }
                    "gain"         => { self.gain         = entry["value"].as_f64().unwrap_or(1.0) as f32; }
                    "wave_speed"   => { self.wave_speed   = entry["value"].as_f64().unwrap_or(1.0) as f32; }
                    "color_scheme" => { if let Some(s) = entry["value"].as_str() { self.color_scheme = s.to_string(); } }
                    "density"      => { if let Some(s) = entry["value"].as_str() { self.density      = s.to_string(); } }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, _size: TermSize) {
        self.t += dt * self.wave_speed;

        let fft = &audio.fft;
        let n   = fft.len();

        for i in 0..self.band_count {
            let lo = self.bin_lo[i];
            let hi = self.bin_hi[i].min(n);
            let raw = if lo < hi { rms(&fft[lo..hi]) } else { 0.0 };
            let scaled = (raw * 6.0 * self.gain).min(1.0); // amplify — band energy is small
            self.bands[i] = smooth_asymmetric(self.bands[i], scaled, 0.35, 0.88);
        }

        self.beat.update(&audio.fft, dt);
        if self.beat.is_beat() {
            self.beat_flash = 1.0;
        }
        self.beat_flash = (self.beat_flash - dt * 3.0).max(0.0);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        // density → column step (1 = every col, 2 = every other, 3 = every third)
        let col_step: usize = match self.density.as_str() {
            "sparse" => 3,
            "dense"  => 1,
            _        => 2,
        };

        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let ry = r as f32 / vis as f32; // 0 (top) .. 1 (bottom)
            let mut line = String::with_capacity(cols * 14);

            for c in 0..cols {
                if col_step > 1 && c % col_step != 0 {
                    // In non-dense mode we skip columns but keep width consistent.
                    // We still need to check for any band coverage here for wider
                    // beams; fall through to check with a 0-intensity placeholder.
                    line.push(' ');
                    continue;
                }

                let cx = c as f32 / cols as f32; // 0..1

                // Accumulate contributions from each band
                let mut max_intensity = 0.0f32;
                let mut max_band = 0;

                for b in 0..self.band_count {
                    let bf = b as f32 / self.band_count.max(1) as f32;

                    // Each band has a slowly drifting horizontal centre column
                    let center_x = 0.5
                        + 0.4 * (self.t * (0.13 + bf * 0.09) + bf * 7.3).sin();
                    // Width of beam expands with energy
                    let beam_w = 0.08 + self.bands[b] * 0.35;

                    let horiz_dist = ((cx - center_x).abs() / beam_w).clamp(0.0, 1.0);
                    let horiz_env  = (1.0 - horiz_dist * horiz_dist).max(0.0); // bell curve

                    // Vertical: aurora hangs from top; lower bands hang lower.
                    // hang_base controls resting position; curtain_h grows strongly with energy.
                    let hang_base = 0.08 + bf * 0.35;
                    let hang      = hang_base + self.bands[b] * 0.12;
                    // Curtain waviness: sinusoidal fringe at the bottom
                    let fringe_y  = hang + 0.18 * (cx * (4.0 + bf * 3.0) * PI + self.t * (0.7 + bf * 0.4)).sin();
                    let fringe_y2 = fringe_y + 0.12 * (cx * (6.0 + bf * 2.0) * PI - self.t * 0.5).sin()
                                            + 0.06 * (cx * (9.0 + bf * 1.5) * PI + self.t * 0.3).cos();
                    // Curtain height: very responsive — goes nearly full screen at high energy
                    let curtain_h = 0.04 + self.bands[b] * 0.55;

                    // Vertical intensity: full at top of curtain, fades at fringe
                    let vert_intensity = if ry < fringe_y2 {
                        let t_top = (fringe_y2 - curtain_h).max(0.0);
                        if ry < t_top { 0.0 }
                        else { ((ry - t_top) / curtain_h.max(0.001)).clamp(0.0, 1.0) * 0.7 }
                    } else {
                        let below = (ry - fringe_y2) / (0.10 + self.bands[b] * 0.15);
                        (1.0 - below.clamp(0.0, 1.0)).powi(2)
                    };

                    let intensity = horiz_env * vert_intensity * (0.3 + self.bands[b] * 0.7);
                    if intensity > max_intensity {
                        max_intensity = intensity;
                        max_band = b;
                    }
                }

                // Beat flash lifts curtain brightness
                max_intensity = (max_intensity + self.beat_flash * 0.2 * max_intensity).min(1.0);

                if max_intensity < 0.05 {
                    line.push(' ');
                    continue;
                }

                let band_frac = max_band as f32 / self.band_count.max(1) as f32;
                let code = aurora_color(max_intensity, band_frac, &self.color_scheme);

                let ch = if max_intensity > 0.80 { '█' }
                         else if max_intensity > 0.55 { '▓' }
                         else if max_intensity > 0.30 { '▒' }
                         else if max_intensity > 0.10 { '░' }
                         else { '·' };

                let bold = if max_intensity > 0.70 { "\x1b[1m" } else { "" };
                line.push_str(&format!("{bold}\x1b[38;5;{code}m{ch}\x1b[0m"));
            }
            lines.push(line);
        }

        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(AuroraViz::new(""))]
}
