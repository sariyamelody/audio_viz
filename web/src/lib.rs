/// web/src/lib.rs — WebAssembly entry point for audio_viz.
///
/// Exposes a `WebViz` handle to JavaScript.  Each frame, JS calls
/// `tick(fft, left, right, dt)` and then `render(cols, rows)` which returns a
/// flat JSON array of cell objects:
///
///   [{ ch, col, row, r, g, b, bold }, ...]
///
/// The JS canvas renderer iterates this array and draws each character with
/// the correct RGB colour.  Only non-space cells are included so the array is
/// sparse — the canvas is cleared to black before each frame.

use wasm_bindgen::prelude::*;

use audio_viz::visualizer::{AudioFrame, TermSize, Visualizer, FFT_SIZE, SAMPLE_RATE};
use audio_viz::visualizers;

// ── xterm-256 → RGB lookup ────────────────────────────────────────────────────
//
// The visualizer render methods emit 256-colour ANSI codes.  We need to map
// those colour indices to RGB so the canvas renderer can use them.
//
// The 256-colour palette is structured as:
//   0-15:   system colours (we use standard xterm defaults)
//   16-231: 6×6×6 colour cube
//   232-255: greyscale ramp

fn xterm256_to_rgb(idx: u8) -> (u8, u8, u8) {
    match idx {
        // System colours (xterm defaults)
        0  => (0,   0,   0),
        1  => (128, 0,   0),
        2  => (0,   128, 0),
        3  => (128, 128, 0),
        4  => (0,   0,   128),
        5  => (128, 0,   128),
        6  => (0,   128, 128),
        7  => (192, 192, 192),
        8  => (128, 128, 128),
        9  => (255, 0,   0),
        10 => (0,   255, 0),
        11 => (255, 255, 0),
        12 => (0,   0,   255),
        13 => (255, 0,   255),
        14 => (0,   255, 255),
        15 => (255, 255, 255),
        // 6×6×6 colour cube: indices 16–231
        16..=231 => {
            let n  = idx - 16;
            let b  = n % 6;
            let g  = (n / 6) % 6;
            let r  = n / 36;
            let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (lv(r), lv(g), lv(b))
        }
        // Greyscale ramp: indices 232–255
        232..=255 => {
            let v = 8 + (idx - 232) * 10;
            (v, v, v)
        }
    }
}

// ── ANSI parser ───────────────────────────────────────────────────────────────
//
// Parses the ANSI-escaped strings returned by render() into a flat list of
// positioned cells, each carrying an RGB colour and a bold flag.

#[derive(serde::Serialize)]
struct Cell {
    ch:   String,
    col:  u32,
    row:  u32,
    r:    u8,
    g:    u8,
    b:    u8,
    bold: bool,
    dim:  bool,
}

fn parse_frame(lines: &[String]) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(lines.len() * 40);

    for (row_idx, line) in lines.iter().enumerate() {
        let mut col  = 0u32;
        let mut fg   = (192u8, 192u8, 192u8); // default: light grey
        let mut bold = false;
        let mut dim  = false;

        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '[' {
                // Parse the CSI sequence: ESC [ <params> m
                i += 2;
                let mut params = String::new();
                while i < chars.len() && chars[i] != 'm' {
                    params.push(chars[i]);
                    i += 1;
                }
                i += 1; // consume 'm'

                // Process each semicolon-separated parameter
                let parts: Vec<&str> = params.split(';').collect();
                let mut pi = 0;
                while pi < parts.len() {
                    match parts[pi].parse::<u32>().unwrap_or(0) {
                        0  => { bold = false; dim = false; fg = (192, 192, 192); }
                        1  => { bold = true; }
                        2  => { dim = true; }
                        // 38;5;n — 256-colour foreground
                        38 if pi + 2 < parts.len() && parts[pi + 1] == "5" => {
                            let idx = parts[pi + 2].parse::<u8>().unwrap_or(7);
                            fg = xterm256_to_rgb(idx);
                            pi += 2;
                        }
                        _ => {}
                    }
                    pi += 1;
                }
            } else {
                let ch = chars[i];
                i += 1;
                if ch != ' ' {
                    // Apply bold brightness boost and dim reduction
                    let (mut r, mut g, mut b) = fg;
                    if bold {
                        r = r.saturating_add(40);
                        g = g.saturating_add(40);
                        b = b.saturating_add(40);
                    }
                    if dim {
                        r = (r as u16 * 55 / 100) as u8;
                        g = (g as u16 * 55 / 100) as u8;
                        b = (b as u16 * 55 / 100) as u8;
                    }
                    cells.push(Cell {
                        ch: ch.to_string(),
                        col,
                        row: row_idx as u32,
                        r, g, b,
                        bold,
                        dim,
                    });
                }
                col += 1;
            }
        }
    }

    cells
}

