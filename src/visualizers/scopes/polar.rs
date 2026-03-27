/// polar.rs — Polar waveform oscilloscope
///
/// The mono waveform is bent into a circle.  Time maps to angle (0 → 2π) and
/// amplitude modulates the radius outward from a base ring.  A silent signal
/// draws a perfect circle; loud signals push the perimeter outward in
/// rhythmic pulses.
///
/// A dim reference ring is drawn at the zero-amplitude radius so the
/// deformation is always visible even at low gain.
///
/// ═══════════════════════════════════════════════════════════════════════════
///  CONFIG
/// ═══════════════════════════════════════════════════════════════════════════
///
///  gain        — amplitude multiplier before the radius modulation is applied.
///  base_radius — radius of the zero-amplitude reference ring as a fraction of
///                the maximum usable radius (0.2 = small inner ring,
///                0.9 = near the edge).  Default 0.55.
///  theme       — phosphor color palette: "green" (classic P31), "amber"
///                (P3), or "white" (P4 — sharp fade through grey).

// ── Index: PolarViz@33 · new@42 · draw_line@68 · impl@100 · config@104 · set_config@137 · tick@155 · render@159 · register@266
use std::f32::consts::PI;

use crate::visualizer::{
    merge_config,
    hline, pad_frame, status_bar, title_line,
    AudioFrame, TermSize, Visualizer, FFT_SIZE,
};

const CONFIG_VERSION: u64 = 1;

pub struct PolarWaveformViz {
    source:      String,
    samples:     Vec<f32>,
    gain:        f32,
    base_radius: f32,
    theme:       String,
}

impl PolarWaveformViz {
    pub fn new(source: &str) -> Self {
        Self {
            source:      source.to_string(),
            samples:     vec![0.0; FFT_SIZE],
            gain:        1.0,
            base_radius: 0.55,
            theme:       "green".to_string(),
        }
    }

    /// Returns `(title_color, [c_low, c_mid, c_high])`.
    ///
    /// Three tiers correspond to amplitude bands: ≤0.25, ≤0.55, >0.55.
    /// White theme uses a narrow pure-white top tier so only the loudest
    /// peaks are white; quieter segments snap immediately to grey.
    fn palette(theme: &str) -> (u8, [u8; 3]) {
        match theme {
            "amber" => (220, [136, 172, 220]),
            "white" => (231, [240, 246, 231]),
            _       => ( 46, [ 28,  34,  46]),  // "green" default
        }
    }
}

/// Draw a line between two grid positions using Bresenham's algorithm.
/// Bright characters ('*') win over dim ones ('.') if cells overlap.
fn draw_line(
    canvas: &mut [Vec<(char, u8)>],
    r0: i32, c0: i32,
    r1: i32, c1: i32,
    ch: char, color: u8,
) {
    let rows = canvas.len() as i32;
    let cols = if rows > 0 { canvas[0].len() as i32 } else { 0 };

    let mut x = c0;
    let mut y = r0;
    let dx =  (c1 - c0).abs();
    let dy =  (r1 - r0).abs();
    let sx = if c0 < c1 { 1i32 } else { -1 };
    let sy = if r0 < r1 { 1i32 } else { -1 };
    let mut err = dx - dy;

    loop {
        if x >= 0 && x < cols && y >= 0 && y < rows {
            let cell = &mut canvas[y as usize][x as usize];
            // Only overwrite dim cells so bright segments win.
            if cell.0 == ' ' || cell.0 == '-' || (ch == '*' && cell.0 == '.') {
                *cell = (ch, color);
            }
        }
        if x == c1 && y == r1 { break; }
        let e2 = 2 * err;
        if e2 > -dy { err -= dy; x += sx; }
        if e2 <  dx { err += dx; y += sy; }
    }
}

