/// visualizer.rs — The Visualizer trait and all shared data types.
///
/// This file is intentionally kept free of application logic.  Its only job
/// is to define the stable interface between the core engine (main.rs) and
/// individual visualizers (src/visualizers/*.rs).

// ── Shared constants ──────────────────────────────────────────────────────────

/// Audio sample rate used throughout the application.
pub const SAMPLE_RATE: u32 = 44_100;

/// FFT window size.  Must be a power of two for rustfft efficiency.
pub const FFT_SIZE: usize = 4_096;

/// Number of audio channels captured (stereo).
pub const CHANNELS: usize = 2;

/// Target render rate in frames per second.
pub const FPS_TARGET: f32 = 45.0;

// ── Spectrum bar dynamics ─────────────────────────────────────────────────────

pub const RISE_COEFF:     f32 = 0.80;
pub const FALL_COEFF:     f32 = 0.55;
pub const PEAK_HOLD_SECS: f32 = 1.2;
pub const PEAK_DROP_RATE: f32 = 0.018;
pub const DB_MIN: f32 = -72.0;
pub const DB_MAX: f32 = -12.0;

// ── Colour palette ────────────────────────────────────────────────────────────

pub const SPEC_GRADIENT: &[u8] = &[
    196, 202, 208, 214, 220, 226, 190, 154, 118, 82, 46, 47, 48, 49, 50, 51,
    45, 39, 33, 27, 21, 57, 93, 129,
];

#[inline]
pub fn specgrad(frac: f32) -> u8 {
    let i = ((frac * (SPEC_GRADIENT.len() - 1) as f32) as usize)
        .min(SPEC_GRADIENT.len() - 1);
    SPEC_GRADIENT[i]
}

// ── Terminal size ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSize {
    pub rows: u16,
    pub cols: u16,
}

// ── Audio frame ───────────────────────────────────────────────────────────────

pub struct AudioFrame {
    pub left:        Vec<f32>,
    pub right:       Vec<f32>,
    pub mono:        Vec<f32>,
    pub fft:         Vec<f32>,
    pub sample_rate: u32,
}

// ── The core trait ────────────────────────────────────────────────────────────

pub trait Visualizer: Send {
    fn name(&self)        -> &str;
    fn description(&self) -> &str;
    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize);
    fn render(&self, size: TermSize, fps: f32) -> Vec<String>;
    fn on_resize(&mut self, _size: TermSize) {}

    // ── Runtime configuration interface ──────────────────────────────────────

    /// Return the default (reference) configuration for this visualizer as a
    /// JSON string.
    ///
    /// Schema:
    /// ```json
    /// {
    ///   "visualizer_name": "spectrum",
    ///   "version": 1,
    ///   "config": [
    ///     { "name": "gain", "display_name": "Gain", "type": "float",
    ///       "value": 1.0, "min": 0.0, "max": 4.0 },
    ///     { "name": "style", "display_name": "Style", "type": "enum",
    ///       "value": "solid", "variants": ["solid", "braille"] }
    ///   ]
    /// }
    /// ```
    ///
    /// `&self` is required by Rust's object-safety rules; implementations
    /// must never read instance state — every call returns the same schema
    /// regardless of the current configuration values.
    fn get_default_config(&self) -> String;

    /// Apply a (possibly partial) JSON configuration string.
    ///
    /// The input is merged against `get_default_config()`:
    ///   - Keys absent from `json`    → filled from the default
    ///   - Keys absent from the schema → silently dropped
    ///   - Values that fail type / range validation → fall back to the default
    ///
    /// On success returns the complete, cleaned JSON that was applied.
    /// This string is suitable for persisting to disk and round-tripping back.
    ///
    /// Returns `Err(description)` only when the input cannot be parsed at all.
    fn set_config(&mut self, json: &str) -> Result<String, String>;
}

// ── Config helpers ────────────────────────────────────────────────────────────

/// Return the platform-correct config file path for the named visualizer.
///
/// macOS:       ~/Library/Application Support/audio_viz/{name}.json
/// Linux/other: $XDG_CONFIG_HOME/audio_viz/{name}.json
///              (falls back to ~/.config/audio_viz/{name}.json)
pub fn config_path(name: &str) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("audio_viz")
            .join(format!("{name}.json"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                std::path::PathBuf::from(home).join(".config")
            });
        base.join("audio_viz").join(format!("{name}.json"))
    }
}

