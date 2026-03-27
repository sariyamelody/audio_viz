/// tunnel.rs — Fly-through perspective tunnel driven by audio.
///
/// Each terminal cell is mapped to polar tunnel coordinates (u = angle,
/// v = depth) via perspective projection from the screen centre.  The tunnel
/// walls are a layered composition of scrolling depth rings and angular ribs
/// that are modulated by bass/mid/high band energies.  A perspective fog
/// darkens the centre (distance) and brightens the outer rim (proximity).
///
/// Config:
///   gain        — 0–4: scales audio reactivity
///   color_scheme — spectrum / fire / neon / ice / gold
///   shape        — circle / square / hex  (cross-section geometry)
///   speed        — 0.1–3.0: forward velocity through the tunnel
///   turbulence   — 0–1: how much audio warps the wall texture

// ── Index: tunnel_color@31 · shape_dist@43 · TunnelViz@58 · new@77 · impl@98 · config@102 · set_config@149 · tick@168 · render@183 · register@309
use std::f32::consts::PI;

use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, TermSize, Visualizer, FFT_SIZE,
};
use crate::visualizer_utils::{
    freq_to_bin, palette_lookup, rms, smooth_asymmetric,
    PALETTE_FIRE, PALETTE_NEON, PALETTE_ICE, PALETTE_GOLD,
};

const CONFIG_VERSION: u64 = 1;

fn tunnel_color(frac: f32, scheme: &str) -> u8 {
    match scheme {
        "fire" => palette_lookup(frac, PALETTE_FIRE),
        "neon" => palette_lookup(frac, PALETTE_NEON),
        "ice"  => palette_lookup(frac, PALETTE_ICE),
        "gold" => palette_lookup(frac, PALETTE_GOLD),
        _      => specgrad(frac),
    }
}

// ── Shape distance (normalised so the "unit circle" of each shape ≈ 1) ────────

