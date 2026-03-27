/// crystal.rs — Kaleidoscope symmetry mirror driven by the audio spectrum.
///
/// The base quadrant (top-right) is computed as a Lissajous-style X/Y scatter
/// of spectrum energy.  It is then reflected into N symmetric sectors to create
/// a kaleidoscope effect.  The whole mandala slowly rotates; the `arm_style`
/// config switches between line, mirror and filled rendering modes.
///
/// Config:
///   symmetry       — 3–12: number of mirror sectors
///   rotation_speed — 0–2: angular velocity of the mandala (rad/s)
///   color_scheme   — spectrum / fire / ice / neon / gold
///   arm_style      — line / mirror / filled

// ── Index: crystal_color@29 · CrystalViz@41 · new@54 · impl@70 · config@74 · set_config@121 · tick@149 · render@155 · register@254
use std::f32::consts::PI;

use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, SpectrumBars, TermSize, Visualizer,
};
use crate::visualizer_utils::{
    palette_lookup, with_gained_fft,
    PALETTE_FIRE, PALETTE_ICE, PALETTE_NEON, PALETTE_GOLD,
};

const CONFIG_VERSION: u64 = 1;

fn crystal_color(frac: f32, scheme: &str) -> u8 {
    match scheme {
        "fire" => palette_lookup(frac, PALETTE_FIRE),
        "ice"  => palette_lookup(frac, PALETTE_ICE),
        "neon" => palette_lookup(frac, PALETTE_NEON),
        "gold" => palette_lookup(frac, PALETTE_GOLD),
        _      => specgrad(frac),
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct CrystalViz {
    bars:    SpectrumBars,
    t:       f32,
    source:  String,
    // config
    gain:           f32,
    symmetry:       usize,
    rotation_speed: f32,
    color_scheme:   String,
    arm_style:      String, // "line" | "mirror" | "filled"
}

impl CrystalViz {
    pub fn new(source: &str) -> Self {
        Self {
            bars:           SpectrumBars::new(120),
            t:              0.0,
            source:         source.to_string(),
            gain:           2.5,
            symmetry:       6,
            rotation_speed: 0.2,
            color_scheme:   "spectrum".to_string(),
            arm_style:      "mirror".to_string(),
        }
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for CrystalViz {
    fn name(&self)        -> &str { "crystal" }
    fn description(&self) -> &str { "Kaleidoscope symmetry mandala driven by spectrum energy" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "crystal",
            "version": CONFIG_VERSION,
            "config": [
                {
                    "name": "gain",
                    "display_name": "Gain",
                    "type": "float",
                    "value": 2.5,
                    "min": 0.0,
                    "max": 4.0
                },
                {
                    "name": "symmetry",
                    "display_name": "Symmetry",
                    "type": "int",
                    "value": 6,
                    "min": 3,
                    "max": 12
                },
                {
                    "name": "rotation_speed",
                    "display_name": "Rotation Speed",
                    "type": "float",
                    "value": 0.2,
                    "min": 0.0,
                    "max": 2.0
                },
                {
                    "name": "color_scheme",
                    "display_name": "Color Scheme",
                    "type": "enum",
                    "value": "spectrum",
                    "variants": ["spectrum", "fire", "ice", "neon", "gold"]
                },
                {
                    "name": "arm_style",
                    "display_name": "Arm Style",
                    "type": "enum",
                    "value": "mirror",
                    "variants": ["line", "mirror", "filled"]
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
                    "symmetry" => {
                        let v = entry["value"].as_i64()
                            .or_else(|| entry["value"].as_f64().map(|f| f as i64))
                            .unwrap_or(6);
                        self.symmetry = (v as usize).clamp(3, 12);
                    }
                    "gain"           => { self.gain           = entry["value"].as_f64().unwrap_or(2.0) as f32; }
                    "rotation_speed" => { self.rotation_speed = entry["value"].as_f64().unwrap_or(0.2) as f32; }
                    "color_scheme"   => { if let Some(s) = entry["value"].as_str() { self.color_scheme = s.to_string(); } }
                    "arm_style"      => { if let Some(s) = entry["value"].as_str() { self.arm_style    = s.to_string(); } }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        self.bars.resize(size.cols as usize);
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        self.t += dt * self.rotation_speed;
        self.bars.resize(size.cols as usize);
        with_gained_fft(&audio.fft, self.gain, |fft| self.bars.update(fft, dt));
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let cx = cols as f32 / 2.0;
        let cy = vis  as f32 / 2.0;
        // dy is doubled in the distance formula (aspect correction); the
        // inscribed circle in that corrected space has radius min(cx, cy*2).
        let maxr = cx.min(cy * 2.0).max(1.0);

        let sym  = self.symmetry as f32;
        let n    = self.bars.smoothed.len().max(1);

        // ── Pre-build a brightness grid ────────────────────────────────────────
        // For "filled" mode we paint the full interior; for "line" / "mirror"
        // we only paint near the spectrum outline.

        let mut brightness: Vec<Vec<f32>> = vec![vec![0.0f32; cols]; vis];
        let mut hue_grid:   Vec<Vec<f32>> = vec![vec![0.0f32; cols]; vis];

        let sector_angle = 2.0 * PI / sym;

        // Iterate over pixels
        for r in 0..vis {
            for c in 0..cols {
                let dx = c as f32 - cx;
                let dy = (r as f32 - cy) * 2.0; // undo char aspect
                let dist = (dx * dx + dy * dy).sqrt();
                let r_norm = dist / maxr;
                if r_norm > 1.02 { continue; }

                // Map angle to canonical sector [0, sector_angle)
                let theta_raw = dy.atan2(dx) + self.t; // include rotation
                // Fold into the fundamental domain [0, sector_angle)
                let mut theta = theta_raw.rem_euclid(sector_angle);
                // Mirror mode: fold again within the sector for bilateral symmetry
                if self.arm_style != "line" && theta > sector_angle * 0.5 {
                    theta = sector_angle - theta;
                }

                // Map canonical angle to a spectrum bar index
                let bi   = ((theta / sector_angle * n as f32) as usize).min(n - 1);
                let bar_h = self.bars.smoothed[bi];
                let freq_frac = bi as f32 / (n - 1).max(1) as f32;

                let lit = match self.arm_style.as_str() {
                    "filled" => r_norm < bar_h,
                    _ => {
                        // Line / mirror: draw near the boundary of the bar
                        let rim = (r_norm - bar_h).abs();
                        rim < 0.055
                    }
                };

                if lit {
                    let intensity = if self.arm_style == "filled" {
                        // brighter near the outer edge
                        (r_norm / bar_h.max(0.01)).clamp(0.5, 1.0)
                    } else {
                        1.0 - (r_norm - bar_h).abs() / 0.055
                    };
                    brightness[r][c] = intensity.clamp(0.0, 1.0);
                    hue_grid[r][c]   = freq_frac;
                }
            }
        }

        // ── Render ────────────────────────────────────────────────────────────
        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 14);
            for c in 0..cols {
                let b = brightness[r][c];
                if b < 0.05 {
                    line.push(' ');
                    continue;
                }
                let frac = hue_grid[r][c];
                let code = crystal_color(frac, &self.color_scheme);
                let ch   = if b > 0.85 { '█' }
                           else if b > 0.60 { '▓' }
                           else if b > 0.35 { '▒' }
                           else if b > 0.15 { '░' }
                           else { '·' };
                let bold = if b > 0.70 { "\x1b[1m" } else { "" };
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
    vec![Box::new(CrystalViz::new(""))]
}
