/// scope.rs — Dual-channel time-domain oscilloscope.
///
/// The left and right audio channels are drawn as separate waveform panels,
/// stacked vertically.  Steep slopes are bridged with '|' connectors.
/// A dim zero-line runs through the centre of each panel.
///
/// Left channel: cyan (256-colour 51 / 39)
/// Right channel: orange (256-colour 214 / 208)

use crate::visualizer::{
    merge_config,
    pad_frame, status_bar, hline, title_line,
    AudioFrame, TermSize, Visualizer, FFT_SIZE, SAMPLE_RATE,
};

const CONFIG_VERSION: u64 = 1;

/// Default duration: exactly one full FFT window's worth of samples.
const DURATION_DEFAULT: f32 = FFT_SIZE as f32 / SAMPLE_RATE as f32;

pub struct ScopeViz {
    left:   Vec<f32>,
    right:  Vec<f32>,
    source: String,
    // ── Config fields ──────────────────────────────────────────────────────
    gain:     f32,
    /// Time window in seconds to display (controls visible waveform duration).
    duration: f32,
    /// When true, render a single averaged mono waveform instead of two channels.
    mono:     bool,
}

impl ScopeViz {
    pub fn new(source: &str) -> Self {
        Self {
            left:     vec![0.0; FFT_SIZE],
            right:    vec![0.0; FFT_SIZE],
            source:   source.to_string(),
            gain:     1.0,
            duration: DURATION_DEFAULT,
            mono:     false,
        }
    }

    /// Render one waveform channel into `height` rows × `cols` columns.
    fn draw_wave(samples: &[f32], height: usize, cols: usize, color_hi: u8, color_lo: u8)
        -> Vec<String>
    {
        let mut chars:  Vec<Vec<char>> = vec![vec![' '; cols]; height];
        let mut colors: Vec<Vec<u8>>   = vec![vec![0;   cols]; height];
        let mut bolds:  Vec<Vec<bool>> = vec![vec![false; cols]; height];

        let zero = height / 2;

        for c in 0..cols {
            chars [zero][c] = '-';
            colors[zero][c] = 234;
        }

        if samples.len() < 2 {
            return chars.iter().map(|row| row.iter().collect()).collect();
        }

        let mut rpos = vec![0usize; cols];
        let mut amps = vec![0f32;  cols];
        for xi in 0..cols {
            let src_idx = (xi as f32 / (cols - 1).max(1) as f32
                * (samples.len() - 1) as f32) as usize;
            let amp = samples[src_idx.min(samples.len() - 1)];
            amps[xi] = amp;
            let row = ((1.0 - amp) * 0.5 * (height - 1) as f32)
                .round()
                .clamp(0.0, (height - 1) as f32) as usize;
            rpos[xi] = row;
        }

        let mut prev = rpos[0];
        for xi in 0..cols {
            let cur  = rpos[xi];
            let amp  = amps[xi].abs();
            let code = if amp > 0.45 { color_hi } else { color_lo };
            let bold = amp > 0.3;

            let lo_r = prev.min(cur);
            let hi_r = prev.max(cur);
            for r in lo_r..=hi_r {
                chars [r][xi] = if r != cur { '|' } else if bold { '*' } else { '.' };
                colors[r][xi] = code;
                bolds [r][xi] = bold && (r == cur);
            }
            prev = cur;
        }

        chars
            .iter()
            .enumerate()
            .map(|(r, row)| {
                let mut s = String::with_capacity(cols * 12);
                for c in 0..cols {
                    let ch   = row[c];
                    let code = colors[r][c];
                    if code > 0 {
                        let bold_pfx = if bolds[r][c] { "\x1b[1m" } else { "" };
                        s.push_str(&format!("{bold_pfx}\x1b[38;5;{code}m{ch}\x1b[0m"));
                    } else {
                        s.push(ch);
                    }
                }
                s
            })
            .collect()
    }

    fn sep(cols: usize, label: &str, lcolor: u8) -> String {
        let lbl  = format!(" {label} ");
        let ld   = 3;
        let rd   = cols.saturating_sub(ld + lbl.len());
        format!(
            "\x1b[2m\x1b[38;5;238m{dashes}\x1b[0m\x1b[1m\x1b[38;5;{lcolor}m{lbl}\x1b[0m\x1b[2m\x1b[38;5;238m{rdashes}\x1b[0m",
            dashes  = "-".repeat(ld),
            rdashes = "-".repeat(rd),
        )
    }
}

impl Visualizer for ScopeViz {
    fn name(&self)        -> &str { "scope" }
    fn description(&self) -> &str { "Dual-channel time-domain oscilloscope" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "scope",
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
                    "name": "duration",
                    "display_name": "Duration (s)",
                    "type": "float",
                    "value": DURATION_DEFAULT,
                    "min": 0.01,
                    "max": 0.5
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
                    "gain"     => self.gain     = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "duration" => self.duration = entry["value"].as_f64().unwrap_or(DURATION_DEFAULT as f64) as f32,
                    "mode"     => self.mono     = entry["value"].as_str() == Some("mono"),
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn tick(&mut self, audio: &AudioFrame, _dt: f32, _size: TermSize) {
        if (self.gain - 1.0).abs() > f32::EPSILON {
            self.left  = audio.left .iter().map(|v| v * self.gain).collect();
            self.right = audio.right.iter().map(|v| v * self.gain).collect();
        } else {
            self.left .clone_from(&audio.left);
            self.right.clone_from(&audio.right);
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;

        // Number of samples to display based on the duration config
        let n_samples = ((self.duration * SAMPLE_RATE as f32) as usize)
            .clamp(2, FFT_SIZE);
        let left_slice  = &self.left [FFT_SIZE - n_samples..];
        let right_slice = &self.right[FFT_SIZE - n_samples..];

        let mut lines = Vec::with_capacity(rows);

        if self.mono {
            // Single averaged waveform
            // Overhead: title(1) + sep(1) + hline(1) + status(1) = 4 rows
            let vis = (rows.saturating_sub(4)).max(4);
            let mono_wave: Vec<f32> = left_slice.iter()
                .zip(right_slice.iter())
                .map(|(l, r)| (l + r) * 0.5)
                .collect();

            lines.push(title_line(cols, " OSCILLOSCOPE ", 51));
            lines.push(Self::sep(cols, "MONO", 51));
            lines.extend(Self::draw_wave(&mono_wave, vis, cols, 51, 39));
            lines.push(hline(cols, 238));
        } else {
            // Dual-channel stereo
            // Overhead: title(1) + sep_L(1) + sep_R(1) + hline(1) + status(1) = 5 rows
            let vis  = (rows.saturating_sub(5)).max(4);
            let half = vis / 2;

            lines.push(title_line(cols, " OSCILLOSCOPE ", 51));
            lines.push(Self::sep(cols, "LEFT  ch.1", 51));
            lines.extend(Self::draw_wave(left_slice,  half,       cols, 51,  39));
            lines.push(Self::sep(cols, "RIGHT ch.2", 214));
            lines.extend(Self::draw_wave(right_slice, vis - half, cols, 214, 208));
            lines.push(hline(cols, 238));
        }

        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(ScopeViz::new(""))]
}