fn shape_dist(dx: f32, dy: f32, shape: &str) -> f32 {
    match shape {
        "square" => dx.abs().max(dy.abs()),
        "hex"    => {
            // Regular hexagon: Chebyshev in rotated frame
            let ax = dx.abs();
            let ay = dy.abs();
            (ax * 0.866_025 + ay * 0.5).max(ay) // ≈ hexagonal metric
        }
        _ => (dx * dx + dy * dy).sqrt(),  // circle
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct TunnelViz {
    t:       f32,
    bass:    f32,
    mid:     f32,
    high:    f32,
    bass_lo: usize,
    bass_hi: usize,
    mid_hi:  usize,
    high_hi: usize,
    source:  String,
    // config
    gain:         f32,
    color_scheme: String,
    shape:        String,
    speed:        f32,
    turbulence:   f32,
}

impl TunnelViz {
    pub fn new(source: &str) -> Self {
        let n_bins = FFT_SIZE / 2 + 1;
        let bass_lo = freq_to_bin(20.0, n_bins);
        let bass_hi = freq_to_bin(250.0, n_bins).max(bass_lo + 1);
        let mid_hi  = freq_to_bin(4_000.0, n_bins).max(bass_hi + 1);
        let high_hi = freq_to_bin(12_000.0, n_bins).max(mid_hi + 1);
        Self {
            t: 0.0, bass: 0.0, mid: 0.0, high: 0.0,
            bass_lo, bass_hi, mid_hi, high_hi,
            source:       source.to_string(),
            gain:         1.0,
            color_scheme: "spectrum".to_string(),
            shape:        "circle".to_string(),
            speed:        1.0,
            turbulence:   0.3,
        }
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for TunnelViz {
    fn name(&self)        -> &str { "tunnel" }
    fn description(&self) -> &str { "Perspective fly-through tunnel with audio-reactive walls" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "tunnel",
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
                    "name": "color_scheme",
                    "display_name": "Color Scheme",
                    "type": "enum",
                    "value": "spectrum",
                    "variants": ["spectrum", "fire", "neon", "ice", "gold"]
                },
                {
                    "name": "shape",
                    "display_name": "Shape",
                    "type": "enum",
                    "value": "circle",
                    "variants": ["circle", "square", "hex"]
                },
                {
                    "name": "speed",
                    "display_name": "Speed",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.1,
                    "max": 3.0
                },
                {
                    "name": "turbulence",
                    "display_name": "Turbulence",
                    "type": "float",
                    "value": 0.3,
                    "min": 0.0,
                    "max": 1.0
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
                    "gain"         => { self.gain         = entry["value"].as_f64().unwrap_or(1.0) as f32; }
                    "color_scheme" => { if let Some(s) = entry["value"].as_str() { self.color_scheme = s.to_string(); } }
                    "shape"        => { if let Some(s) = entry["value"].as_str() { self.shape        = s.to_string(); } }
                    "speed"        => { self.speed        = entry["value"].as_f64().unwrap_or(1.0) as f32; }
                    "turbulence"   => { self.turbulence   = entry["value"].as_f64().unwrap_or(0.3) as f32; }
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
        let raw_mid  = if self.mid_hi  < n { rms(&fft[self.bass_hi..self.mid_hi ]) } else { 0.0 };
        let raw_high = if self.high_hi < n { rms(&fft[self.mid_hi ..self.high_hi]) } else { 0.0 };

        let g = self.gain;
        self.bass = smooth_asymmetric(self.bass, (raw_bass * g).min(1.0), 0.30, 0.88);
        self.mid  = smooth_asymmetric(self.mid,  (raw_mid  * g).min(1.0), 0.35, 0.90);
        self.high = smooth_asymmetric(self.high, (raw_high * g).min(1.0), 0.25, 0.92);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let cx   = cols as f32 * 0.5;
        let cy   = vis  as f32 * 0.5;
        // Account for terminal character aspect (~2:1 height:width)
        // maxr is the half-diagonal in screen-space units
        let maxr_x = cx;
        let maxr_y = cy * 0.5;
        let maxr   = maxr_x.min(maxr_y).max(1.0);

        let t    = self.t;
        let bass = self.bass;
        let mid  = self.mid;
        let high = self.high;
        let turb = self.turbulence;

        // How many depth-ring cycles fit across the visible radius
        const RING_FREQ: f32 = 6.0;
        // Angular rib count (increases with mid energy)
        let rib_count = 8.0 + mid * 8.0;

        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 16);
            let dy = (r as f32 - cy) * 2.0; // multiply by 2 to correct for char aspect

            for c in 0..cols {
                let dx = c as f32 - cx;

                // Shape-aware distance from centre
                let dist = shape_dist(dx, dy, &self.shape);

                if dist < 1.5 {
                    line.push(' ');
                    continue;
                }

                // Angle in [-π, π]
                let angle = dy.atan2(dx);

                // ── Perspective depth coordinate ──────────────────────────
                // dist=0 → depth=∞ (deep in tunnel)
                // dist=maxr → depth=1 (right in front of viewer)
                // We clamp to avoid infinite depth at center.
                let depth = (maxr / dist.max(0.5)).clamp(1.0, 30.0);

                // Scrolling depth coordinate (creates forward-fly effect)
                let v = (depth * 0.25 + t).fract();

                // Angular coordinate [0, 1)
                let u_raw = (angle / (2.0 * PI) + 0.5).fract();

                // ── Turbulence warping ────────────────────────────────────
                // Audio warps the UV coordinates — walls ripple and pulse
                let warp_u = turb * bass * 0.08 * (depth * 2.5 * PI + t * 1.3).sin();
                let warp_v = turb * mid  * 0.06 * (angle * 3.0 + t * 0.9).cos();
                let u = (u_raw + warp_u).fract();
                let vw = (v    + warp_v).fract();

                // ── Wall brightness layers ────────────────────────────────

                // 1. Depth rings: scrolling bands — the primary tunnel feel
                let ring = {
                    let phase = vw * RING_FREQ * PI;
                    // Make rings pulse with bass: sharper bright bands on beat
                    let sharpness = 2.0 + bass * 4.0;
                    (phase.sin().abs()).powf(sharpness)
                };

                // 2. Angular ribs: lines running down the length of the tunnel
                let rib = {
                    let rib_phase = u * rib_count * PI;
                    (rib_phase.sin().abs()).powf(3.0 + mid * 3.0) * 0.6 + 0.4
                };

                // 3. High-frequency sparkle at ring intersections
                let sparkle = if high > 0.1 {
                    let sp = (vw * RING_FREQ * 2.0 * PI).sin() * (u * rib_count * 2.0 * PI).sin();
                    (sp.abs() * high * 2.0).min(1.0)
                } else { 0.0 };

                // 4. Bass pulse: momentary brightness surge across all walls
                let pulse = bass * 0.4 * (t * 2.0).sin().abs();

                let raw_brightness = ring * rib + sparkle * 0.3 + pulse;

                // ── Perspective fog ───────────────────────────────────────
                // Closer to viewer (larger dist from centre) = brighter
                // Deep in tunnel (smaller dist from centre) = darker
                let fog = (dist / (maxr * 1.1)).clamp(0.0, 1.0).powf(0.6);
                let brightness = (raw_brightness * fog).clamp(0.0, 1.0);

                if brightness < 0.04 {
                    line.push(' ');
                    continue;
                }

                // ── Color ─────────────────────────────────────────────────
                // Hue cycles with angle + depth + time for a vivid look
                let color_frac = (u * 0.6 + v * 0.2 + t * 0.04).fract();
                let code = tunnel_color(color_frac, &self.color_scheme);

                let ch = if brightness > 0.85 { '█' }
                         else if brightness > 0.65 { '▓' }
                         else if brightness > 0.40 { '▒' }
                         else if brightness > 0.18 { '░' }
                         else { '·' };

                let bold = if brightness > 0.72 { "\x1b[1m" } else { "" };
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
    vec![Box::new(TunnelViz::new(""))]
}