/// Merge a partial config JSON string into the default config.
///
/// The merge operates on the `config` array, matching entries by `"name"`.
///
///   - Entries in `default` not in `partial`  → kept with default value
///   - Entries in `partial` not in `default`  → silently dropped
///   - Entries in both                         → partial value applied if it
///       passes type / range / variants check; otherwise default is kept
///
/// Returns the complete merged JSON string (pretty-printed).
/// Returns `default` unchanged on any parse failure.
pub fn merge_config(default: &str, partial: &str) -> String {
    let default_val: serde_json::Value = match serde_json::from_str(default) {
        Ok(v) => v,
        Err(_) => return default.to_string(),
    };
    let partial_val: serde_json::Value = match serde_json::from_str(partial) {
        Ok(v) => v,
        Err(_) => return default.to_string(),
    };

    let default_config = match default_val["config"].as_array() {
        Some(arr) => arr.clone(),
        None => return default.to_string(),
    };

    let empty_arr: Vec<serde_json::Value> = Vec::new();
    let partial_config = partial_val["config"]
        .as_array()
        .unwrap_or(&empty_arr);

    // Build a name → value map from the partial config
    let partial_values: std::collections::HashMap<&str, &serde_json::Value> = partial_config
        .iter()
        .filter_map(|entry| {
            let name  = entry["name"].as_str()?;
            let value = entry.get("value")?;
            Some((name, value))
        })
        .collect();

    // Merge: for each schema entry apply the partial value when it validates
    let merged: Vec<serde_json::Value> = default_config.iter().map(|def| {
        let name = match def["name"].as_str() {
            Some(n) => n,
            None    => return def.clone(),
        };
        let Some(&partial_val) = partial_values.get(name) else {
            return def.clone();
        };
        let kind = def["type"].as_str().unwrap_or("");
        if validate_config_value(def, kind, partial_val) {
            let mut merged_entry = def.clone();
            merged_entry["value"] = partial_val.clone();
            merged_entry
        } else {
            def.clone()
        }
    }).collect();

    let mut result = default_val.clone();
    result["config"] = serde_json::Value::Array(merged);
    serde_json::to_string_pretty(&result).unwrap_or_else(|_| default.to_string())
}

/// Validate a candidate config value against its schema entry.
fn validate_config_value(
    schema: &serde_json::Value,
    kind:   &str,
    value:  &serde_json::Value,
) -> bool {
    match kind {
        "float" => {
            let Some(v) = value.as_f64() else { return false; };
            if let Some(min) = schema["min"].as_f64() { if v < min { return false; } }
            if let Some(max) = schema["max"].as_f64() { if v > max { return false; } }
            true
        }
        "int" => {
            let Some(v) = value.as_i64()
                .or_else(|| value.as_f64().map(|f| f as i64))
            else { return false; };
            if let Some(min) = schema["min"].as_i64() { if v < min { return false; } }
            if let Some(max) = schema["max"].as_i64() { if v > max { return false; } }
            true
        }
        "enum" => {
            let Some(v_str) = value.as_str() else { return false; };
            let Some(variants) = schema["variants"].as_array() else { return false; };
            variants.iter().any(|var| var.as_str() == Some(v_str))
        }
        "bool" => value.as_bool().is_some(),
        _ => false,
    }
}

// ── Shared DSP helpers ────────────────────────────────────────────────────────

pub fn build_binmap(n_bars: usize, fmin: f32, fmax: f32) -> (Vec<usize>, Vec<usize>) {
    let n_bins   = FFT_SIZE / 2 + 1;
    let freq_res = SAMPLE_RATE as f32 / FFT_SIZE as f32;

    let log_lo = fmin.log10();
    let log_hi = fmax.log10();

    let mut lo_bins = Vec::with_capacity(n_bars);
    let mut hi_bins = Vec::with_capacity(n_bars);

    for i in 0..n_bars {
        let edge_lo = 10f32.powf(log_lo + (log_hi - log_lo) * i       as f32 / n_bars as f32);
        let edge_hi = 10f32.powf(log_lo + (log_hi - log_lo) * (i + 1) as f32 / n_bars as f32);

        let lo = ((edge_lo / freq_res) as usize).clamp(1, n_bins - 2);
        let hi = ((edge_hi / freq_res) as usize).clamp(2, n_bins - 1);
        let hi = hi.max(lo + 1);

        lo_bins.push(lo);
        hi_bins.push(hi);
    }

    (lo_bins, hi_bins)
}

