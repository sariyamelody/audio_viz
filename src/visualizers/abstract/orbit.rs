/// orbit.rs — Stereo phase constellation in polar form.
///
/// Each audio sample (L, R) is decomposed into its mid/side components:
///
///   mid  = (L + R) / 2        — summed (mono) content
///   side = |L − R|            — stereo difference (width)
///
/// The sample is then plotted at polar coordinates:
///   angle  = mid  * π         — sweeps −π..π as mono content varies
///   radius = side             — 0 for perfectly mono, 1 for maximum width
///
/// The result reveals the stereo geometry of the mix:
///   • Mono / centred audio → tight cluster near the origin
///   • Wide stereo          → expanding ring or arc
///   • Out-of-phase signal  → points pushed to maximum radius
///
/// A 2D brightness + age grid provides persistence: points fade over time
/// with a slow decay, leaving glowing trails whose length is controlled by
/// the `trail` config value.
///
/// Config:
///   gain   — amplifies L/R before plotting; useful for widening the figure
///   trail  — 0.5 = long trails, 2.0 = short trails
///   hue    — 0–255; shifts the colour palette around the spectrum gradient

// ── Index: OrbitViz@40 · new@62 · ensure_grid@75 · impl@89 · config@93 · set_config@126 · tick@156 · render@224 · register@284
use std::f32::consts::PI;

use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, TermSize, Visualizer,
};
use crate::visualizer_utils::rms;

const CONFIG_VERSION: u64 = 1;

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct OrbitViz {
    // ── Persistence grid ──────────────────────────────────────────────────
    brightness: Vec<Vec<f32>>,  // [vis][cols] — 0.0 (dark) .. 1.0 (bright)
    age:        Vec<Vec<f32>>,  // [vis][cols] — 0.0 (fresh) .. 1.0 (old)

    // ── Audio state ───────────────────────────────────────────────────────
    rms_smooth: f32,

    // ── Size cache ────────────────────────────────────────────────────────
    cached_rows: usize,
    cached_cols: usize,

    // ── Metadata ──────────────────────────────────────────────────────────
    source: String,

    // ── Config fields ─────────────────────────────────────────────────────
    gain:  f32,   // linear multiplier on L/R samples before plotting
    trail: f32,   // 0.5 = long trails, 2.0 = short trails
    hue:   u8,    // 0–255 colour palette shift
}

impl OrbitViz {
    pub fn new(source: &str) -> Self {
        Self {
            brightness:  Vec::new(),
            age:         Vec::new(),
            rms_smooth:  0.0,
            cached_rows: 0,
            cached_cols: 0,
            source:      source.to_string(),
            gain:        1.0,
            trail:       1.0,
            hue:         0,
        }
    }