// ── WebViz handle ─────────────────────────────────────────────────────────────

#[wasm_bindgen]
pub struct WebViz {
    viz:  Box<dyn Visualizer>,
    size: TermSize,
}

#[wasm_bindgen]
impl WebViz {
    /// Construct a new WebViz for the named visualizer.
    /// Valid names: "spectrum", "scope", "matrix", "radial", "lissajous", "fire", "vu"
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str, cols: u16, rows: u16) -> WebViz {
        // getrandom/rand need this hook on WASM
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook_init();

        let size = TermSize { cols, rows };
        let mut viz = make_viz(name);
        viz.on_resize(size);

        WebViz { viz, size }
    }

    /// Update the terminal dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.size = TermSize { cols, rows };
        self.viz.on_resize(self.size);
    }

    /// Advance the visualizer by one frame.
    ///
    /// `fft_ptr` and `fft_len` point to the magnitude spectrum (FFT_SIZE/2+1 floats).
    /// `left_ptr`/`right_ptr` point to FFT_SIZE raw audio samples each.
    /// `dt` is elapsed seconds since the last tick.
    pub fn tick(
        &mut self,
        fft:   &[f32],
        left:  &[f32],
        right: &[f32],
        dt:    f32,
    ) {
        // Pad or truncate to FFT_SIZE
        let mut l = left.to_vec();
        let mut r = right.to_vec();
        let mut m: Vec<f32> = l.iter().zip(r.iter()).map(|(a, b)| (a + b) * 0.5).collect();
        l.resize(FFT_SIZE, 0.0);
        r.resize(FFT_SIZE, 0.0);
        m.resize(FFT_SIZE, 0.0);

        let frame = AudioFrame {
            left:        l,
            right:       r,
            mono:        m,
            fft:         fft.to_vec(),
            sample_rate: SAMPLE_RATE,
        };
        self.viz.tick(&frame, dt, self.size);
    }

    /// Render the current frame and return a JSON string of cell objects.
    pub fn render(&self, fps: f32) -> String {
        let lines = self.viz.render(self.size, fps);
        let cells = parse_frame(&lines);
        serde_json::to_string(&cells).unwrap_or_else(|_| "[]".to_string())
    }

    /// Return the list of all visualizer names as a JSON array.
    pub fn all_names() -> String {
        let vizs = visualizers::all_visualizers();
        let names: Vec<&str> = vizs.iter().map(|v| v.name()).collect();
        serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
    }

    /// Return the default config JSON for this visualizer.
    pub fn get_config(&self) -> String {
        self.viz.get_default_config()
    }

    /// Apply a (possibly partial) config JSON and return the merged result.
    pub fn set_config(&mut self, json: &str) -> String {
        self.viz.set_config(json).unwrap_or_default()
    }

    /// Return the name of the active visualizer.
    pub fn name(&self) -> String {
        self.viz.name().to_string()
    }
}

fn make_viz(name: &str) -> Box<dyn Visualizer> {
    use audio_viz::visualizers::*;
    match name {
        "spectrum"  => Box::new(spectrum ::SpectrumViz ::new("microphone")),
        "scope"     => Box::new(scope    ::ScopeViz    ::new("microphone")),
        "matrix"    => Box::new(matrix   ::MatrixViz   ::new("microphone")),
        "radial"    => Box::new(radial   ::RadialViz   ::new("microphone")),
        "lissajous" => Box::new(lissajous::LissajousViz::new("microphone")),
        "fire"      => Box::new(fire     ::FireViz     ::new("microphone")),
        "vu"        => Box::new(vu       ::VuViz       ::new("microphone")),
        _           => Box::new(spectrum ::SpectrumViz ::new("microphone")),
    }
}

#[cfg(target_arch = "wasm32")]
fn console_error_panic_hook_init() {
    // Sets a panic hook that forwards panics to the browser console.
    // This is a no-op after the first call.
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let msg = info.to_string();
            web_sys_log(&msg);
        }));
    });
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_sys_log(s: &str);
}
