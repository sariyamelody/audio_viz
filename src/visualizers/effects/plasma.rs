/// plasma.rs — Full-screen interference plasma driven by audio band energies.
///
/// Each cell's colour is computed from the sum of four sine waves whose
/// arguments mix the cell position, a time accumulator, and three smoothed
/// frequency-band energy values (bass / mid / high).
///
/// Config:
///   gain         — scales how strongly audio band energies modulate the waves
///   speed        — multiplies dt when advancing the time accumulator
///   warp         — stretches the spatial frequency of every wave; higher = more
///                  complex, tightly-packed interference patterns
///   turbulence   — controls how chaotically the waves interact; low = smooth
///                  rolling fields, high = rapid swirling distortion
///   color_scheme — one of five colour palettes (see below)
///
/// Color schemes:
///   spectrum — full rainbow cycling through the shared specgrad palette
///   fire     — ember reds through orange into bright yellow-white
///   ocean    — deep navy through cyan to pale aqua
///   neon     — electric pink → purple → cyan → white
///   sunset   — violet dusk through coral into golden amber

// ── Index: scheme_color@38 · PlasmaViz@51 · new@76 · impl@105 · config@109 · set_config@157 · tick@180 · render@200 · register@277
use std::f32::consts::PI;

use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, TermSize, Visualizer, FFT_SIZE,
};
use crate::visualizer_utils::{
    freq_to_bin, palette_lookup, rms, smooth_asymmetric,
    PALETTE_FIRE, PALETTE_OCEAN, PALETTE_NEON, PALETTE_SUNSET,
};

const CONFIG_VERSION: u64 = 1;

