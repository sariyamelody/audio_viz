/// main.rs — Core application: CLI, audio capture, FFT pipeline, render loop.
///
/// Responsibilities
/// ────────────────
/// 1. Parse CLI arguments (clap).
/// 2. Enumerate audio devices and select an input source.
/// 3. Spawn an audio capture thread (cpal) that fills a lock-free ring buffer.
/// 4. Run the render loop on the main thread:
///      a. Drain the ring buffer into a window of FFT_SIZE samples.
///      b. Apply a Hann window and compute the rfft magnitude spectrum (rustfft).
///      c. Call viz.tick() with the AudioFrame.
///      d. Call viz.render() and write the result to stdout via crossterm.
///      e. Handle terminal resize events.
///      f. Handle F1 to open the settings overlay.
///      g. Sleep to target FPS_TARGET frames per second.

mod visualizer;
mod visualizers;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    style::Print,
    terminal::{self, ClearType},
};
use rustfft::{FftPlanner, num_complex::Complex};

use visualizer::{
    AudioFrame, TermSize, Visualizer,
    CHANNELS, FFT_SIZE, FPS_TARGET, SAMPLE_RATE,
    config_path, merge_config,
};

// ── ALSA / JACK stderr silencer ──────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod stderr_silence {
    use std::fs::OpenOptions;
    use std::os::unix::io::IntoRawFd;
    use std::sync::atomic::{AtomicI32, Ordering};

    static SAVED_STDERR: AtomicI32 = AtomicI32::new(-1);

    pub fn suppress() {
        if SAVED_STDERR.load(Ordering::Relaxed) >= 0 { return; }
        let saved = unsafe { libc::dup(2) };
        if saved < 0 { return; }
        SAVED_STDERR.store(saved, Ordering::Relaxed);
        if let Ok(dev) = OpenOptions::new().write(true).open("/dev/null") {
            unsafe { libc::dup2(dev.into_raw_fd(), 2); }
        }
    }

    pub fn write_err(msg: &str) {
        let fd = SAVED_STDERR.load(Ordering::Relaxed);
        if fd < 0 { return; }
        let b = msg.as_bytes();
        unsafe { libc::write(fd, b.as_ptr() as *const libc::c_void, b.len()); }
    }
}

#[cfg(not(target_os = "linux"))]
mod stderr_silence {
    pub fn suppress() {}
    pub fn write_err(msg: &str) { eprintln!("{msg}"); }
}

macro_rules! diag {
    ($($arg:tt)*) => {
        stderr_silence::write_err(&format!("{}\n", format_args!($($arg)*)))
    };
}

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name        = "audio_viz",
    about       = "Multi-mode terminal audio visualizer",
    long_about  = None,
)]
struct Cli {
    #[arg(default_value = "spectrum")]
    visualizer: String,

    #[arg(short, long)]
    device: Option<String>,

    #[arg(short, long)]
    list: bool,

    #[arg(long)]
    list_devices: bool,

    #[arg(long, default_value_t = FPS_TARGET)]
    fps: f32,
}

// ── Ring buffer ───────────────────────────────────────────────────────────────

type RingBuf = Arc<Mutex<Vec<f32>>>;

fn make_ring() -> RingBuf {
    Arc::new(Mutex::new(Vec::with_capacity(FFT_SIZE * CHANNELS * 4)))
}

// ── Audio host selection ──────────────────────────────────────────────────────

fn select_host() -> cpal::Host { cpal::default_host() }

// ── PulseAudio environment setup (Linux) ──────────────────────────────────────

#[cfg(target_os = "linux")]
fn prepare_pulse_env(host: &cpal::Host) -> anyhow::Result<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    use std::process::Command;

    let pulse_available = host
        .input_devices()
        .map(|mut devs| devs.any(|d| d.name().map(|n| n == "pulse").unwrap_or(false)))
        .unwrap_or(false);

    if !pulse_available {
        anyhow::bail!(
            "The ALSA PulseAudio plugin is not installed.\n\
             \n\
             audio_viz requires this plugin to capture system audio through\n\
             PipeWire or PulseAudio.  Install it with:\n\
             \n\
               Debian/Ubuntu:  sudo apt install libasound2-plugins\n\
               Fedora:         sudo dnf install alsa-plugins-pulse\n\
               Arch:           sudo pacman -S alsa-plugins\n\
             \n\
             After installing, run audio_viz again.\n\
             \n\
             Alternatively, select a specific device with --device:\n\
               audio_viz --list-devices\n\
               audio_viz --device <n>"
        );
    }

    let out = Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
        .map_err(|_| anyhow::anyhow!(
            "Could not run `pactl`.  Ensure PipeWire or PulseAudio is running\n\
             and pulseaudio-utils (or equivalent) is installed."
        ))?;

    let stdout = String::from_utf8_lossy(&out.stdout);

    let monitor = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .find(|name| name.contains(".monitor"))
        .ok_or_else(|| anyhow::anyhow!(
            "`pactl list short sources` returned no .monitor source.\n\
             Ensure PipeWire or PulseAudio is running."
        ))?
        .to_string();

    unsafe { std::env::set_var("PULSE_SOURCE", &monitor) };
    Ok(monitor)
}

#[cfg(not(target_os = "linux"))]
fn prepare_pulse_env(_host: &cpal::Host) -> anyhow::Result<String> { Ok(String::new()) }

// ── Audio device selection ────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn pulse_device_present(host: &cpal::Host) -> bool {
    use cpal::traits::{DeviceTrait, HostTrait};
    host.input_devices()
        .map(|mut devs| devs.any(|d| d.name().map(|n| n == "pulse").unwrap_or(false)))
        .unwrap_or(false)
}

fn find_best_device(host: &cpal::Host) -> Option<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};

    #[cfg(target_os = "linux")]
    if let Ok(mut devs) = host.input_devices() {
        if let Some(d) = devs.find(|d| d.name().map(|n| n == "pulse").unwrap_or(false)) {
            return Some(d);
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        if let Ok(mut devs) = host.input_devices() {
            if let Some(d) = devs.find(|d| {
                d.name().map(|n| {
                    let lc = n.to_lowercase();
                    lc.contains("blackhole") || lc.contains("loopback")
                }).unwrap_or(false)
            }) {
                return Some(d);
            }
        }
        diag!("audio: no loopback device found.");
        diag!("       Install BlackHole: https://existential.audio/blackhole/");
    }

    host.default_input_device()
}

fn find_device_by_name(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let name_lc = name.to_lowercase();
    if let Ok(mut devs) = host.input_devices() {
        if let Some(d) = devs.find(|d| {
            d.name().map(|n| n.to_lowercase().contains(&name_lc)).unwrap_or(false)
        }) {
            return Some(d);
        }
    }
    if let Ok(idx) = name.parse::<usize>() {
        if let Ok(devs) = host.input_devices() {
            return devs.into_iter().nth(idx);
        }
    }
    None
}

// ── Hann window ───────────────────────────────────────────────────────────────

fn hann_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos()))
        .collect()
}

// ── FFT pipeline ──────────────────────────────────────────────────────────────

fn compute_fft(mono: &[f32], window: &[f32], planner: &mut FftPlanner<f32>) -> Vec<f32> {
    let n = FFT_SIZE;
    let mut input: Vec<Complex<f32>> = (0..n)
        .map(|i| {
            let s = if i < mono.len() { mono[i] } else { 0.0 };
            Complex::new(s * window[i], 0.0)
        })
        .collect();
    let fft = planner.plan_fft_forward(n);
    fft.process(&mut input);
    let scale = 1.0 / n as f32;
    input[..n / 2 + 1].iter().map(|c| c.norm() * scale).collect()
}

// ── Terminal helpers ──────────────────────────────────────────────────────────

fn term_size() -> TermSize {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    TermSize { rows, cols }
}

// ── Config persistence ────────────────────────────────────────────────────────

