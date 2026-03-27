/// waterfall.rs — Scrolling spectrogram: frequency on X, time flowing downward.
///
/// Each new frame the spectrum is captured as a row of colour-coded intensity
/// values. Rows scroll downward; the newest row is always at the top.
///
/// Config:
///   speed          — 1–4: how many rows to advance per frame (1=slow, 4=fast)
///   color_scheme   — heat / ice / spectrum / mono / phosphor
///   frequency_scale — linear / log
///   peak_hold      — 0–3 s: time a peak marker stays lit before fading

// ── Index: palettes@27 · WaterfallViz@53 · new@72 · impl@168 · config@172 · set_config@226 · tick@271 · render@311 · register@379
use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, TermSize, Visualizer, FFT_SIZE,
};
use crate::visualizer_utils::{
    palette_lookup, mag_to_frac as mag_to_frac_generic,
};

const CONFIG_VERSION: u64 = 1;

// ── Colour palettes ────────────────────────────────────────────────────────────

// Waterfall-specific palettes with leading black (232) for dark background
const HEAT:  &[u8] = &[232, 52, 88, 124, 160, 196, 202, 208, 214, 220, 226, 227, 228, 229, 230, 231];
const W_ICE: &[u8] = &[232, 17, 18, 19, 20, 21, 27, 33, 39, 45, 51, 87, 123, 159, 195, 231];
const PHOS:  &[u8] = &[232, 22, 28, 34, 40, 46, 82, 118, 154, 190, 226, 229, 231];

fn palette_mono(frac: f32) -> u8 {
    let level = (frac.clamp(0.0, 1.0) * 23.0) as u8;
    232 + level
}

fn color_for(frac: f32, scheme: &str) -> u8 {
    match scheme {
        "heat"     => palette_lookup(frac, HEAT),
        "ice"      => palette_lookup(frac, W_ICE),
        "mono"     => palette_mono(frac),
        "phosphor" => palette_lookup(frac, PHOS),
        _          => specgrad(frac),
    }
}