fn scheme_color(frac: f32, shift: f32, scheme: &str) -> u8 {
    let s = (frac + shift).fract();
    match scheme {
        "fire"   => palette_lookup(s, PALETTE_FIRE),
        "ocean"  => palette_lookup(s, PALETTE_OCEAN),
        "neon"   => palette_lookup(s, PALETTE_NEON),
        "sunset" => palette_lookup(s, PALETTE_SUNSET),
        _        => specgrad(s),
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct PlasmaViz {
    // ── Audio-reactive state ───────────────────────────────────────────────
    t:       f32,   // global time accumulator; advances by dt * speed
    bass:    f32,   // smoothed ~20–250 Hz RMS energy
    mid:     f32,   // smoothed ~250–4 kHz RMS energy
    high:    f32,   // smoothed ~4–12 kHz RMS energy

    // ── FFT bin boundaries (precomputed from SAMPLE_RATE / FFT_SIZE) ───────
    bass_lo: usize,
    bass_hi: usize,
    mid_hi:  usize,
    high_hi: usize,

    // ── Metadata ──────────────────────────────────────────────────────────
    source: String,

    // ── Config fields ─────────────────────────────────────────────────────
    gain:         f32,   // scales band energy modulation depth
    speed:        f32,   // 0.1–5.0, time advance multiplier
    warp:         f32,   // 0.25–4.0, spatial frequency of waves
    turbulence:   f32,   // 0.0–1.0, cross-wave chaos
    color_scheme: String,
}

impl PlasmaViz {
    pub fn new(source: &str) -> Self {
        let n_bins = FFT_SIZE / 2 + 1;

        let bass_lo = freq_to_bin(20.0, n_bins);
        let bass_hi = freq_to_bin(250.0, n_bins).max(bass_lo + 1);
        let mid_hi  = freq_to_bin(4_000.0, n_bins).max(bass_hi + 1);
        let high_hi = freq_to_bin(12_000.0, n_bins).max(mid_hi + 1);

        Self {
            t:            0.0,
            bass:         0.0,
            mid:          0.0,
            high:         0.0,
            bass_lo,
            bass_hi,
            mid_hi,
            high_hi,
            source:       source.to_string(),
            gain:         1.0,
            speed:        1.0,
            warp:         1.0,
            turbulence:   0.3,
            color_scheme: "spectrum".to_string(),
        }
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for PlasmaViz {
    fn name(&self)        -> &str { "plasma" }
    fn description(&self) -> &str { "Interference plasma — audio-reactive sine wave colour field" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "plasma",
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
                    "name": "speed",
                    "display_name": "Speed",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.1,
                    "max": 5.0
                },
                {
                    "name": "warp",
                    "display_name": "Warp",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.25,
                    "max": 4.0
                },
                {
                    "name": "turbulence",
                    "display_name": "Turbulence",
                    "type": "float",
                    "value": 0.3,
                    "min": 0.0,
                    "max": 1.0
                },
                {
                    "name": "color_scheme",
                    "display_name": "Color Scheme",
                    "type": "enum",
                    "value": "spectrum",
                    "variants": ["spectrum", "fire", "ocean", "neon", "sunset"]
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
                    "gain"         => self.gain       = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "speed"        => self.speed      = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "warp"         => self.warp       = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "turbulence"   => self.turbulence = entry["value"].as_f64().unwrap_or(0.3) as f32,
                    "color_scheme" => {
                        if let Some(s) = entry["value"].as_str() {
                            self.color_scheme = s.to_string();
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, _size: TermSize) {
        self.t += dt * self.speed;

        let fft = &audio.fft;
        let n   = fft.len();

        let raw_bass = if self.bass_hi < n { rms(&fft[self.bass_lo..self.bass_hi]) } else { 0.0 };
        let raw_mid  = if self.mid_hi  < n { rms(&fft[self.bass_hi..self.mid_hi])  } else { 0.0 };
        let raw_high = if self.high_hi < n { rms(&fft[self.mid_hi..self.high_hi])  } else { 0.0 };

        // Scale by gain, then smooth with fast-attack / slow-release
        let scaled_bass = (raw_bass  * self.gain).min(1.0);
        let scaled_mid  = (raw_mid   * self.gain).min(1.0);
        let scaled_high = (raw_high  * self.gain).min(1.0);

        self.bass = smooth_asymmetric(self.bass, scaled_bass, 0.40, 0.92);
        self.mid  = smooth_asymmetric(self.mid,  scaled_mid,  0.40, 0.92);
        self.high = smooth_asymmetric(self.high, scaled_high, 0.35, 0.92);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let t    = self.t;
        let bass = self.bass;
        let mid  = self.mid;
        let high = self.high;
        let w    = self.warp;
        let turb = self.turbulence;

        // Palette shift so colours cycle even in silence
        let palette_shift = (t * 0.08).fract();

        // Drifting radial-wave centre (adds movement without audio)
        let drift_x = 0.5 + 0.15 * (t * 0.23).sin();
        let drift_y = 0.5 + 0.08 * (t * 0.31).cos();

        let mut lines: Vec<String> = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 14);
            let ry = r as f32 / vis.max(1) as f32;   // 0..1

            for c in 0..cols {
                let cx = c as f32 / cols.max(1) as f32; // 0..1

                // Wave 1: horizontal roll, bass-modulated, warp-scaled
                let v1 = (cx * w * (2.0 + bass * 6.0) * PI + t * 0.7).sin();

                // Wave 2: diagonal, mid-modulated, warp-scaled
                // turbulence bends the diagonal by mixing in a cross-product term
                let diag = cx + ry + turb * (cx - ry) * (bass * 0.5 + 0.5);
                let v2   = (diag * w * (3.0 + mid * 5.0) * PI + t * 1.1).sin();

                // Wave 3: radial from drifting centre, high-modulated (fine ripple)
                // turbulence adds a phase twist driven by the cross-product of v1/v2
                let dx      = cx - drift_x;
                let dy      = (ry - drift_y) * 0.5;
                let dist    = (dx * dx + dy * dy).sqrt();
                let phase3  = turb * v1 * v2 * PI;  // chaos: v1*v2 interference
                let v3      = (dist * w * (8.0 + high * 20.0) * PI - t * 1.8 + phase3).sin();

                // Wave 4: vertical sweep, bass-driven, turbulence mixes in v1
                let sweep = ry + turb * v1 * 0.15;
                let v4    = (sweep * w * (4.0 + bass * 4.0) * PI + t * 0.5 + cx * PI).sin();

                let plasma = (v1 + v2 + v3 + v4) / 4.0; // −1..1
                let frac   = plasma * 0.5 + 0.5;          // 0..1

                let ch = if frac < 0.15 { ' ' }
                         else if frac < 0.38 { '░' }
                         else if frac < 0.58 { '▒' }
                         else if frac < 0.80 { '▓' }
                         else { '█' };

                if ch == ' ' {
                    line.push(' ');
                    continue;
                }

                let code = scheme_color(frac, palette_shift, &self.color_scheme);
                let bold = if frac > 0.75 { "\x1b[1m" } else { "" };
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
    vec![Box::new(PlasmaViz::new(""))]
}