/// Load saved config for the active visualizer, apply it, and write back the
/// merged/cleaned version.  Silently ignores I/O or parse errors so a corrupt
/// file never prevents startup.
fn load_and_apply_config(viz: &mut Box<dyn Visualizer>) {
    let path = config_path(viz.name());
    let saved = match std::fs::read_to_string(&path) {
        Ok(s)  => s,
        Err(_) => return,
    };
    match viz.set_config(&saved) {
        Ok(cleaned) => {
            // Write back the cleaned version to drop obsolete keys and fill
            // in any new fields added since the config was last saved.
            let _ = write_config(viz.name(), &cleaned);
        }
        Err(_) => {}
    }
}

/// Persist the current config to disk.
fn write_config(name: &str, json: &str) -> std::io::Result<()> {
    let path = config_path(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, json)
}

// ── Settings overlay ──────────────────────────────────────────────────────────
//
// Rendered as a centred modal box drawn over the last rendered frame.
// Navigation:
//   ↑ / ↓         — move between fields
//   ← / →         — for enum fields: move highlight in popup list
//                   for float/int fields: nudge value by one step
//   Enter / s      — confirm value, then on the save row: apply + persist
//   Backspace      — for text input: delete last char
//   0-9 / . / -    — for float/int fields: type a value directly
//   Esc            — discard all changes and close overlay

/// A live editing session for one visualizer's config.
/// We clone the default-config schema once at open time and edit in place.
struct SettingsOverlay {
    /// Current config entries being edited.  One per schema item.
    entries:  Vec<ConfigEntry>,
    /// Index of the currently focused row.
    cursor:   usize,
    /// When Some, the enum-popup for `entries[cursor]` is open.
    /// The inner usize is the currently highlighted variant index.
    popup:    Option<usize>,
    /// Inline text buffer for numeric fields during keyboard entry.
    text_buf: String,
    /// True while the user is actively typing into text_buf.
    /// Enter commits the value but does not save; another Enter or `s` saves.
    editing:  bool,
    /// One-frame error message shown next to an invalid numeric input.
    err_msg:  Option<String>,
}

struct ConfigEntry {
    name:         String,
    display_name: String,
    kind:         String,       // "float" | "int" | "enum" | "bool"
    value:        EntryValue,
    min:          Option<f64>,
    max:          Option<f64>,
    variants:     Vec<String>,  // only for enum
}

enum EntryValue {
    Float(f64),
    Int(i64),
    Enum(String),
    Bool(bool),
}

impl EntryValue {
    fn as_json(&self) -> serde_json::Value {
        match self {
            EntryValue::Float(v) => serde_json::json!(v),
            EntryValue::Int(v)   => serde_json::json!(v),
            EntryValue::Enum(v)  => serde_json::json!(v),
            EntryValue::Bool(v)  => serde_json::json!(v),
        }
    }

    fn display(&self) -> String {
        match self {
            EntryValue::Float(v) => format!("{:.2}", v),
            EntryValue::Int(v)   => format!("{}", v),
            EntryValue::Enum(v)  => v.clone(),
            EntryValue::Bool(v)  => if *v { "true" } else { "false" }.to_string(),
        }
    }
}

impl SettingsOverlay {
    /// Open the overlay populated with the user's current (live) config values.
    ///
    /// We obtain live values by reading the persisted config file and merging
    /// it against the schema defaults — the same operation performed at startup.
    /// This ensures the overlay always shows what is actually running.
    fn open(viz: &Box<dyn Visualizer>) -> Option<Self> {
        let default_json = viz.get_default_config();
        let schema: serde_json::Value = serde_json::from_str(&default_json).ok()?;
        let arr = schema["config"].as_array()?;

        // Build a name → live-value map from disk (merged against defaults).
        // If there is no saved file, every entry falls back to its schema default.
        let saved_raw = std::fs::read_to_string(config_path(viz.name())).unwrap_or_default();
        let live_json = if saved_raw.is_empty() {
            default_json.clone()
        } else {
            merge_config(&default_json, &saved_raw)
        };
        let live: serde_json::Value = serde_json::from_str(&live_json)
            .unwrap_or_else(|_| serde_json::from_str(&default_json).unwrap());

        let live_map: std::collections::HashMap<&str, &serde_json::Value> = live["config"]
            .as_array()
            .map(|a| a.iter()
                .filter_map(|x| Some((x["name"].as_str()?, x.get("value")?)))
                .collect())
            .unwrap_or_default();

        let entries = arr.iter().filter_map(|e| {
            let name         = e["name"].as_str()?.to_string();
            let display_name = e["display_name"].as_str().unwrap_or(&name).to_string();
            let kind         = e["type"].as_str()?.to_string();
            let min          = e["min"].as_f64();
            let max          = e["max"].as_f64();

            // Use the live value when present; fall back to schema default.
            let live_val = live_map.get(name.as_str())
                .copied()
                .unwrap_or(&e["value"]);

            let value = match kind.as_str() {
                "float" => EntryValue::Float(live_val.as_f64().unwrap_or(
                                e["value"].as_f64().unwrap_or(0.0))),
                "int"   => EntryValue::Int(live_val.as_i64().unwrap_or(
                                e["value"].as_i64().unwrap_or(0))),
                "enum"  => EntryValue::Enum(
                                live_val.as_str().unwrap_or(
                                    e["value"].as_str().unwrap_or("")).to_string()),
                "bool"  => EntryValue::Bool(live_val.as_bool().unwrap_or(
                                e["value"].as_bool().unwrap_or(false))),
                _       => return None,
            };

            let variants = e["variants"].as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();

            Some(ConfigEntry { name, display_name, kind, value, min, max, variants })
        }).collect();

        Some(Self {
            entries,
            cursor:   0,
            popup:    None,
            text_buf: String::new(),
            editing:  false,
            err_msg:  None,
        })
    }