    fn ensure_grid(&mut self, vis: usize, cols: usize) {
        if self.brightness.len() == vis
            && self.brightness.first().map_or(0, |r| r.len()) == cols
        {
            return;
        }
        self.brightness = vec![vec![0.0f32; cols]; vis];
        self.age        = vec![vec![1.0f32; cols]; vis];
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for OrbitViz {
    fn name(&self)        -> &str { "orbit" }
    fn description(&self) -> &str { "Stereo phase constellation — mid/side polar scatter plot" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "orbit",
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
                    "name": "trail",
                    "display_name": "Trail",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.5,
                    "max": 2.0
                },
                {
                    "name": "hue",
                    "display_name": "Hue",
                    "type": "int",
                    "value": 0,
                    "min": 0,
                    "max": 255
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
                    "gain"  => self.gain  = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "trail" => self.trail = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "hue"   => {
                        let v = entry["value"].as_i64()
                            .or_else(|| entry["value"].as_f64().map(|f| f as i64))
                            .unwrap_or(0);
                        self.hue = v.clamp(0, 255) as u8;
                    }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        let vis  = (size.rows as usize).saturating_sub(1).max(1);
        let cols = size.cols as usize;
        self.ensure_grid(vis, cols);
        self.cached_rows = size.rows as usize;
        self.cached_cols = cols;
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        if rows != self.cached_rows || cols != self.cached_cols {
            self.ensure_grid(vis, cols);
            self.cached_rows = rows;
            self.cached_cols = cols;
        }

        // Smooth RMS for decay modulation
        let rms = rms(&audio.mono);
        self.rms_smooth = 0.7 * self.rms_smooth + 0.3 * rms;

        // Decay the grid: louder audio → slightly faster decay.
        // `trail` > 1.0 raises the base to a power > 1, decaying faster.
        let base_decay = (0.82 - self.rms_smooth * 0.15).clamp(0.68, 0.90);
        let decay = base_decay.powf(self.trail);
        for row in &mut self.brightness { for v in row { *v *= decay; } }
        for row in &mut self.age        { for v in row { *v = (*v + dt * 0.8).min(1.0); } }

        // Map samples to grid.
        // Use 97% of available half-width/height so the constellation fills
        // the display rather than clustering in the centre quarter.
        let cx = (cols - 1) as f32 / 2.0;
        let cy = (vis  - 1) as f32 / 2.0;
        let rx = cx * 0.97;
        let ry = cy * 0.97;

        let n    = audio.left.len().min(audio.right.len());
        let vis_isize  = vis  as isize;
        let cols_isize = cols as isize;

        for i in 0..n {
            let l = audio.left[i]  * self.gain;
            let r = audio.right[i] * self.gain;

            let mid    = (l + r) * 0.5;
            // Use the full stereo difference (not halved) so typical music
            // produces a noticeably wide plot without needing high gain.
            let side   = (l - r).abs().clamp(0.0, 1.0);
            let angle  = mid * PI;
            let radius = side;

            let sx = cx + angle.cos() * radius * rx;
            let sy = cy - angle.sin() * radius * ry;

            let xi = sx.round().clamp(0.0, (cols - 1) as f32) as usize;
            let yi = sy.round().clamp(0.0, (vis  - 1) as f32) as usize;

            self.brightness[yi][xi] = 1.0;
            self.age[yi][xi]        = 0.0;

            const NEIGHBOURS: &[(isize, isize, f32)] = &[
                (-1, 0, 0.55), (1, 0, 0.55), (0, -1, 0.45), (0, 1, 0.45),
            ];
            for &(dr, dc, w) in NEIGHBOURS {
                let ny = (yi as isize + dr).clamp(0, vis_isize  - 1) as usize;
                let nx = (xi as isize + dc).clamp(0, cols_isize - 1) as usize;
                if self.brightness[ny][nx] < w {
                    self.brightness[ny][nx] = w;
                    self.age[ny][nx]        = 0.1;
                }
            }
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let cx = (cols - 1) as f32 / 2.0;
        let cy = (vis  - 1) as f32 / 2.0;

        // Precompute hue shift as a fraction for specgrad offset
        let hue_shift = self.hue as f32 / 255.0;

        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 14);

            let brow = if r < self.brightness.len() { &self.brightness[r] } else { &[] as &[f32] };
            let arow = if r < self.age.len()        { &self.age[r]        } else { &[] as &[f32] };

            for c in 0..cols {
                let b = if c < brow.len() { brow[c] } else { 0.0 };

                if b <= 0.05 {
                    line.push(' ');
                    continue;
                }

                let age_val = if c < arow.len() { arow[c] } else { 1.0 };

                // Color blends two signals:
                //   1. Age: young (0) = warm/bright; old (1) = cool/dim
                //   2. Distance from centre: outer = warmer (higher stereo width)
                let dr         = (r as f32 - cy) / cy.max(1.0);
                let dc         = (c as f32 - cx) / cx.max(1.0);
                let dist_frac  = (dr * dr + dc * dc * 0.25).sqrt().min(1.0);
                let color_frac = ((1.0 - age_val) * 0.6 + dist_frac * 0.4).clamp(0.0, 1.0);
                let code       = specgrad((color_frac + hue_shift).fract());

                let ch = if b > 0.88 { '@' }
                         else if b > 0.65 { '#' }
                         else if b > 0.40 { '*' }
                         else if b > 0.20 { '+' }
                         else { '.' };
                let bold = if b > 0.70 { "\x1b[1m" } else { "" };

                line.push_str(&format!("{bold}\x1b[38;5;{code}m{ch}\x1b[0m"));
            }

            lines.push(line);
        }

        let rms_pct = (self.rms_smooth * 100.0).min(100.0) as u32;
        let extra   = format!(" | rms {:3}%", rms_pct);
        lines.push(status_bar(cols, fps, self.name(), &self.source, &extra));
        pad_frame(lines, rows, cols)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(OrbitViz::new(""))]
}