impl Visualizer for PolarWaveformViz {
    fn name(&self)        -> &str { "polar" }
    fn description(&self) -> &str { "Polar waveform — circular oscilloscope" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "polar",
            "version": CONFIG_VERSION,
            "config": [
                {
                    "name": "gain",
                    "display_name": "Gain",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.1,
                    "max": 4.0
                },
                {
                    "name": "base_radius",
                    "display_name": "Base Radius",
                    "type": "float",
                    "value": 0.55,
                    "min": 0.2,
                    "max": 0.9
                },
                {
                    "name": "theme",
                    "display_name": "Phosphor Color",
                    "type": "enum",
                    "value": "green",
                    "variants": ["green", "amber", "white"]
                }
            ]
        })
        .to_string()
    }

    fn set_config(&mut self, json: &str) -> Result<String, String> {
        let merged = merge_config(&self.get_default_config(), json);
        let val: serde_json::Value = serde_json::from_str(&merged)
            .map_err(|e| format!("JSON parse error: {e}"))?;

        if let Some(config) = val["config"].as_array() {
            for entry in config {
                match entry["name"].as_str().unwrap_or("") {
                    "gain"        => self.gain        = entry["value"].as_f64().unwrap_or(1.0)  as f32,
                    "base_radius" => self.base_radius = entry["value"].as_f64().unwrap_or(0.55) as f32,
                    "theme"       => self.theme       = entry["value"].as_str().unwrap_or("green").to_string(),
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn tick(&mut self, audio: &AudioFrame, _dt: f32, _size: TermSize) {
        self.samples.clone_from(&audio.mono);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        // 3 rows overhead: title + hline + status
        let draw_rows = rows.saturating_sub(3);

        let (title_color, pal) = Self::palette(&self.theme);

        let mut lines = Vec::with_capacity(rows);
        lines.push(title_line(cols, " POLAR WAVEFORM ", title_color));

        if draw_rows == 0 || cols == 0 {
            lines.push(hline(cols, 238));
            lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
            return pad_frame(lines, rows, cols);
        }

        // ── Geometry ─────────────────────────────────────────────────────────
        //
        // Terminal characters are ~2× wider than they are tall.  To make the
        // figure appear circular we divide the Y screen-coordinate by 2
        // (equivalently, we work in "column units" throughout).
        //
        //   col = cx  +  r · cos(θ)
        //   row = cy  +  r · sin(θ) · 0.5
        //
        // max_r_cols: largest radius (in column units) that still fits inside
        // both the col extent and the row extent.

        let cx = cols as f32 / 2.0;
        let cy = draw_rows as f32 / 2.0;
        let half_w = cols as f32 / 2.0;
        let half_h = draw_rows as f32 / 2.0;

        // half_h * 2.0 converts row half-extent to column units.
        let max_r_cols = half_w.min(half_h * 2.0) * 0.93;
        let r_base     = self.base_radius * max_r_cols;
        // Maximum excursion above/below the base ring for full-scale amplitude.
        let r_amp      = max_r_cols * (1.0 - self.base_radius) * 0.85;

        // ── Canvas ───────────────────────────────────────────────────────────
        let mut canvas: Vec<Vec<(char, u8)>> = vec![vec![(' ', 0u8); cols]; draw_rows];

        // Reference ring at r_base (drawn first so the signal overwrites it).
        {
            let n_ref = ((r_base * PI * 2.0) as usize).max(64);
            for i in 0..n_ref {
                let theta = 2.0 * PI * i as f32 / n_ref as f32;
                let col = (cx + r_base * theta.cos()).round() as i32;
                let row = (cy + r_base * theta.sin() * 0.5).round() as i32;
                if row >= 0 && row < draw_rows as i32 && col >= 0 && col < cols as i32 {
                    let cell = &mut canvas[row as usize][col as usize];
                    if cell.0 == ' ' {
                        *cell = ('-', 236);
                    }
                }
            }
        }

        // ── Pre-compute waveform points ───────────────────────────────────────
        let n = FFT_SIZE;
        let mut pts: Vec<(i32, i32, f32)> = Vec::with_capacity(n);
        for i in 0..n {
            let theta = 2.0 * PI * i as f32 / n as f32;
            let amp   = (self.samples[i] * self.gain).clamp(-1.0, 1.0);
            let r     = r_base + amp * r_amp;
            let col   = (cx + r * theta.cos()).round() as i32;
            let row   = (cy + r * theta.sin() * 0.5).round() as i32;
            pts.push((row, col, amp.abs()));
        }

        // ── Draw waveform trace (connected segments) ──────────────────────────
        for i in 0..n {
            let (r0, c0, amp0) = pts[i];
            let (r1, c1, amp1) = pts[(i + 1) % n];
            let amp = (amp0 + amp1) * 0.5;

            let (ch, color): (char, u8) = if amp > 0.55 {
                ('*', pal[2])
            } else if amp > 0.25 {
                ('.', pal[1])
            } else {
                ('.', pal[0])
            };

            draw_line(&mut canvas, r0, c0, r1, c1, ch, color);
        }

        // ── Serialise canvas to ANSI strings ─────────────────────────────────
        for row_data in &canvas {
            let mut s = String::with_capacity(cols * 12);
            for &(ch, color) in row_data {
                if color > 0 {
                    s.push_str(&format!("\x1b[38;5;{color}m{ch}\x1b[0m"));
                } else {
                    s.push(' ');
                }
            }
            lines.push(s);
        }

        lines.push(hline(cols, 238));
        lines.push(status_bar(cols, fps, self.name(), &self.source, "mono→θ  amp→r"));
        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(PolarWaveformViz::new(""))]
}