    /// Nudge the currently focused float/int value by `delta` steps.
    fn nudge(&mut self, delta: f64) {
        self.err_msg = None;
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            match &mut entry.value {
                EntryValue::Float(v) => {
                    let step = 0.05 * delta;
                    *v = (*v + step).clamp(
                        entry.min.unwrap_or(f64::NEG_INFINITY),
                        entry.max.unwrap_or(f64::INFINITY),
                    );
                    // Round to 2 dp to avoid floating drift
                    *v = (*v * 100.0).round() / 100.0;
                }
                EntryValue::Int(v) => {
                    *v += delta as i64;
                    if let Some(mn) = entry.min { *v = (*v).max(mn as i64); }
                    if let Some(mx) = entry.max { *v = (*v).min(mx as i64); }
                }
                _ => {}
            }
        }
        self.text_buf.clear();
    }

    /// Commit the current text_buf as the field's value.
    fn commit_text(&mut self) {
        self.err_msg = None;
        let Some(entry) = self.entries.get_mut(self.cursor) else { return; };
        if self.text_buf.is_empty() { return; }

        match &mut entry.value {
            EntryValue::Float(v) => {
                match self.text_buf.parse::<f64>() {
                    Ok(parsed) => {
                        let clamped = parsed.clamp(
                            entry.min.unwrap_or(f64::NEG_INFINITY),
                            entry.max.unwrap_or(f64::INFINITY),
                        );
                        *v = (clamped * 100.0).round() / 100.0;
                        self.text_buf.clear();
                    }
                    Err(_) => {
                        self.err_msg = Some(format!("\"{}\" is not a valid number", self.text_buf));
                        self.text_buf.clear();
                    }
                }
            }
            EntryValue::Int(v) => {
                match self.text_buf.parse::<i64>() {
                    Ok(parsed) => {
                        let mut p = parsed;
                        if let Some(mn) = entry.min { p = p.max(mn as i64); }
                        if let Some(mx) = entry.max { p = p.min(mx as i64); }
                        *v = p;
                        self.text_buf.clear();
                    }
                    Err(_) => {
                        self.err_msg = Some(format!("\"{}\" is not a valid integer", self.text_buf));
                        self.text_buf.clear();
                    }
                }
            }
            _ => { self.text_buf.clear(); }
        }
    }

    /// Build the partial JSON accepted by set_config() from the current entries.
    fn to_partial_json(&self) -> String {
        let arr: Vec<serde_json::Value> = self.entries.iter().map(|e| {
            serde_json::json!({
                "name":  e.name,
                "value": e.value.as_json(),
            })
        }).collect();
        serde_json::json!({ "config": arr }).to_string()
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    /// Render the overlay box as a Vec<String> that will be composited over the
    /// frozen visualizer frame.  Only the cells within the box are overwritten.
    fn render_over(&self, base: &[String], size: TermSize) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;

        let n_items = self.entries.len();
        // Box height: title(1) + blank(1) + items + blank(1) + err/blank(1) + save(1) + bottom(1)
        let box_h   = (n_items + 6).min(rows.saturating_sub(2));
        let box_w   = (56usize).min(cols.saturating_sub(4));
        let box_row = (rows.saturating_sub(box_h)) / 2;
        let box_col = (cols.saturating_sub(box_w)) / 2;

        // Clone the base frame
        let mut out: Vec<String> = base.to_vec();
        while out.len() < rows { out.push(" ".repeat(cols)); }

        // ── Helper: overwrite a region of one row ──────────────────────────
        // Replaces characters at [col_start .. col_start + text.display_len]
        // without touching the rest of the row.
        let overwrite = |row: &mut String, col_start: usize, text: &str| {
            // Strip existing ANSI from source to measure display width
            let chars_before: Vec<char> = strip_ansi(row).chars().collect();
            let total = cols;

            // Rebuild the line with the new text spliced in
            let mut result = String::with_capacity(total * 12);

            // Characters before the insertion point
            let mut display_col = 0usize;
            let raw_chars: Vec<char> = row.chars().collect();
            let mut ri = 0usize; // raw index into row

            // Fast-forward through ANSI escapes + visible chars until we reach col_start
            while display_col < col_start && ri < raw_chars.len() {
                if raw_chars[ri] == '\x1b' {
                    // copy the escape sequence verbatim
                    result.push(raw_chars[ri]);
                    ri += 1;
                    while ri < raw_chars.len() && raw_chars[ri] != 'm' {
                        result.push(raw_chars[ri]);
                        ri += 1;
                    }
                    if ri < raw_chars.len() { result.push(raw_chars[ri]); ri += 1; }
                } else {
                    result.push(raw_chars[ri]);
                    ri += 1;
                    display_col += 1;
                }
            }

            // Reset so the overlay text stands cleanly
            result.push_str("\x1b[0m");
            result.push_str(text);
            result.push_str("\x1b[0m");

            // Advance source past the region we overwrote
            let text_display_len = strip_ansi_str(text).chars().count();
            let mut skipped = 0usize;
            while skipped < text_display_len && ri < raw_chars.len() {
                if raw_chars[ri] == '\x1b' {
                    ri += 1;
                    while ri < raw_chars.len() && raw_chars[ri] != 'm' { ri += 1; }
                    if ri < raw_chars.len() { ri += 1; }
                } else {
                    ri += 1;
                    skipped += 1;
                }
            }

            // Copy remainder of the original line
            while ri < raw_chars.len() {
                result.push(raw_chars[ri]);
                ri += 1;
            }

            let _ = chars_before; // suppress unused warning
            *row = result;
        };

        // ── Draw box border ───────────────────────────────────────────────
        let inner_w = box_w.saturating_sub(2);

        // Top border
        if box_row < rows {
            let top = format!(
                "\x1b[1m\x1b[38;5;255m╔{}╗\x1b[0m",
                "═".repeat(inner_w)
            );
            overwrite(&mut out[box_row], box_col, &top);
        }

        // Bottom border
        let bot_row = box_row + box_h.saturating_sub(1);
        if bot_row < rows {
            let bot = format!(
                "\x1b[1m\x1b[38;5;255m╚{}╝\x1b[0m",
                "═".repeat(inner_w)
            );
            overwrite(&mut out[bot_row], box_col, &bot);
        }

        // Side borders + content rows
        for r in (box_row + 1)..bot_row {
            if r >= rows { break; }
            let inner_row = r - box_row - 1;
            let content   = self.box_content(inner_row, inner_w);
            let side      = format!("\x1b[1m\x1b[38;5;255m║\x1b[0m{}\x1b[1m\x1b[38;5;255m║\x1b[0m", content);
            overwrite(&mut out[r], box_col, &side);
        }

        // ── Draw enum popup if open ───────────────────────────────────────
        if let Some(hi) = self.popup {
            if let Some(entry) = self.entries.get(self.cursor) {
                let item_row  = box_row + 1 + 2 + self.cursor; // title(1)+blank(1)+items
                let pop_top   = (item_row + 1).min(rows - entry.variants.len().min(8) - 2);
                let pop_left  = box_col + 2;
                let pop_w     = entry.variants.iter().map(|v| v.len()).max().unwrap_or(8) + 4;
                let pop_w     = pop_w.min(inner_w);

                // Popup top border
                if pop_top < rows {
                    let b = format!("\x1b[38;5;244m┌{}┐\x1b[0m", "─".repeat(pop_w.saturating_sub(2)));
                    overwrite(&mut out[pop_top], pop_left, &b);
                }
                for (vi, variant) in entry.variants.iter().enumerate().take(8) {
                    let pr = pop_top + 1 + vi;
                    if pr >= rows { break; }
                    let selected = vi == hi;
                    let row_str = if selected {
                        format!("\x1b[38;5;244m│\x1b[0m\x1b[1m\x1b[38;5;33m▶ {:<width$}\x1b[0m\x1b[38;5;244m│\x1b[0m",
                            variant, width = pop_w.saturating_sub(4))
                    } else {
                        format!("\x1b[38;5;244m│\x1b[0m  {:<width$}\x1b[38;5;244m│\x1b[0m",
                            variant, width = pop_w.saturating_sub(4))
                    };
                    overwrite(&mut out[pr], pop_left, &row_str);
                }
                let pop_bot = pop_top + 1 + entry.variants.len().min(8);
                if pop_bot < rows {
                    let b = format!("\x1b[38;5;244m└{}┘\x1b[0m", "─".repeat(pop_w.saturating_sub(2)));
                    overwrite(&mut out[pop_bot], pop_left, &b);
                }
            }
        }

        out
    }

    /// Build the text for content row `inner_row` of width `w`.
    fn box_content(&self, inner_row: usize, w: usize) -> String {
        let n = self.entries.len();
        match inner_row {
            // Row 0: title
            0 => {
                let title = "[ SETTINGS ]";
                let pad   = w.saturating_sub(title.len()) / 2;
                format!("\x1b[1m\x1b[38;5;255m{}{:<width$}\x1b[0m",
                    " ".repeat(pad), title, width = w.saturating_sub(pad))
            }
            // Row 1: blank separator
            1 => " ".repeat(w),
            // Rows 2 .. 2+n-1: config items
            r if r >= 2 && r < 2 + n => {
                let idx     = r - 2;
                let entry   = &self.entries[idx];
                let focused = idx == self.cursor;

                let label_w = 20usize;
                // Total row width budget:
                //   indicator (2) + label (label_w) + value + hint (2) = w
                // → val_w = w - label_w - 2 - 2 = w - label_w - 4
                let val_w   = w.saturating_sub(label_w + 4);

                // Label column
                let label_col = if focused {
                    format!("\x1b[1m\x1b[38;5;33m▶ {:<lw$}\x1b[0m", entry.display_name, lw = label_w)
                } else {
                    format!("  {:<lw$}", entry.display_name, lw = label_w)
                };

                // Value column — val_w chars total.
                //
                // When the user is actively typing (self.editing), we show:
                //   [<text_buf padded>]  <dim bounds hint>
                // where the bounds hint uses any remaining space after the bracket.
                //
                // Bounds hint format:  "(min..max)"  e.g. "(0.00..4.00)"
                // It is rendered in dim text immediately after the closing bracket.
                // If there is no space for it the hint is simply omitted.
                //
                // When not typing:
                //   Focused:   [<value>]
                //   Unfocused:  <value>

                // Bool fields render as a checkbox — no editing mode, no bounds hint.
                if entry.kind == "bool" {
                    let checked = matches!(&entry.value, EntryValue::Bool(true));
                    let checkbox = if checked {
                        "\x1b[1m\x1b[38;5;82m[✓]\x1b[0m"
                    } else {
                        "\x1b[38;5;244m[ ]\x1b[0m"
                    };
                    let hint = if focused { "\x1b[2m<>\x1b[0m" } else { "  " };
                    let gap = val_w.saturating_sub(3); // "[ ]" / "[✓]" = 3 visible chars
                    let checkbox_col = format!("{}{}", checkbox, " ".repeat(gap));
                    return format!("{}{}{}", label_col, checkbox_col, hint);
                }

                // Build the optional bounds hint string (visible chars only).
                let bounds_hint: Option<String> = if focused && self.editing && entry.kind != "enum" {
                    match (entry.min, entry.max) {
                        (Some(lo), Some(hi)) => {
                            if entry.kind == "int" {
                                Some(format!(" ({:.0}..{:.0})", lo, hi))
                            } else {
                                Some(format!(" ({:.2}..{:.2})", lo, hi))
                            }
                        }
                        (Some(lo), None) => {
                            if entry.kind == "int" {
                                Some(format!(" (>={:.0})", lo))
                            } else {
                                Some(format!(" (>={:.2})", lo))
                            }
                        }
                        (None, Some(hi)) => {
                            if entry.kind == "int" {
                                Some(format!(" (<={:.0})", hi))
                            } else {
                                Some(format!(" (<={:.2})", hi))
                            }
                        }
                        (None, None) => None,
                    }
                } else {
                    None
                };

                // How many chars are available for the editable/value portion?
                // If we have a bounds hint, shrink the value area to fit it.
                let bounds_vis = bounds_hint.as_deref().map(|s| s.len()).unwrap_or(0);
                // Bracket chars: 2 when focused (or editing), 0 when unfocused
                let bracket_chars = if focused { 2 } else { 0 };
                // Inner value width (inside brackets or flush)
                let inner_w = val_w
                    .saturating_sub(bracket_chars)
                    .saturating_sub(bounds_vis);

                let val_str = if focused && self.editing && entry.kind != "enum" {
                    // Actively typing: yellow text inside brackets, then bounds
                    let typed = format!("\x1b[1m\x1b[38;5;51m[\x1b[38;5;220m{:<iw$}\x1b[38;5;51m]\x1b[0m",
                        self.text_buf, iw = inner_w);
                    if let Some(ref bh) = bounds_hint {
                        format!("{}{}{}{}",
                            typed,
                            "\x1b[2m\x1b[38;5;238m",
                            bh,
                            "\x1b[0m")
                    } else {
                        typed
                    }
                } else {
                    let v = entry.value.display();
                    if focused {
                        format!("\x1b[1m\x1b[38;5;51m[{:<iw$}]\x1b[0m", v, iw = inner_w)
                    } else {
                        format!("\x1b[38;5;250m {:<iw$}\x1b[0m", v, iw = val_w.saturating_sub(1))
                    }
                };

                // Hint column — exactly 2 chars wide
                let hint = if focused {
                    "\x1b[2m<>\x1b[0m"
                } else {
                    "  "
                };

                format!("{}{}{}", label_col, val_str, hint)
            }
            // Blank separator before error/save
            r if r == 2 + n => {
                if let Some(msg) = &self.err_msg {
                    let truncated: String = msg.chars().take(w.saturating_sub(2)).collect();
                    format!("\x1b[38;5;196m ⚠ {:<width$}\x1b[0m", truncated, width = w.saturating_sub(4))
                } else {
                    " ".repeat(w)
                }
            }
            // Save / cancel row — hint adapts to current editing state
            r if r == 3 + n => {
                let popup_open = self.popup.is_some();
                let focused_bool = self.entries.get(self.cursor)
                    .map_or(false, |e| e.kind == "bool");
                let save_hint = if self.editing {
                    " [↵] Confirm  [s] Save & close  [Esc] Cancel "
                } else if popup_open {
                    " [↵] Select  [Esc] Close menu "
                } else if focused_bool {
                    " [Space] Toggle  [s/↵] Save & close  [Esc] Cancel "
                } else {
                    " [Space] Edit  [s/↵] Save & close  [Esc] Cancel "
                };
                let pad = w.saturating_sub(save_hint.len()) / 2;
                format!("\x1b[2m\x1b[38;5;240m{}{:<width$}\x1b[0m",
                    " ".repeat(pad), save_hint, width = w.saturating_sub(pad))
            }
            _ => " ".repeat(w),
        }
    }
}


