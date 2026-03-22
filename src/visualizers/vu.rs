/// vu.rs — Stereo / mono VU meter visualizer.
///
/// This file is intentionally kept as simple as possible to serve as a
/// reference implementation for developers adding new visualizers.
///
/// ═══════════════════════════════════════════════════════════════════
///  HOW TO ADD A NEW VISUALIZER
/// ═══════════════════════════════════════════════════════════════════
///
///  1. Create src/visualizers/yourname.rs (this file is your template).
///
///  2. Implement the `Visualizer` trait:
///       - `name()`        short lowercase string, used on the CLI
///       - `description()` one line shown in --list
///       - `tick()`        called every frame with fresh audio + dt seconds
///       - `render()`      return exactly `size.rows` strings, each exactly
///                         `size.cols` display-columns wide
///       - `on_resize()`   optional; invalidate any size-dependent caches
///       - `get_default_config()`  return canonical JSON config schema
///       - `set_config()`  merge + apply a partial JSON config
///
///  3. Export `pub fn register() -> Vec<Box<dyn Visualizer>>` returning
///     one entry per visualizer defined in this file.
///
///  4. Run `cargo build` — build.rs scans src/visualizers/*.rs automatically
///     and adds your visualizer to the registry.  No other files need editing.

use crate::visualizer::{
    merge_config,
    pad_frame, status_bar,
    AudioFrame, TermSize, Visualizer,
};

const CONFIG_VERSION: u64 = 1;

// ── Constants ─────────────────────────────────────────────────────────────────

const RISE: f32 = 0.30;
const FALL: f32 = 0.85;
const PEAK_HOLD: f32 = 1.5;
const PEAK_FALL: f32 = 0.40;

// ── Colour ramp ───────────────────────────────────────────────────────────────

fn level_colour(level: f32) -> u8 {
    if level > 0.85 {
        196 // bright red
    } else if level > 0.65 {
        214 // orange
    } else if level > 0.40 {
        226 // yellow
    } else {
        46  // green
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct VuViz {
    level_l: f32,
    level_r: f32,
    peak_l:  f32,
    peak_r:  f32,
    timer_l: f32,
    timer_r: f32,
    source: String,
    // ── Config fields ──────────────────────────────────────────────────────
    gain: f32,
    /// When true, average L+R and show a single mono bar.
    mono: bool,
}

impl VuViz {
    pub fn new(source: &str) -> Self {
        Self {
            level_l: 0.0,
            level_r: 0.0,
            peak_l:  0.0,
            peak_r:  0.0,
            timer_l: 0.0,
            timer_r: 0.0,
            source:  source.to_string(),
            gain:    1.0,
            mono:    false,
        }
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() { return 0.0; }
        let mean_sq = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
        mean_sq.sqrt()
    }

    fn update_channel(level: &mut f32, peak: &mut f32, timer: &mut f32, raw: f32, dt: f32) {
        let alpha = if raw > *level { RISE } else { FALL };
        *level = alpha * *level + (1.0 - alpha) * raw;
        if *level > *peak {
            *peak  = *level;
            *timer = 0.0;
        } else {
            *timer += dt;
            if *timer > PEAK_HOLD {
                *peak = (*peak - PEAK_FALL * dt).max(0.0);
            }
        }
    }

    fn render_bar(label: &str, level: f32, peak: f32, bar_width: usize) -> String {
        if bar_width == 0 { return label.to_string(); }

        let filled   = (level * bar_width as f32).round() as usize;
        let peak_pos = (peak  * bar_width as f32).round() as usize;
        let mut bar  = String::with_capacity(bar_width * 20);

        for i in 0..bar_width {
            if i < filled {
                let code = level_colour(i as f32 / bar_width as f32);
                bar.push_str(&format!("\x1b[38;5;{code}m█\x1b[0m"));
            } else if i == peak_pos && peak > 0.01 {
                let code = level_colour(i as f32 / bar_width as f32);
                bar.push_str(&format!("\x1b[1m\x1b[38;5;{code}m▌\x1b[0m"));
            } else {
                let is_tick = (i + 1) % (bar_width / 10).max(1) == 0;
                if is_tick {
                    bar.push_str("\x1b[38;5;236m·\x1b[0m");
                } else {
                    bar.push(' ');
                }
            }
        }
        format!("{label}{bar}")
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for VuViz {
    fn name(&self)        -> &str { "vu" }
    fn description(&self) -> &str { "Stereo / mono VU meter" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "vu",
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
                    "name": "mode",
                    "display_name": "Mode",
                    "type": "enum",
                    "value": "stereo",
                    "variants": ["stereo", "mono"]
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
                    "gain" => self.gain = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "mode" => self.mono = entry["value"].as_str() == Some("mono"),
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, _size: TermSize) {
        let (raw_l, raw_r) = if self.mono {
            let m = Self::rms(&audio.mono) * self.gain;
            (m, m)
        } else {
            (Self::rms(&audio.left)  * self.gain,
             Self::rms(&audio.right) * self.gain)
        };

        Self::update_channel(&mut self.level_l, &mut self.peak_l, &mut self.timer_l, raw_l, dt);
        Self::update_channel(&mut self.level_r, &mut self.peak_r, &mut self.timer_r, raw_r, dt);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;

        let label_w = 4;
        let bar_w   = cols.saturating_sub(label_w);

        let mut lines: Vec<String> = Vec::with_capacity(rows);

        lines.push(String::new());
        let title = if self.mono { " VU METER — MONO " } else { " VU METER — STEREO " };
        let pad   = cols.saturating_sub(title.len()) / 2;
        lines.push(format!("\x1b[1m\x1b[38;5;255m{}{}\x1b[0m", " ".repeat(pad), title));
        lines.push(String::new());

        if self.mono {
            lines.push(Self::render_bar(" M  ", self.level_l, self.peak_l, bar_w));
            lines.push(String::new());
        } else {
            lines.push(Self::render_bar(" L  ", self.level_l, self.peak_l, bar_w));
            lines.push(String::new());
            lines.push(Self::render_bar(" R  ", self.level_r, self.peak_r, bar_w));
            lines.push(String::new());
        }

        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(VuViz::new(""))]
}