pub fn spec_to_bars(fft: &[f32], lo_bins: &[usize], hi_bins: &[usize]) -> Vec<f32> {
    lo_bins
        .iter()
        .zip(hi_bins.iter())
        .map(|(&lo, &hi)| {
            let slice = &fft[lo..hi.min(fft.len())];
            if slice.is_empty() { return 0.0; }
            let rms = (slice.iter().map(|v| v * v).sum::<f32>() / slice.len() as f32).sqrt();
            let db  = 20.0 * rms.max(1e-9).log10();
            ((db - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0.0, 1.0)
        })
        .collect()
}

// ── Shared per-visualizer spectrum bar state ──────────────────────────────────

pub struct SpectrumBars {
    pub smoothed: Vec<f32>,
    pub peaks:    Vec<f32>,
    peak_timers:  Vec<f32>,
    lo_bins:      Vec<usize>,
    hi_bins:      Vec<usize>,
    n_bars:       usize,
}

impl SpectrumBars {
    pub fn new(n_bars: usize) -> Self {
        let (lo, hi) = build_binmap(n_bars, 30.0, 18_000.0);
        Self {
            smoothed:    vec![0.0; n_bars],
            peaks:       vec![0.0; n_bars],
            peak_timers: vec![0.0; n_bars],
            lo_bins:     lo,
            hi_bins:     hi,
            n_bars,
        }
    }

    pub fn resize(&mut self, n_bars: usize) {
        if n_bars == self.n_bars { return; }
        *self = Self::new(n_bars);
    }

    pub fn update(&mut self, fft: &[f32], dt: f32) {
        let norm = spec_to_bars(fft, &self.lo_bins, &self.hi_bins);

        for i in 0..self.n_bars {
            let n = norm[i];
            let a = if n > self.smoothed[i] { RISE_COEFF } else { FALL_COEFF };
            self.smoothed[i] = a * self.smoothed[i] + (1.0 - a) * n;

            if self.smoothed[i] > self.peaks[i] {
                self.peaks[i]       = self.smoothed[i];
                self.peak_timers[i] = 0.0;
            } else {
                self.peak_timers[i] += dt;
                if self.peak_timers[i] > PEAK_HOLD_SECS {
                    self.peaks[i] = (self.peaks[i] - PEAK_DROP_RATE).max(0.0);
                }
            }
        }
    }
}

// ── ANSI rendering helpers ────────────────────────────────────────────────────

/// Build the status bar string (bottom row) common to all visualizers.
///
/// Key-bind hints are right-aligned in the remaining space so users always
/// know how to open the settings overlay and quit.
///
/// The `extra` argument may contain embedded ANSI escape sequences (e.g. the
/// lissajous beat indicator).  We reset and reapply the dim-grey style
/// explicitly before the hints so they always render cleanly.
pub fn status_bar(cols: usize, fps: f32, name: &str, source: &str, extra: &str) -> String {
    const HINTS: &str = "  [Esc] visualizers  [F1] settings  [F2] defaults  [q] quit  ";
    let hints_len = HINTS.len(); // all ASCII

    let left_cols = cols.saturating_sub(hints_len);
    let src_max   = left_cols.saturating_sub(30);
    let src_trunc = &source[..source.len().min(src_max)];

    let raw  = format!(" {:4.0} fps | {}{} | {}", fps, name, extra, src_trunc);
    // Count visible (non-ANSI) characters for truncation
    let raw_visible: String = {
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' { for ch in chars.by_ref() { if ch == 'm' { break; } } }
            else { out.push(c); }
        }
        out
    };
    let visible_len = raw_visible.chars().count().min(left_cols);

    // Truncate `raw` to `visible_len` visible characters, preserving ANSI
    let left = {
        let mut out   = String::with_capacity(raw.len());
        let mut count = 0usize;
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                let mut seq = String::from('\x1b');
                for ch in chars.by_ref() { seq.push(ch); if ch == 'm' { break; } }
                out.push_str(&seq);
            } else {
                if count >= left_cols { break; }
                out.push(c);
                count += 1;
            }
        }
        out
    };

    let gap = left_cols.saturating_sub(visible_len);

    // Reset any colour the `extra` string may have left open, then restyle hints.
    format!(
        "\x1b[2m\x1b[38;5;240m{left}{gap}\x1b[0m\x1b[2m\x1b[38;5;240m{HINTS}\x1b[0m",
        gap = " ".repeat(gap),
    )
}

/// A full-width horizontal rule in a dim colour.
pub fn hline(cols: usize, color: u8) -> String {
    format!("\x1b[2m\x1b[38;5;{color}m{}\x1b[0m", "-".repeat(cols))
}

/// A centred title string.
pub fn title_line(cols: usize, text: &str, color: u8) -> String {
    let pad = cols.saturating_sub(text.len()) / 2;
    format!("\x1b[1m\x1b[38;5;{color}m{}{}\x1b[0m", " ".repeat(pad), text)
}

/// Pad or truncate a Vec<String> to exactly `rows` entries of width `cols`.
pub fn pad_frame(mut lines: Vec<String>, rows: usize, cols: usize) -> Vec<String> {
    let blank = " ".repeat(cols);
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(blank.clone());
    }
    lines
}