// ── Visualizer picker ─────────────────────────────────────────────────────────
//
// A two-level modal overlay for selecting a visualizer.
//
// Level 1 — Category list: opened with Esc when nothing else is active.
// Level 2 — Visualizer list within a category: opened by pressing Enter on a
//            category row.
//
// Navigation (both levels): ↑/↓ move cursor, Enter descends/selects, Esc
// goes back one level (or closes the picker from the category level).

/// Which level the picker is currently displaying.
enum PickerLevel {
    /// Showing the list of categories.  `cursor` is the highlighted category.
    Category { cursor: usize },
    /// Showing the visualizers inside one category.
    Visualizer { category_idx: usize, cursor: usize },
}

struct VizPicker {
    /// (display_name, [(viz_name, viz_desc)])
    categories:  Vec<(String, Vec<(String, String)>)>,
    /// Name of the currently running visualizer.
    active_name: String,
    level:       PickerLevel,
}

impl VizPicker {
    /// Build a picker pre-focused on the category that contains `current_name`.
    fn open(categories: &[(String, Vec<(String, String)>)], current_name: &str) -> Self {
        // Find which category the active visualizer lives in.
        let active_cat = categories
            .iter()
            .position(|(_, vizs)| vizs.iter().any(|(n, _)| n == current_name))
            .unwrap_or(0);
        Self {
            categories:  categories.to_vec(),
            active_name: current_name.to_string(),
            level:       PickerLevel::Category { cursor: active_cat },
        }
    }

    /// If the picker is about to switch visualizers, return the chosen name.
    /// Called from the Enter handler.  Returns `None` when Enter just drills
    /// into a category rather than selecting a visualizer.
    fn enter(&mut self) -> Option<String> {
        match &self.level {
            PickerLevel::Category { cursor } => {
                let cat_idx = *cursor;
                // Pre-highlight the active visualizer if it's in this category.
                let viz_cursor = self.categories[cat_idx]
                    .1
                    .iter()
                    .position(|(n, _)| *n == self.active_name)
                    .unwrap_or(0);
                self.level = PickerLevel::Visualizer { category_idx: cat_idx, cursor: viz_cursor };
                None
            }
            PickerLevel::Visualizer { category_idx, cursor } => {
                let name = self.categories[*category_idx].1[*cursor].0.clone();
                Some(name)
            }
        }
    }

    /// Move cursor up within the current level.
    fn up(&mut self) {
        match &mut self.level {
            PickerLevel::Category { cursor } => {
                if *cursor > 0 { *cursor -= 1; } else { *cursor = self.categories.len().saturating_sub(1); }
            }
            PickerLevel::Visualizer { category_idx, cursor } => {
                let n = self.categories[*category_idx].1.len();
                if *cursor > 0 { *cursor -= 1; } else { *cursor = n.saturating_sub(1); }
            }
        }
    }

