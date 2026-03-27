/// fire.rs — Audio-reactive rising ASCII fire.
///
/// A heat field is seeded at the bottom of the screen by bass and mid-range
/// energy.  Per-column spectrum intensity creates varied flame heights across
/// the screen.  Heat propagates upward via a weighted average of the three
/// cells below, with random flicker noise applied each frame.
///
/// Colour palette: near-black → deep red → orange → yellow → white.
/// Characters:     space → . → ` → ^ → ' → | → * → # → $ → @

// ── Index: FireViz@26 · new@36 · impl@52 · config@56 · set_config@73 · tick@92 · render@130 · register@164
use rand::Rng;
use crate::visualizer::{
    merge_config,
    pad_frame, status_bar,
    AudioFrame, SpectrumBars, TermSize, Visualizer,
};
use crate::visualizer_utils::with_gained_fft;

const CONFIG_VERSION: u64 = 1;

const FIRE_PAL:   &[u8]  = &[232, 52, 88, 124, 160, 196, 202, 208, 214, 220,
                              226, 227, 228, 229, 230, 231];
const FIRE_CHARS: &[u8]  = b" .`^'|*#$@";

pub struct FireViz {
    /// heat[row][col] ∈ [0, 1].  Row 0 = top of screen.
    heat:   Vec<Vec<f32>>,
    bars:   SpectrumBars,
    source: String,
    // ── Config fields ──────────────────────────────────────────────────────
    gain:   f32,
}

impl FireViz {
    pub fn new(source: &str) -> Self {
        Self {
            heat:   Vec::new(),
            bars:   SpectrumBars::new(80),
            source: source.to_string(),
            gain:   1.0,
        }
    }

    fn ensure_size(&mut self, rows: usize, cols: usize) {
        if self.heat.len() != rows || self.heat.first().map_or(0, |r| r.len()) != cols {
            self.heat = vec![vec![0.0f32; cols]; rows];
        }
    }
}

impl Visualizer for FireViz {
    fn name(&self)        -> &str { "fire" }
    fn description(&self) -> &str { "Audio-reactive ASCII fire" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "fire",
            "version": CONFIG_VERSION,
            "config": [
                {
                    "name": "gain",
                    "display_name": "Gain",
                    "type": "float",
                    "value": 1.0,
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
        self.ensure_size(size.rows as usize, size.cols as usize);
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let rows = size.rows as usize;
        let cols = size.cols as usize;

        self.bars.resize(cols);
        with_gained_fft(&audio.fft, self.gain, |fft| self.bars.update(fft, dt));
        self.ensure_size(rows, cols);

        let n   = self.bars.smoothed.len().max(1);
        let bot = rows.saturating_sub(2);

        let bass = self.bars.smoothed[..n / 6].iter().copied().sum::<f32>()
            / (n / 6).max(1) as f32;
        let mid  = self.bars.smoothed[n / 6..n / 3].iter().copied().sum::<f32>()
            / ((n / 3) - n / 6).max(1) as f32;

        let mut rng = rand::thread_rng();

        let base = (0.12 + bass * 1.3 + mid * 0.25).min(1.0);
        for c in 0..cols {
            let band  = (c * n / cols.max(1)).min(n - 1);
            let col_e = self.bars.smoothed[band];
            let noise: f32 = rng.gen_range(0.5..1.5);
            self.heat[bot][c] = (base * noise + col_e * 0.4).min(1.0);
        }

        for r in (0..bot).rev() {
            for c in 0..cols {
                let below   = self.heat[r + 1][c];
                let below_l = if c > 0         { self.heat[r + 1][c - 1] } else { below };
                let below_r = if c + 1 < cols  { self.heat[r + 1][c + 1] } else { below };
                let avg     = (below * 2.0 + below_l + below_r) / 4.0;
                let flicker: f32 = rng.gen_range(0.0..0.025);
                self.heat[r][c] = (avg * 0.92 - flicker).max(0.0);
            }
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1);

        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 12);
            let heat_row = if r < self.heat.len() { &self.heat[r] } else { &[] as &[f32] };

            for c in 0..cols {
                let h = if c < heat_row.len() { heat_row[c] } else { 0.0 };
                if h > 0.015 {
                    let pi   = ((h * (FIRE_PAL.len()   - 1) as f32) as usize)
                                 .min(FIRE_PAL.len()   - 1);
                    let ci   = ((h * (FIRE_CHARS.len() - 1) as f32) as usize)
                                 .min(FIRE_CHARS.len() - 1);
                    let code = FIRE_PAL[pi];
                    let ch   = FIRE_CHARS[ci] as char;
                    let bold = if h > 0.7 { "\x1b[1m" } else { "" };
                    line.push_str(&format!("{bold}\x1b[38;5;{code}m{ch}\x1b[0m"));
                } else {
                    line.push(' ');
                }
            }
            lines.push(line);
        }

        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(FireViz::new(""))]
}
