/// classic_lissajous.rs — Classic XY phosphor oscilloscope (Lissajous figure)
///
/// Maps the left audio channel to the X axis and the right channel to the Y
/// axis.  Each sample pair becomes a point in 2D space.  Points accumulate on
/// a persistence grid that decays over time — simulating a phosphor CRT.
///
/// ═══════════════════════════════════════════════════════════════════════════
///  CONFIG
/// ═══════════════════════════════════════════════════════════════════════════
///
///  gain        — amplitude multiplier applied to both channels before
///                plotting; controls how wide the figure spreads.
///  persistence — fraction of brightness retained per second (0 = instant
///                clear, 0.99 = very long tail).  Default 0.85.
///  theme       — phosphor color palette: "green" (classic P31), "amber"
///                (P3), or "white" (P4 — sharp fade through grey).

// ── Index: ClassicLissajousViz@27 · new@40 · impl@74 · config@78 · set_config@111 · tick@129 · render@173 · register@223
use crate::visualizer::{
    merge_config,
    hline, pad_frame, status_bar, title_line,
    AudioFrame, TermSize, Visualizer, FFT_SIZE,
};

const CONFIG_VERSION: u64 = 1;

pub struct ClassicLissajousViz {
    source:      String,
    gain:        f32,
    /// Fraction of brightness remaining after 1 second.
    persistence: f32,
    theme:       String,
    /// Flattened row-major brightness grid (f32 in [0, 1]).
    grid:        Vec<f32>,
    grid_rows:   usize,
    grid_cols:   usize,
}

impl ClassicLissajousViz {
    pub fn new(source: &str) -> Self {
        Self {
            source:      source.to_string(),
            gain:        1.0,
            persistence: 0.85,
            theme:       "green".to_string(),
            grid:        Vec::new(),
            grid_rows:   0,
            grid_cols:   0,
        }
    }

    fn resize_if_needed(&mut self, rows: usize, cols: usize) {
        if self.grid_rows != rows || self.grid_cols != cols {
            self.grid      = vec![0.0f32; rows * cols];
            self.grid_rows = rows;
            self.grid_cols = cols;
        }
    }

    /// Returns `(title_color, [c_dim, c_mid_dim, c_mid, c_bright, c_full])`.
    ///
    /// Five tiers map to the brightness bands: <0.15, <0.35, <0.60, <0.85, ≥0.85.
    /// White theme uses a narrow full-white band so the trace snaps sharply
    /// from white to grey rather than blending through warm tones.
    fn palette(theme: &str) -> (u8, [u8; 5]) {
        match theme {
            "amber" => (220, [130, 136, 172, 214, 220]),
            "white" => (231, [237, 242, 246, 250, 231]),
            _       => ( 46, [ 22,  28,  34,  40,  46]),  // "green" default
        }
    }
}

impl Visualizer for ClassicLissajousViz {
    fn name(&self)        -> &str { "classic_lissajous" }
    fn description(&self) -> &str { "Classic XY phosphor oscilloscope — Lissajous figure" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "classic_lissajous",
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
                    "name": "persistence",
                    "display_name": "Persistence",
                    "type": "float",
                    "value": 0.85,
                    "min": 0.0,
                    "max": 0.99
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
                    "persistence" => self.persistence = entry["value"].as_f64().unwrap_or(0.85) as f32,
                    "theme"       => self.theme       = entry["value"].as_str().unwrap_or("green").to_string(),
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        // 3 rows overhead: title + hline + status
        let draw_rows = (size.rows as usize).saturating_sub(3);
        let draw_cols = size.cols as usize;
        if draw_rows == 0 || draw_cols == 0 { return; }

        self.resize_if_needed(draw_rows, draw_cols);

        // Exponential decay towards zero (frame-rate independent).
        let decay = self.persistence.powf(dt);
        for b in self.grid.iter_mut() {
            *b *= decay;
        }

        // Dim axis crosshairs — only set where the signal hasn't already lit up.
        let cx = draw_cols / 2;
        let cy = draw_rows / 2;
        for c in 0..draw_cols {
            let idx = cy * draw_cols + c;
            if self.grid[idx] < 0.04 { self.grid[idx] = 0.04; }
        }
        for r in 0..draw_rows {
            let idx = r * draw_cols + cx;
            if self.grid[idx] < 0.04 { self.grid[idx] = 0.04; }
        }

        // Plot signal: left → X (cols), right → Y (rows).
        // [-1, 1] maps to [0, dim-1].  Y is flipped so +1 = top of screen.
        let col_scale = (draw_cols - 1) as f32;
        let row_scale = (draw_rows - 1) as f32;
        for i in 0..FFT_SIZE {
            let x =  audio.left [i] * self.gain;
            let y = -audio.right[i] * self.gain;

            let col = ((x + 1.0) * 0.5 * col_scale).round().clamp(0.0, col_scale) as usize;
            let row = ((y + 1.0) * 0.5 * row_scale).round().clamp(0.0, row_scale) as usize;

            let idx = row * draw_cols + col;
            if idx < self.grid.len() {
                self.grid[idx] = (self.grid[idx] + 0.35).min(1.0);
            }
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let draw_rows = rows.saturating_sub(3);

        let (title_color, pal) = Self::palette(&self.theme);

        let mut lines = Vec::with_capacity(rows);
        lines.push(title_line(cols, " LISSAJOUS ", title_color));

        for r in 0..draw_rows {
            let mut s = String::with_capacity(cols * 12);
            let row_base = r * self.grid_cols;

            for c in 0..cols {
                let b = if r < self.grid_rows && c < self.grid_cols {
                    self.grid[row_base + c]
                } else {
                    0.0
                };

                let (ch, color): (char, u8) = if b < 0.04 {
                    (' ', 0)
                } else if b < 0.15 {
                    ('.', pal[0])
                } else if b < 0.35 {
                    ('+', pal[1])
                } else if b < 0.60 {
                    ('*', pal[2])
                } else if b < 0.85 {
                    ('#', pal[3])
                } else {
                    ('@', pal[4])
                };

                if color > 0 {
                    s.push_str(&format!("\x1b[38;5;{color}m{ch}\x1b[0m"));
                } else {
                    s.push(' ');
                }
            }
            lines.push(s);
        }

        lines.push(hline(cols, 238));
        lines.push(status_bar(cols, fps, self.name(), &self.source, "L→X  R→Y"));
        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(ClassicLissajousViz::new(""))]
}