    /// Move cursor down within the current level.
    fn down(&mut self) {
        match &mut self.level {
            PickerLevel::Category { cursor } => {
                *cursor = (*cursor + 1) % self.categories.len().max(1);
            }
            PickerLevel::Visualizer { category_idx, cursor } => {
                let n = self.categories[*category_idx].1.len();
                *cursor = (*cursor + 1) % n.max(1);
            }
        }
    }

    /// Escape: go back to category level, or signal that the picker should
    /// close (returns `true` when the picker should be dismissed).
    fn escape(&mut self) -> bool {
        match &self.level {
            PickerLevel::Visualizer { category_idx, .. } => {
                let cat = *category_idx;
                self.level = PickerLevel::Category { cursor: cat };
                false
            }
            PickerLevel::Category { .. } => true,
        }
    }

    /// Render the picker as a centred modal over `base`.
    fn render_over(&self, base: &[String], size: TermSize) -> Vec<String> {
        match &self.level {
            PickerLevel::Category { cursor } =>
                self.render_category_level(*cursor, base, size),
            PickerLevel::Visualizer { category_idx, cursor } =>
                self.render_visualizer_level(*category_idx, *cursor, base, size),
        }
    }

    // ── shared helper: overwrite a region of one rendered row ─────────────────

    fn overwrite(row: &mut String, col_start: usize, text: &str) {
        let text_vis: String = {
            let mut s = String::new();
            let mut ch = text.chars();
            while let Some(c) = ch.next() {
                if c == '\x1b' { for x in ch.by_ref() { if x == 'm' { break; } } }
                else { s.push(c); }
            }
            s
        };
        let tlen = text_vis.chars().count();
        let raw_chars: Vec<char> = row.chars().collect();
        let mut result = String::with_capacity(row.len() + text.len());
        let mut dcol = 0usize;
        let mut ri   = 0usize;
        while dcol < col_start && ri < raw_chars.len() {
            if raw_chars[ri] == '\x1b' {
                result.push(raw_chars[ri]); ri += 1;
                while ri < raw_chars.len() && raw_chars[ri] != 'm' { result.push(raw_chars[ri]); ri += 1; }
                if ri < raw_chars.len() { result.push(raw_chars[ri]); ri += 1; }
            } else { result.push(raw_chars[ri]); ri += 1; dcol += 1; }
        }
        result.push_str("\x1b[0m");
        result.push_str(text);
        result.push_str("\x1b[0m");
        let mut skipped = 0usize;
        while skipped < tlen && ri < raw_chars.len() {
            if raw_chars[ri] == '\x1b' {
                ri += 1;
                while ri < raw_chars.len() && raw_chars[ri] != 'm' { ri += 1; }
                if ri < raw_chars.len() { ri += 1; }
            } else { ri += 1; skipped += 1; }
        }
        while ri < raw_chars.len() { result.push(raw_chars[ri]); ri += 1; }
        *row = result;
    }

    fn draw_box(out: &mut Vec<String>, rows: usize, cols: usize,
                box_row: usize, box_col: usize, box_h: usize, box_w: usize,
                inner_w: usize, content_fn: impl Fn(usize, usize) -> String) {
        if box_row < rows {
            Self::overwrite(&mut out[box_row], box_col,
                &format!("\x1b[1m\x1b[38;5;255m╔{}╗\x1b[0m", "═".repeat(inner_w)));
        }
        let bot = box_row + box_h.saturating_sub(1);
        if bot < rows {
            Self::overwrite(&mut out[bot], box_col,
                &format!("\x1b[1m\x1b[38;5;255m╚{}╝\x1b[0m", "═".repeat(inner_w)));
        }
        for r in (box_row + 1)..bot {
            if r >= rows { break; }
            let content = content_fn(r - box_row - 1, inner_w);
            let side = format!("\x1b[1m\x1b[38;5;255m║\x1b[0m{}\x1b[1m\x1b[38;5;255m║\x1b[0m", content);
            Self::overwrite(&mut out[r], box_col, &side);
        }
        let _ = (cols, box_w); // suppress unused warnings
    }

    // ── Level 1: category list ────────────────────────────────────────────────

    fn render_category_level(&self, cursor: usize, base: &[String], size: TermSize) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let n    = self.categories.len();

        let box_h   = (n + 4).min(rows.saturating_sub(2));
        let box_w   = (52usize).min(cols.saturating_sub(4));
        let box_row = (rows.saturating_sub(box_h)) / 2;
        let box_col = (cols.saturating_sub(box_w)) / 2;
        let inner_w = box_w.saturating_sub(2);

        let mut out: Vec<String> = base.to_vec();
        while out.len() < rows { out.push(" ".repeat(cols)); }

        // Which category contains the active visualizer?
        let active_cat = self.categories
            .iter()
            .position(|(_, vizs)| vizs.iter().any(|(n, _)| *n == self.active_name))
            .unwrap_or(usize::MAX);

        Self::draw_box(&mut out, rows, cols, box_row, box_col, box_h, box_w, inner_w, |inner_row, w| {
            match inner_row {
                0 => {
                    let title = "[ SELECT CATEGORY ]";
                    let pad = w.saturating_sub(title.len()) / 2;
                    format!("\x1b[1m\x1b[38;5;255m{}{:<width$}\x1b[0m",
                        " ".repeat(pad), title, width = w.saturating_sub(pad))
                }
                1 => " ".repeat(w),
                r if r >= 2 && r < 2 + n => {
                    let idx       = r - 2;
                    let focused   = idx == cursor;
                    let has_active = idx == active_cat;
                    let (cat_name, vizs) = &self.categories[idx];
                    let display   = Self::capitalize(cat_name);
                    let count     = format!("({} visualizers)", vizs.len());
                    let name_w    = 14usize;
                    let count_w   = w.saturating_sub(name_w + 4);

                    let indicator = if has_active && focused { "▶●" }
                                    else if has_active       { " ●" }
                                    else if focused          { "▶ " }
                                    else                     { "  " };

                    let ind_col = if focused {
                        format!("\x1b[1m\x1b[38;5;33m{}\x1b[0m ", indicator)
                    } else {
                        format!("\x1b[38;5;238m{}\x1b[0m ", indicator)
                    };
                    let name_col = if focused {
                        format!("\x1b[1m\x1b[38;5;33m{:<nw$}\x1b[0m", display, nw = name_w)
                    } else if has_active {
                        format!("\x1b[38;5;255m{:<nw$}\x1b[0m", display, nw = name_w)
                    } else {
                        format!("\x1b[38;5;244m{:<nw$}\x1b[0m", display, nw = name_w)
                    };
                    let count_col = {
                        let truncated: String = count.chars().take(count_w).collect();
                        if focused {
                            format!("\x1b[38;5;250m {:<dw$}\x1b[0m", truncated, dw = count_w)
                        } else {
                            format!("\x1b[38;5;238m {:<dw$}\x1b[0m", truncated, dw = count_w)
                        }
                    };
                    format!("{}{}{}", ind_col, name_col, count_col)
                }
                r if r == 2 + n => " ".repeat(w),
                r if r == 3 + n => {
                    let hint = " [↑↓] Navigate  [↵] Open  [Esc] Close ";
                    let pad  = w.saturating_sub(hint.len()) / 2;
                    format!("\x1b[2m\x1b[38;5;240m{}{:<width$}\x1b[0m",
                        " ".repeat(pad), hint, width = w.saturating_sub(pad))
                }
                _ => " ".repeat(w),
            }
        });

