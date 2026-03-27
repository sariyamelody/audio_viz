/// radial.rs — Polar spectrum: frequency bands radiate from the centre.
///
/// Each cell is computed in polar coordinates.  Its angular sector maps
/// to a frequency bar; the cell is lit if the normalised radius is less than
/// that bar's smoothed energy.  Characters densify toward the core:
///   @  #  *  +  .  `   (inner → outer)
///
/// A faint crosshair (+, -, |) is visible through silence.
///
/// Performance: the polar grid (rnorm, theta arrays) is recomputed only on
/// resize; it is cached in the struct between frames.

// ── Index: RadialViz@25 · new@41 · precompute@55 · impl@89 · config@93 · set_config@110 · tick@129 · render@146 · register@206
use std::f32::consts::PI;
use crate::beat::{BeatDetector, BeatDetectorConfig};
use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, SpectrumBars, TermSize, Visualizer,
};
use crate::visualizer_utils::with_gained_fft;

const CONFIG_VERSION: u64 = 1;

pub struct RadialViz {
    bars:       SpectrumBars,
    beat:       BeatDetector,
    beat_flash: f32,
    source:     String,
    /// Cached normalised radius for each cell.  Shape: rows × cols.
    rnorm:  Vec<Vec<f32>>,
    /// Cached angle (−π … π) for each cell.
    theta:  Vec<Vec<f32>>,
    cached_rows: usize,
    cached_cols: usize,
    // ── Config fields ──────────────────────────────────────────────────────
    gain: f32,
}

impl RadialViz {
    pub fn new(source: &str) -> Self {
        Self {
            bars:        SpectrumBars::new(80),
            beat:        BeatDetector::new(BeatDetectorConfig::standard()),
            beat_flash:  0.0,
            source:      source.to_string(),
            rnorm:       Vec::new(),
            theta:       Vec::new(),
            cached_rows: 0,
            cached_cols: 0,
            gain:        2.0,
        }
    }

    fn precompute(&mut self, rows: usize, cols: usize) {
        let cy = rows as f32 / 2.0;
        let cx = cols as f32 / 2.0;
        let maxr = (cy).min(cx * 0.5).max(1.0);

        self.rnorm = (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| {
                        let dy = r as f32 - cy;
                        let dx = (c as f32 - cx) * 0.5;
                        (dx * dx + dy * dy).sqrt() / maxr
                    })
                    .collect()
            })
            .collect();

        self.theta = (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| {
                        let dy = r as f32 - cy;
                        let dx = (c as f32 - cx) * 0.5;
                        dy.atan2(dx)
                    })
                    .collect()
            })
            .collect();

        self.cached_rows = rows;
        self.cached_cols = cols;
    }
}

impl Visualizer for RadialViz {
    fn name(&self)        -> &str { "radial" }
    fn description(&self) -> &str { "Polar spectrum radiating from the centre" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "radial",
            "version": CONFIG_VERSION,
            "config": [
                {
                    "name": "gain",
                    "display_name": "Gain",
                    "type": "float",
                    "value": 2.0,
                    "min": 0.0,
                    "max": 4.0
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
                if entry["name"].as_str() == Some("gain") {
                    self.gain = entry["value"].as_f64().unwrap_or(1.0) as f32;
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        self.bars.resize(size.cols as usize);
        self.precompute(size.rows as usize, size.cols as usize);
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let rows = size.rows as usize;
        let cols = size.cols as usize;

        if rows != self.cached_rows || cols != self.cached_cols {
            self.precompute(rows, cols);
        }
        self.bars.resize(cols);
        with_gained_fft(&audio.fft, self.gain, |fft| self.bars.update(fft, dt));

        self.beat.update(&audio.fft, dt);
        if self.beat.is_beat() {
            self.beat_flash = 1.0;
        }
        self.beat_flash = (self.beat_flash - dt * 4.0).max(0.0);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1);
        let cy2  = vis  / 2;
        let cx2  = cols / 2;
        let n    = self.bars.smoothed.len().max(1);

        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 12);

            let row_rnorm = if r < self.rnorm.len() { &self.rnorm[r] } else { &[] as &[f32] };
            let row_theta = if r < self.theta.len() { &self.theta[r] } else { &[] as &[f32] };

            for c in 0..cols {
                let rn = if c < row_rnorm.len() { row_rnorm[c] } else { 999.0 };
                let th = if c < row_theta.len() { row_theta[c] } else { 0.0 };

                let bi    = ((th + PI) / (2.0 * PI) * n as f32) as usize % n;
                let bar_h = self.bars.smoothed[bi] + self.beat_flash * 0.15;

                if rn < bar_h && rn < 1.0 {
                    let frac = bi as f32 / (n - 1).max(1) as f32;
                    let code = specgrad(frac);
                    let ch   = if       rn < 0.12 { '@' }
                               else if  rn < 0.28 { '#' }
                               else if  rn < 0.48 { '*' }
                               else if  rn < 0.68 { '+' }
                               else if  rn < 0.84 { '.' }
                               else               { '`' };
                    let bold = if rn < 0.35 { "\x1b[1m" } else { "" };
                    line.push_str(&format!("{bold}\x1b[38;5;{code}m{ch}\x1b[0m"));
                } else {
                    let on_x = r == cy2;
                    let on_y = c == cx2;
                    if on_x && on_y {
                        line.push_str("\x1b[2m\x1b[38;5;237m+\x1b[0m");
                    } else if on_x || on_y {
                        let dist = r.abs_diff(cy2) + c.abs_diff(cx2);
                        if dist > 2 {
                            let ch = if on_y { '|' } else { '-' };
                            line.push_str(&format!("\x1b[2m\x1b[38;5;234m{ch}\x1b[0m"));
                        } else {
                            line.push(' ');
                        }
                    } else {
                        line.push(' ');
                    }
                }
            }
            lines.push(line);
        }

        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(RadialViz::new(""))]
}