/// Convert linear FFT magnitude to dB-normalised 0..1 frac.
fn mag_to_frac(v: f32) -> f32 {
    mag_to_frac_generic(v, -72.0, -12.0)
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct WaterfallViz {
    /// Circular buffer of rows. Each row is `cols` frac values (0..1).
    history:  Vec<Vec<f32>>,
    /// Parallel peak buffer (frac per column).
    peaks:    Vec<f32>,
    peak_age: Vec<f32>,
    head:     usize,
    cached_cols: usize,
    source: String,
    // ── Config ────────────────────────────────────────────────────────────────
    gain:            f32,
    speed:           usize,  // 1–4 rows/frame
    color_scheme:    String,
    frequency_scale: String, // "linear" | "log"
    peak_hold:       f32,    // seconds
    freq_axis:       bool,
}

impl WaterfallViz {
    pub fn new(source: &str) -> Self {
        Self {
            history:         Vec::new(),
            peaks:           Vec::new(),
            peak_age:        Vec::new(),
            head:            0,
            cached_cols:     0,
            source:          source.to_string(),
            gain:            1.0,
            speed:           1,
            color_scheme:    "heat".to_string(),
            frequency_scale: "log".to_string(),
            peak_hold:       1.0,
            freq_axis:       false,
        }
    }

    fn ensure_buffers(&mut self, rows: usize, cols: usize) {
        if self.history.len() != rows || self.cached_cols != cols {
            self.history  = vec![vec![0.0f32; cols]; rows];
            self.peaks    = vec![0.0f32; cols];
            self.peak_age = vec![999.0f32; cols];
            self.head     = 0;
            self.cached_cols = cols;
        }
    }

    /// Build a labelled frequency axis row for display at the top.
    fn build_freq_axis(&self, cols: usize) -> String {
        use crate::visualizer::SAMPLE_RATE;
        // Key frequencies to label
        const LABELS: &[(f32, &str)] = &[
            (50.0,    "50"),
            (100.0,   "100"),
            (250.0,   "250"),
            (500.0,   "500"),
            (1_000.0, "1k"),
            (2_000.0, "2k"),
            (4_000.0, "4k"),
            (8_000.0, "8k"),
            (16_000.0,"16k"),
        ];

        let n_bins = FFT_SIZE / 2 + 1;
        let nyquist = SAMPLE_RATE as f32 / 2.0;
        let log = self.frequency_scale == "log";

        // Build plain character buffer first
        let mut buf = vec![b' '; cols];

        // Draw tick marks at key freq positions
        for &(freq, label) in LABELS {
            if freq > nyquist { break; }
            // Column for this frequency
            let col = if log {
                let lo = 1.0f32.ln();
                let hi = (n_bins as f32).ln();
                let bin = (freq / (SAMPLE_RATE as f32 / FFT_SIZE as f32)) as f32;
                let t = ((bin.max(1.0).ln() - lo) / (hi - lo)).clamp(0.0, 1.0);
                (t * (cols - 1) as f32) as usize
            } else {
                let bin_frac = freq / nyquist;
                (bin_frac * (cols - 1) as f32) as usize
            };
            if col >= cols { continue; }
            // Write the label left-aligned from the tick column
            let bytes = label.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if col + i < cols { buf[col + i] = b; }
            }
        }

        // Wrap in dim ANSI colour
        let mut line = String::with_capacity(cols * 8);
        line.push_str("\x1b[2m\x1b[38;5;240m");
        line.push_str(std::str::from_utf8(&buf).unwrap_or(""));
        line.push_str("\x1b[0m");
        line
    }

    /// Map a column index (0..cols) to an FFT bin index, honouring freq scale.
    fn col_to_bin(c: usize, cols: usize, n_bins: usize, log: bool) -> usize {
        if log {
            // log scale: map c to a bin with log spacing
            let lo = 1.0f32.ln();
            let hi = (n_bins as f32).ln();
            let t  = c as f32 / cols.max(1) as f32;
            ((lo + t * (hi - lo)).exp() as usize).clamp(1, n_bins - 1)
        } else {
            (c * n_bins / cols.max(1)).clamp(0, n_bins - 1)
        }
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for WaterfallViz {
    fn name(&self)        -> &str { "waterfall" }
    fn description(&self) -> &str { "Scrolling spectrogram — frequency vs time" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "waterfall",
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
                    "type": "int",
                    "value": 1,
                    "min": 1,
                    "max": 4
                },
                {
                    "name": "color_scheme",
                    "display_name": "Color Scheme",
                    "type": "enum",
                    "value": "heat",
                    "variants": ["heat", "ice", "spectrum", "mono", "phosphor"]
                },
                {
                    "name": "frequency_scale",
                    "display_name": "Frequency Scale",
                    "type": "enum",
                    "value": "log",
                    "variants": ["linear", "log"]
                },
                {
                    "name": "peak_hold",
                    "display_name": "Peak Hold (s)",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.0,
                    "max": 3.0
                },
                {
                    "name": "freq_axis",
                    "display_name": "Freq Axis",
                    "type": "enum",
                    "value": "off",
                    "variants": ["off", "on"]
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
                    "speed" => {
                        let v = entry["value"].as_i64()
                            .or_else(|| entry["value"].as_f64().map(|f| f as i64))
                            .unwrap_or(1);
                        self.speed = (v as usize).clamp(1, 4);
                    }
                    "color_scheme" => {
                        if let Some(s) = entry["value"].as_str() {
                            self.color_scheme = s.to_string();
                        }
                    }
                    "frequency_scale" => {
                        if let Some(s) = entry["value"].as_str() {
                            self.frequency_scale = s.to_string();
                        }
                    }
                    "gain" => {
                        self.gain = entry["value"].as_f64().unwrap_or(1.0) as f32;
                    }
                    "peak_hold" => {
                        self.peak_hold = entry["value"].as_f64().unwrap_or(1.0) as f32;
                    }
                    "freq_axis" => {
                        self.freq_axis = entry["value"].as_str() == Some("on");
                    }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        self.ensure_buffers(rows.saturating_sub(1).max(1), cols);
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let rows = (size.rows as usize).saturating_sub(1).max(1);
        let cols = size.cols as usize;
        self.ensure_buffers(rows, cols);

        let fft     = &audio.fft;
        let n_bins  = fft.len();
        let log     = self.frequency_scale == "log";

        // Build one row of frac values
        let new_row: Vec<f32> = (0..cols)
            .map(|c| {
                let bin = Self::col_to_bin(c, cols, n_bins, log);
                (mag_to_frac(fft[bin]) * self.gain).min(1.0)
            })
            .collect();

        // Advance peak markers
        for c in 0..cols {
            if self.peak_age[c] < self.peak_hold {
                self.peak_age[c] += dt;
            } else {
                // fade after hold
                self.peaks[c] = (self.peaks[c] - dt * 0.3).max(0.0);
            }
            if new_row[c] >= self.peaks[c] {
                self.peaks[c]    = new_row[c];
                self.peak_age[c] = 0.0;
            }
        }

        // Write `speed` copies of the new row into the circular buffer
        for _ in 0..self.speed {
            if rows > 0 {
                self.history[self.head] = new_row.clone();
                self.head = (self.head + 1) % rows;
            }
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let mut lines = Vec::with_capacity(rows);

        // ── Optional frequency axis (row 0) ──────────────────────────────────
        let data_start = if self.freq_axis {
            let axis_row = self.build_freq_axis(cols);
            lines.push(axis_row);
            1
        } else {
            0
        };

        let n_hist = self.history.len();

        for r in data_start..vis {
            let data_r = r - data_start; // row index into history
            // Row 0 = newest data; head-1 = most recently written row.
            let hist_idx = if n_hist > 0 {
                (self.head + n_hist - 1 - data_r % n_hist) % n_hist
            } else {
                0
            };

            let mut line = String::with_capacity(cols * 12);

            let row_data = if hist_idx < self.history.len() {
                &self.history[hist_idx]
            } else {
                &[] as &[f32]
            };

            for c in 0..cols {
                let frac = if c < row_data.len() { row_data[c] } else { 0.0 };

                // Peak marker on the newest data row only
                let is_peak = data_r == 0
                    && c < self.peaks.len()
                    && self.peaks[c] > 0.02
                    && (self.peaks[c] - frac).abs() < 0.12;

                if is_peak {
                    let code = color_for(self.peaks[c], &self.color_scheme);
                    line.push_str(&format!("\x1b[1m\x1b[38;5;{code}m▲\x1b[0m"));
                } else if frac < 0.04 {
                    line.push(' ');
                } else {
                    let code = color_for(frac, &self.color_scheme);
                    let ch = if frac < 0.25 { '░' }
                             else if frac < 0.50 { '▒' }
                             else if frac < 0.75 { '▓' }
                             else { '█' };
                    line.push_str(&format!("\x1b[38;5;{code}m{ch}\x1b[0m"));
                }
            }
            lines.push(line);
        }

        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(WaterfallViz::new(""))]
}