        out
    }

    // ── Level 2: visualizer list within a category ────────────────────────────

    fn render_visualizer_level(&self, category_idx: usize, cursor: usize,
                                base: &[String], size: TermSize) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let (cat_name, vizs) = &self.categories[category_idx];
        let n = vizs.len();

        let box_h   = (n + 4).min(rows.saturating_sub(2));
        let box_w   = (62usize).min(cols.saturating_sub(4));
        let box_row = (rows.saturating_sub(box_h)) / 2;
        let box_col = (cols.saturating_sub(box_w)) / 2;
        let inner_w = box_w.saturating_sub(2);

        let mut out: Vec<String> = base.to_vec();
        while out.len() < rows { out.push(" ".repeat(cols)); }

        let active_name = &self.active_name;
        let display_cat = Self::capitalize(cat_name);

        Self::draw_box(&mut out, rows, cols, box_row, box_col, box_h, box_w, inner_w, |inner_row, w| {
            match inner_row {
                0 => {
                    let title = format!("[ {} ]", display_cat.to_uppercase());
                    let pad = w.saturating_sub(title.len()) / 2;
                    format!("\x1b[1m\x1b[38;5;255m{}{:<width$}\x1b[0m",
                        " ".repeat(pad), title, width = w.saturating_sub(pad))
                }
                1 => " ".repeat(w),
                r if r >= 2 && r < 2 + n => {
                    let idx      = r - 2;
                    let focused  = idx == cursor;
                    let active   = vizs[idx].0 == *active_name;
                    let name     = &vizs[idx].0;
                    let desc     = &vizs[idx].1;
                    let name_w   = 12usize;
                    let desc_w   = w.saturating_sub(name_w + 4);

                    let indicator = if active && focused { "▶●" }
                                    else if active       { " ●" }
                                    else if focused      { "▶ " }
                                    else                 { "  " };

                    let ind_col = if focused {
                        format!("\x1b[1m\x1b[38;5;33m{}\x1b[0m ", indicator)
                    } else {
                        format!("\x1b[38;5;238m{}\x1b[0m ", indicator)
                    };
                    let name_col = if focused {
                        format!("\x1b[1m\x1b[38;5;33m{:<nw$}\x1b[0m", name, nw = name_w)
                    } else if active {
                        format!("\x1b[38;5;255m{:<nw$}\x1b[0m", name, nw = name_w)
                    } else {
                        format!("\x1b[38;5;244m{:<nw$}\x1b[0m", name, nw = name_w)
                    };
                    let desc_col = {
                        let truncated: String = desc.chars().take(desc_w).collect();
                        if focused {
                            format!("\x1b[38;5;250m {:<dw$}\x1b[0m", truncated, dw = desc_w)
                        } else {
                            format!("\x1b[38;5;238m {:<dw$}\x1b[0m", truncated, dw = desc_w)
                        }
                    };
                    format!("{}{}{}", ind_col, name_col, desc_col)
                }
                r if r == 2 + n => " ".repeat(w),
                r if r == 3 + n => {
                    let hint = " [↑↓] Navigate  [↵] Switch  [Esc] Back ";
                    let pad  = w.saturating_sub(hint.len()) / 2;
                    format!("\x1b[2m\x1b[38;5;240m{}{:<width$}\x1b[0m",
                        " ".repeat(pad), hint, width = w.saturating_sub(pad))
                }
                _ => " ".repeat(w),
            }
        });

        out
    }

    fn capitalize(s: &str) -> String {
        let mut c = s.chars();
        match c.next() {
            None    => String::new(),
            Some(f) => f.to_uppercase().to_string() + c.as_str(),
        }
    }
}


/// Construct a fresh visualizer instance by name, loading its persisted config.
fn make_viz(name: &str, device_name: &str) -> Box<dyn Visualizer> {
    let mut v: Box<dyn Visualizer> = match name {
        "spectrum"  => Box::new(visualizers::spectrum ::SpectrumViz ::new(device_name)),
        "scope"     => Box::new(visualizers::scope    ::ScopeViz    ::new(device_name)),
        "matrix"    => Box::new(visualizers::matrix   ::MatrixViz   ::new(device_name)),
        "radial"    => Box::new(visualizers::radial   ::RadialViz   ::new(device_name)),
        "lissajous" => Box::new(visualizers::lissajous::LissajousViz::new(device_name)),
        "fire"      => Box::new(visualizers::fire     ::FireViz     ::new(device_name)),
        "vu"        => Box::new(visualizers::vu       ::VuViz       ::new(device_name)),
        _ => {
            // Fall back to registry instance (handles build.rs-discovered visualizers)
            let all = visualizers::all_visualizers();
            all.into_iter().find(|v| v.name() == name)
                .unwrap_or_else(|| Box::new(visualizers::spectrum::SpectrumViz::new(device_name)))
        }
    };
    load_and_apply_config(&mut v);
    v
}

/// Strip ANSI escape sequences from a string slice, returning the visible text.
fn strip_ansi(s: &str) -> String { strip_ansi_str(s) }

fn strip_ansi_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // skip until 'm'
            for ch in chars.by_ref() {
                if ch == 'm' { break; }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let cli = Cli::parse();

    let all_vizs = visualizers::all_visualizers();

    if cli.list {
        println!("Available visualizers:");
        for v in &all_vizs {
            println!("  {:12}  {}", v.name(), v.description());
        }
        return Ok(());
    }

    stderr_silence::suppress();
    let host = select_host();

    if cli.list_devices {
        println!("Available input devices (host: {}):", host.id().name());
        for (i, d) in host.input_devices()?.enumerate() {
            println!("  [{}] {}", i, d.name().unwrap_or_else(|_| "?".into()));
        }
        #[cfg(target_os = "linux")]
        if !pulse_device_present(&host) {
            diag!("");
            diag!("WARNING: The ALSA PulseAudio plugin (\"pulse\" device) was not found.");
            diag!("         System audio capture will not work without it.");
            diag!("         Install with: sudo apt install libasound2-plugins");
        }
        return Ok(());
    }

    let device = match &cli.device {
        Some(name) => find_device_by_name(&host, name)
            .ok_or_else(|| anyhow::anyhow!("Device not found: {name}\nRun --list-devices to see available devices."))?,
        None => {
            #[cfg(target_os = "linux")]
            let monitor = prepare_pulse_env(&host)?;
            #[cfg(target_os = "linux")]
            diag!("audio: monitor source → {monitor}");

            find_best_device(&host)
                .ok_or_else(|| anyhow::anyhow!(
                    "No suitable input device found.\n\
                     On macOS install BlackHole: https://existential.audio/blackhole/\n\
                     Use --list-devices to see what is available."
                ))?
        }
    };

    let device_name = device.name().unwrap_or_else(|_| "unknown".into());

    let config = {
        let preferred = cpal::StreamConfig {
            channels:    CHANNELS as u16,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };
        let supported = device
            .supported_input_configs()
            .map(|mut it| it.any(|c| {
                c.sample_format() == cpal::SampleFormat::F32
                    && (c.channels() as usize == CHANNELS || c.channels() >= 1)
            }))
            .unwrap_or(false);
        if supported { preferred } else { device.default_input_config()?.into() }
    };

    let actual_channels = config.channels as usize;

    let ring  = make_ring();
    let ring2 = Arc::clone(&ring);

    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _| {
            let mut buf = ring2.lock().unwrap();
            for frame in data.chunks(actual_channels) {
                if actual_channels >= 2 {
                    buf.push(frame[0]);
                    buf.push(frame[1]);
                } else {
                    buf.push(frame[0]);
                    buf.push(frame[0]);
                }
            }
            const MAX_RING: usize = FFT_SIZE * CHANNELS * 8;
            if buf.len() > MAX_RING {
                let drain = buf.len() - MAX_RING;
                buf.drain(0..drain);
            }
        },
        |err| eprintln!("[audio error] {err}"),
        None,
    )?;
    stream.play()?;

    // ── Select and initialise visualizer ──────────────────────────────────────
    let viz_name = cli.visualizer.to_lowercase();
    let mut viz: Box<dyn Visualizer> = {
        let found = all_vizs.iter().any(|v| v.name() == viz_name);
        if !found {
            eprintln!("Unknown visualizer '{viz_name}'.");
            eprintln!("Available: {}", all_vizs.iter().map(|v| v.name()).collect::<Vec<_>>().join(", "));
            std::process::exit(1);
        }
        match viz_name.as_str() {
            "spectrum"  => Box::new(visualizers::spectrum ::SpectrumViz ::new(&device_name)),
            "scope"     => Box::new(visualizers::scope    ::ScopeViz    ::new(&device_name)),
            "matrix"    => Box::new(visualizers::matrix   ::MatrixViz   ::new(&device_name)),
            "radial"    => Box::new(visualizers::radial   ::RadialViz   ::new(&device_name)),
            "lissajous" => Box::new(visualizers::lissajous::LissajousViz::new(&device_name)),
            "fire"      => Box::new(visualizers::fire     ::FireViz     ::new(&device_name)),
            "vu"        => Box::new(visualizers::vu       ::VuViz       ::new(&device_name)),
            _ => all_vizs.into_iter().find(|v| v.name() == viz_name).unwrap(),
        }
    };

    // ── Load persisted config ─────────────────────────────────────────────────
    load_and_apply_config(&mut viz);

    // ── Terminal setup ────────────────────────────────────────────────────────
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        cursor::Hide,
        terminal::Clear(ClearType::All),
    )?;

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        }
    }
    let _guard = Guard;

    // ── FFT setup ─────────────────────────────────────────────────────────────
    let window  = hann_window(FFT_SIZE);
    let mut planner = FftPlanner::<f32>::new();

    let mut mono_window:  Vec<f32> = vec![0.0; FFT_SIZE];
    let mut left_window:  Vec<f32> = vec![0.0; FFT_SIZE];
    let mut right_window: Vec<f32> = vec![0.0; FFT_SIZE];

    // ── Render loop ───────────────────────────────────────────────────────────
    let frame_duration = Duration::from_secs_f32(1.0 / cli.fps);
    let mut fps_display = cli.fps;
    const FPS_ALPHA: f32 = 0.08;

    let mut size = term_size();
    viz.on_resize(size);

    let mut t_prev = Instant::now();

    // Settings overlay state: None = closed, Some = open
    let mut overlay: Option<SettingsOverlay> = None;
    // Visualizer picker state: None = closed, Some = open
    let mut viz_picker: Option<VizPicker> = None;
    // Pre-build category data (names + descriptions) for the two-level picker.
    let viz_categories: Vec<(String, Vec<(String, String)>)> = {
        let all_desc = visualizers::all_visualizers();
        visualizers::visualizer_categories()
            .into_iter()
            .map(|(cat, names)| {
                let entries = names
                    .into_iter()
                    .map(|n| {
                        let desc = all_desc
                            .iter()
                            .find(|v| v.name() == n)
                            .map(|v| v.description().to_string())
                            .unwrap_or_default();
                        (n.to_string(), desc)
                    })
                    .collect();
                (cat.to_string(), entries)
            })
            .collect()
    };
    // Cache the last rendered visualizer frame for compositing with the overlay
    let mut last_frame: Vec<String> = Vec::new();

    loop {
        let t0 = Instant::now();

        // ── Poll events ───────────────────────────────────────────────────────
        while event::poll(Duration::ZERO)? {
            match event::read()? {
                // ── Global: quit ──────────────────────────────────────────────
                Event::Key(KeyEvent { code: KeyCode::Char('q'), modifiers: KeyModifiers::NONE, .. })
                | Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers: KeyModifiers::CONTROL, .. })
                    if overlay.is_none() && viz_picker.is_none() =>
                {
                    return Ok(());
                }

                // ── Global: toggle settings overlay (F1) ─────────────────────
                Event::Key(KeyEvent { code: KeyCode::F(1), .. })
                    if viz_picker.is_none() =>
                {
                    if overlay.is_none() {
                        overlay = SettingsOverlay::open(&viz);
                    } else {
                        overlay = None;
                    }
                }

                // ── Global: load default config (F2) ──────────────────────────
                Event::Key(KeyEvent { code: KeyCode::F(2), .. })
                    if overlay.is_none() && viz_picker.is_none() =>
                {
                    let default_json = viz.get_default_config();
                    if let Ok(cleaned) = viz.set_config(&default_json) {
                        let _ = write_config(viz.name(), &cleaned);
                    }
                    viz.on_resize(size);
                }

                // ── Global: open visualizer picker (Esc when nothing else open) ─
                Event::Key(KeyEvent { code: KeyCode::Esc, .. })
                    if overlay.is_none() && viz_picker.is_none() =>
                {
                    viz_picker = Some(VizPicker::open(&viz_categories, viz.name()));
                }

                // ── Visualizer picker navigation ──────────────────────────────
                Event::Key(kev) if viz_picker.is_some() && overlay.is_none() => {
                    let vp = viz_picker.as_mut().unwrap();
                    match kev.code {
                        KeyCode::Esc => {
                            if vp.escape() { viz_picker = None; }
                        }
                        KeyCode::Up   => { vp.up(); }
                        KeyCode::Down => { vp.down(); }
                        KeyCode::Enter => {
                            if let Some(chosen) = vp.enter() {
                                viz_picker = None;
                                if chosen != viz.name() {
                                    viz = make_viz(&chosen, &device_name);
                                    viz.on_resize(size);
                                    execute!(stdout, terminal::Clear(ClearType::All))?;
                                    last_frame.clear();
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // ── Terminal resize ───────────────────────────────────────────
                Event::Resize(cols, rows) if overlay.is_none() => {
                    size = TermSize { rows, cols };
                    viz.on_resize(size);
                    execute!(stdout, terminal::Clear(ClearType::All))?;
                }

                // ── Overlay navigation ────────────────────────────────────────
                Event::Key(kev) if overlay.is_some() => {
                    let mut close_overlay = false;
                    {
                    let ov = overlay.as_mut().unwrap();
                    let n  = ov.entries.len();

                    match kev.code {
                        // Esc: close popup if open, otherwise close overlay
                        KeyCode::Esc => {
                            if ov.popup.is_some() {
                                ov.popup = None;
                            } else {
                                close_overlay = true;
                            }
                        }

                        // Enter: confirm popup variant / confirm typed value / save & close
                        KeyCode::Enter => {
                            if let Some(hi) = ov.popup {
                                // Popup open: confirm the highlighted variant, close popup
                                let variant = ov.entries[ov.cursor].variants.get(hi).cloned();
                                if let Some(v) = variant {
                                    ov.entries[ov.cursor].value = EntryValue::Enum(v);
                                }
                                ov.popup = None;
                            } else if ov.editing {
                                // Numeric field: commit the typed value, stay in overlay
                                ov.commit_text();
                                ov.editing = false;
                            } else {
                                // Nothing pending — save & close
                                let partial = ov.to_partial_json();
                                match viz.set_config(&partial) {
                                    Ok(cleaned) => {
                                        let _ = write_config(viz.name(), &cleaned);
                                        close_overlay = true;
                                    }
                                    Err(e) => {
                                        ov.err_msg = Some(e);
                                    }
                                }
                            }
                        }
                        KeyCode::Char('s') => {
                            // Always save & close (commit any pending text first).
                            ov.commit_text();
                            ov.editing = false;
                            if ov.err_msg.is_none() {
                                let partial = ov.to_partial_json();
                                match viz.set_config(&partial) {
                                    Ok(cleaned) => {
                                        let _ = write_config(viz.name(), &cleaned);
                                        close_overlay = true;
                                    }
                                    Err(e) => {
                                        ov.err_msg = Some(e);
                                    }
                                }
                            }
                        }

                        // Move cursor up / down (or navigate popup when it is open)
                        KeyCode::Up => {
                            if let Some(ref mut hi) = ov.popup {
                                // Navigate the enum popup
                                let nv = ov.entries[ov.cursor].variants.len();
                                if *hi > 0 { *hi -= 1; } else { *hi = nv.saturating_sub(1); }
                            } else {
                                ov.text_buf.clear();
                                ov.editing = false;
                                ov.err_msg = None;
                                if ov.cursor > 0 { ov.cursor -= 1; } else { ov.cursor = n.saturating_sub(1); }
                            }
                        }
                        KeyCode::Down => {
                            if let Some(ref mut hi) = ov.popup {
                                let nv = ov.entries[ov.cursor].variants.len();
                                *hi = (*hi + 1) % nv.max(1);
                            } else {
                                ov.text_buf.clear();
                                ov.editing = false;
                                ov.err_msg = None;
                                if ov.cursor + 1 < n { ov.cursor += 1; }
                            }
                        }

                        // Space: universal "Edit" key.
                        // Bool   — toggle the value directly.
                        // Enum   — open the browse popup (or confirm if already open).
                        // Float/Int — enter text-edit mode, clearing any previous input.
                        KeyCode::Char(' ') => {
                            let kind = ov.entries.get(ov.cursor)
                                .map(|e| e.kind.as_str())
                                .unwrap_or("");
                            match kind {
                                "bool" => {
                                    if let EntryValue::Bool(v) = &mut ov.entries[ov.cursor].value {
                                        *v = !*v;
                                    }
                                }
                                "enum" => {
                                    if let Some(hi) = ov.popup {
                                        // Popup open: confirm highlighted variant
                                        let variant = ov.entries[ov.cursor].variants.get(hi).cloned();
                                        if let Some(v) = variant {
                                            ov.entries[ov.cursor].value = EntryValue::Enum(v);
                                        }
                                        ov.popup = None;
                                    } else {
                                        // Open popup at current variant
                                        let current_idx = match &ov.entries[ov.cursor].value {
                                            EntryValue::Enum(s) => ov.entries[ov.cursor].variants
                                                .iter().position(|v| v == s).unwrap_or(0),
                                            _ => 0,
                                        };
                                        ov.popup = Some(current_idx);
                                    }
                                }
                                "float" | "int" => {
                                    // Begin text-edit mode with a blank buffer
                                    ov.text_buf.clear();
                                    ov.editing = true;
                                    ov.err_msg = None;
                                }
                                _ => {}
                            }
                        }

                        KeyCode::Left | KeyCode::Right => {
                            let kind = ov.entries.get(ov.cursor)
                                .map(|e| e.kind.as_str())
                                .unwrap_or("");

                            if kind == "bool" {
                                if let EntryValue::Bool(v) = &mut ov.entries[ov.cursor].value {
                                    *v = !*v;
                                }
                            } else if kind == "enum" {
                                let nv = ov.entries[ov.cursor].variants.len();
                                if nv == 0 { /* nothing */ } else if let Some(ref mut hi) = ov.popup {
                                    // Popup open: move highlight within popup
                                    if kev.code == KeyCode::Right {
                                        *hi = (*hi + 1) % nv;
                                    } else {
                                        *hi = if *hi == 0 { nv - 1 } else { *hi - 1 };
                                    }
                                } else {
                                    // Popup closed: cycle the value directly
                                    let current_idx = match &ov.entries[ov.cursor].value {
                                        EntryValue::Enum(s) => ov.entries[ov.cursor].variants
                                            .iter().position(|v| v == s).unwrap_or(0),
                                        _ => 0,
                                    };
                                    let next_idx = if kev.code == KeyCode::Right {
                                        (current_idx + 1) % nv
                                    } else {
                                        if current_idx == 0 { nv - 1 } else { current_idx - 1 }
                                    };
                                    let new_val = ov.entries[ov.cursor].variants[next_idx].clone();
                                    ov.entries[ov.cursor].value = EntryValue::Enum(new_val);
                                }
                            } else {
                                // Numeric nudge
                                ov.commit_text();
                                let delta = if kev.code == KeyCode::Right { 1.0 } else { -1.0 };
                                ov.nudge(delta);
                            }
                        }

                        // Confirm enum popup selection with Enter (already handled above for save;
                        // here we handle the case when the popup is open and the user presses Enter
                        // to select a variant without saving).
                        // Re-use the Enter arm above — it calls commit_text (no-op for enum)
                        // then saves.  For a "select without save" feel, we intercept here:
                        // (leave full save to the Enter arm above)

                        // Backspace: delete last char of text buffer
                        KeyCode::Backspace => {
                            ov.err_msg = None;
                            ov.text_buf.pop();
                            if ov.text_buf.is_empty() { ov.editing = false; }
                        }

                        // Digit / decimal / minus input for numeric fields
                        KeyCode::Char(c) => {
                            let is_numeric = ov.entries.get(ov.cursor)
                                .map_or(false, |e| e.kind == "float" || e.kind == "int");
                            if is_numeric && (c.is_ascii_digit() || c == '.' || c == '-') {
                                ov.err_msg = None;
                                ov.text_buf.push(c);
                                ov.editing = true;
                            }

                        }

                        _ => {}
                    }
                    } // end borrow of overlay
                    if close_overlay { overlay = None; }
                }

                _ => {}
            }
        }

        // Also poll size directly in case resize events were missed
        let current_size = term_size();
        if current_size != size && overlay.is_none() {
            size = current_size;
            viz.on_resize(size);
            execute!(stdout, terminal::Clear(ClearType::All))?;
        }

        // ── Drain ring buffer → sliding sample windows ────────────────────────
        {
            let mut buf = ring.lock().unwrap();
            if !buf.is_empty() {
                let n_pairs = buf.len() / 2;
                let take    = n_pairs.min(FFT_SIZE);
                let keep    = FFT_SIZE - take;

                left_window .copy_within(take.., 0);
                right_window.copy_within(take.., 0);
                mono_window .copy_within(take.., 0);

                let start_pair = n_pairs.saturating_sub(take);
                for i in 0..take {
                    let pair_idx = (start_pair + i) * 2;
                    if pair_idx + 1 < buf.len() {
                        let l = buf[pair_idx];
                        let r = buf[pair_idx + 1];
                        left_window [keep + i] = l;
                        right_window[keep + i] = r;
                        mono_window [keep + i] = (l + r) * 0.5;
                    }
                }
                buf.clear();
            }
        }

        // ── Compute FFT ───────────────────────────────────────────────────────
        let fft_out = compute_fft(&mono_window, &window, &mut planner);

        // ── dt ───────────────────────────────────────────────────────────────
        let dt = {
            let now  = Instant::now();
            let secs = (now - t_prev).as_secs_f32().clamp(1e-4, 0.15);
            t_prev   = now;
            secs
        };

        // ── Tick visualizer (freeze frame while any overlay is visible) ──────
        if overlay.is_none() && viz_picker.is_none() {
            let frame = AudioFrame {
                left:        left_window.clone(),
                right:       right_window.clone(),
                mono:        mono_window.clone(),
                fft:         fft_out,
                sample_rate: SAMPLE_RATE,
            };
            viz.tick(&frame, dt, size);
            last_frame = viz.render(size, fps_display);
        }

        // ── Compose and write frame ───────────────────────────────────────────
        let to_draw: Vec<String> = if let Some(ov) = &overlay {
            ov.render_over(&last_frame, size)
        } else if let Some(vp) = &viz_picker {
            vp.render_over(&last_frame, size)
        } else {
            last_frame.clone()
        };

        execute!(stdout, cursor::MoveTo(0, 0))?;
        let rows = size.rows as usize;
        for (i, line) in to_draw.iter().take(rows).enumerate() {
            execute!(
                stdout,
                Print(line),
                terminal::Clear(ClearType::UntilNewLine),
            )?;
            if i + 1 < rows {
                execute!(stdout, Print("\r\n"))?;
            }
        }
        stdout.flush()?;

        // ── FPS tracking ──────────────────────────────────────────────────────
        let elapsed  = t0.elapsed();
        let inst_fps = 1.0 / elapsed.as_secs_f32().max(1e-6);
        fps_display  = FPS_ALPHA * inst_fps + (1.0 - FPS_ALPHA) * fps_display;

        if let Some(sleep) = frame_duration.checked_sub(elapsed) {
            std::thread::sleep(sleep);
        }
    }
}
