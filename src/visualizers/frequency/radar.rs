/// radar.rs — Rotating radar sweep with phosphor persistence.
///
/// A sweep arm rotates continuously around the centre.  As it passes each
/// angle it reads the FFT and writes brightness into a per-cell persistence
/// buffer that decays exponentially, producing the classic phosphor afterglow.
///
/// Two display modes:
///   frequency (default) — angle = frequency, radius = energy
///       12 o'clock = bass, clockwise through mids to highs
///   time                — angle = time, radius = frequency
///       Each sweep paints the full spectrum as a radial stripe;
///       rotation accumulates a polar spectrogram
///
/// Config:
///   gain         — 0–4:            FFT amplitude multiplier
///   sweep_speed  — 0.1–2:          rotations per second
///   persistence  — 0.1–3:          decay rate (low = long trail)
///   color_scheme — phosphor / amber / neon / spectrum
///   rings        — bool:           concentric range rings
///   full_screen  — bool:           fill terminal; no circular crop
///   labels       — bool:           frequency labels at compass points / radii
///   mode         — frequency/time  display geometry

use std::f32::consts::PI;
use crate::visualizer::{
    merge_config, pad_frame, specgrad, status_bar,
    AudioFrame, SpectrumBars, TermSize, Visualizer,
};

const CONFIG_VERSION: u64 = 1;
/// One frequency bar per degree.
const N_BARS: usize = 360;

// ── Colour ────────────────────────────────────────────────────────────────────

fn radar_color(frac: f32, brightness: f32, scheme: &str) -> u8 {
    let b = brightness.clamp(0.0, 1.0);
    match scheme {
        "phosphor" => {
            const G: &[u8] = &[22, 28, 34, 40, 46, 82, 118, 154, 190, 226];
            G[((b * 9.0) as usize).min(9)]
        }
        "amber" => {
            const A: &[u8] = &[52, 58, 94, 130, 136, 172, 178, 214, 220, 226];
            A[((b * 9.0) as usize).min(9)]
        }
        "neon" => {
            const N: &[u8] = &[54, 57, 93, 129, 165, 201, 200, 199, 198, 231];
            N[((b * 9.0) as usize).min(9)]
        }
        _ /* spectrum */ => specgrad(frac),
    }
}

/// Smallest angular distance between two angles, both in [0, 2π).
fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % (2.0 * PI);
    if d > PI { 2.0 * PI - d } else { d }
}

// ── Label helpers ─────────────────────────────────────────────────────────────

fn push_text(
    row: f32, col_start: f32, text: &str,
    vis: usize, cols: usize,
    out: &mut Vec<(usize, usize, char)>,
) {
    for (i, ch) in text.chars().enumerate() {
        let r = row.round() as isize;
        let c = (col_start + i as f32).round() as isize;
        if r >= 0 && r < vis as isize && c >= 0 && c < cols as isize {
            out.push((r as usize, c as usize, ch));
        }
    }
}

/// Build a list of (row, col, char) for frequency labels.
///
/// frequency mode: labels at the 4 cardinal compass points just outside the
///   circle.  Frequencies are computed for N_BARS=360 log-spaced 30–18 kHz:
///     0°=30 Hz, 90°≈150 Hz, 180°≈740 Hz, 270°≈3.6 kHz.
///
/// time mode: labels along the southward radial axis at depth proportional to
///   log-frequency position, just to the right of centre.
fn build_label_cells(
    cy: f32, cx: f32, maxr: f32,
    mode: &str, vis: usize, cols: usize,
) -> Vec<(usize, usize, char)> {
    let mut cells = Vec::new();

    if mode == "time" {
        // r_frac = log(f/30) / log(600) for each target frequency.
        const LABELS: &[(&str, f32)] = &[
            ("30Hz",  0.04),
            ("250Hz", 0.33),
            ("1kHz",  0.55),
            ("5kHz",  0.80),
        ];
        for &(text, r_frac) in LABELS {
            // Southward axis: row increases with t.
            let row = cy + r_frac * maxr;
            let col = cx + 1.5;
            push_text(row, col, text, vis, cols, &mut cells);
        }
    } else {
        // Cardinal compass labels, placed 1.5 row-units outside the circle.
        const LABELS: &[(&str, f32)] = &[
            ("30Hz",   0.0),
            ("150Hz",  PI * 0.5),
            ("740Hz",  PI),
            ("3.6kHz", PI * 1.5),
        ];
        let gap = maxr + 1.5;
        for &(text, angle) in LABELS {
            // Centre-align the label at the compass position.
            let row = cy - angle.cos() * gap;
            let col = cx + angle.sin() * gap * 2.0 - text.len() as f32 * 0.5;
            push_text(row, col, text, vis, cols, &mut cells);
        }
    }

    cells
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct RadarViz {
    bars:         SpectrumBars,
    source:       String,
    /// Phosphor persistence buffer — brightness [0..1].  Shape: vis_rows × cols.
    screen:       Vec<Vec<f32>>,
    /// Frequency fraction [0..1] per lit cell (drives spectrum color mode).
    screen_frac:  Vec<Vec<f32>>,
    cached_rows:  usize,
    cached_cols:  usize,
    /// Current sweep angle in [0, 2π).  0 = 12 o'clock, clockwise.
    scan_angle:   f32,
    // ── config ────────────────────────────────────────────────────────────────
    gain:         f32,
    sweep_speed:  f32,
    persistence:  f32,
    color_scheme: String,
    rings:        bool,
    full_screen:  bool,
    labels:       bool,
    mode:         String,
}

impl RadarViz {
    pub fn new(source: &str) -> Self {
        Self {
            bars:         SpectrumBars::new(N_BARS),
            source:       source.to_string(),
            screen:       Vec::new(),
            screen_frac:  Vec::new(),
            cached_rows:  0,
            cached_cols:  0,
            scan_angle:   0.0,
            gain:         2.0,
            sweep_speed:  0.5,
            persistence:  1.0,
            color_scheme: "phosphor".to_string(),
            rings:        true,
            full_screen:  false,
            labels:       false,
            mode:         "frequency".to_string(),
        }
    }

    fn ensure_buffers(&mut self, rows: usize, cols: usize) {
        if self.cached_rows == rows && self.cached_cols == cols { return; }
        self.screen      = vec![vec![0.0f32; cols]; rows];
        self.screen_frac = vec![vec![0.0f32; cols]; rows];
        self.cached_rows = rows;
        self.cached_cols = cols;
    }

    /// Rasterise the arm at `theta` into the screen buffer.
    ///
    /// frequency mode: writes full brightness for cells where r_frac ≤ bar energy.
    /// time mode:      writes bar energy at each radius (radius = frequency band).
    ///
    /// No faint-beam trail is written — the arm's dim ghost line is drawn as a
    /// render-time overlay in `render()`, which keeps the screen buffer at zero
    /// between sweeps so range rings remain visible through silence.
    fn trace_arm(&mut self, theta: f32, vis_rows: usize, cols: usize) {
        let cy   = vis_rows as f32 / 2.0;
        let cx   = cols as f32 / 2.0;
        let maxr = cy.min(cx * 0.5).max(1.0);
        // In full-screen mode extend the trace to the screen corners.
        let trace_r = if self.full_screen {
            (cy * cy + cx * 0.5 * (cx * 0.5)).sqrt()
        } else {
            maxr
        };

        let cos_t = theta.cos();
        let sin_t = theta.sin();
        // 2 steps per row-unit avoids gaps.
        let steps = (trace_r * 2.0).ceil() as usize + 2;

        if self.mode == "time" {
            for step in 0..=steps {
                let t = step as f32 / steps as f32 * trace_r;
                let r = (cy - t * cos_t).round() as isize;
                let c = (cx + t * sin_t * 2.0).round() as isize;
                if r < 0 || r >= vis_rows as isize { continue; }
                if c < 0 || c >= cols as isize { continue; }
                let r = r as usize;
                let c = c as usize;

                // r_frac clamped so frequency mapping stays within N_BARS.
                let r_frac  = (t / maxr).min(1.0);
                let freq_bi = (r_frac * (N_BARS - 1) as f32) as usize;
                let e       = self.bars.smoothed[freq_bi];
                if e > 0.02 && self.screen[r][c] < e {
                    self.screen[r][c]      = e;
                    self.screen_frac[r][c] = r_frac;
                }
            }
        } else {
            // frequency mode: one energy value covers the full radial slice.
            let bi     = ((theta / (2.0 * PI)) * N_BARS as f32)
                             .rem_euclid(N_BARS as f32) as usize;
            let bi     = bi.min(N_BARS - 1);
            let energy = self.bars.smoothed[bi];
            let frac   = bi as f32 / (N_BARS - 1).max(1) as f32;

            for step in 0..=steps {
                let t = step as f32 / steps as f32 * trace_r;
                let r = (cy - t * cos_t).round() as isize;
                let c = (cx + t * sin_t * 2.0).round() as isize;
                if r < 0 || r >= vis_rows as isize { continue; }
                if c < 0 || c >= cols as isize { continue; }
                let r = r as usize;
                let c = c as usize;

                if t / maxr <= energy {
                    // Signal hit: write at full brightness.
                    if self.screen[r][c] < 1.0 { self.screen[r][c] = 1.0; }
                    self.screen_frac[r][c] = frac;
                }
                // No faint beam — silence leaves the buffer at zero so rings show.
            }
        }
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for RadarViz {
    fn name(&self)        -> &str { "radar" }
    fn description(&self) -> &str { "Rotating sweep with phosphor persistence" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "radar",
            "version": CONFIG_VERSION,
            "config": [
                { "name": "gain",         "display_name": "Gain",         "type": "float", "value": 2.0,  "min": 0.0, "max": 4.0 },
                { "name": "sweep_speed",  "display_name": "Sweep Speed",  "type": "float", "value": 0.5,  "min": 0.1, "max": 2.0 },
                { "name": "persistence",  "display_name": "Persistence",  "type": "float", "value": 1.0,  "min": 0.1, "max": 3.0 },
                {
                    "name": "color_scheme", "display_name": "Color Scheme", "type": "enum",
                    "value": "phosphor", "variants": ["phosphor", "amber", "neon", "spectrum"]
                },
                { "name": "rings",       "display_name": "Range Rings",  "type": "bool", "value": true  },
                { "name": "full_screen", "display_name": "Full Screen",  "type": "bool", "value": false },
                { "name": "labels",      "display_name": "Labels",       "type": "bool", "value": false },
                {
                    "name": "mode", "display_name": "Mode", "type": "enum",
                    "value": "frequency", "variants": ["frequency", "time"]
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
                    "gain"         => { self.gain         = entry["value"].as_f64().unwrap_or(2.0) as f32; }
                    "sweep_speed"  => { self.sweep_speed  = entry["value"].as_f64().unwrap_or(0.5) as f32; }
                    "persistence"  => { self.persistence  = entry["value"].as_f64().unwrap_or(1.0) as f32; }
                    "color_scheme" => { if let Some(s) = entry["value"].as_str() { self.color_scheme = s.to_string(); } }
                    "rings"        => { self.rings        = entry["value"].as_bool().unwrap_or(true); }
                    "full_screen"  => { self.full_screen  = entry["value"].as_bool().unwrap_or(false); }
                    "labels"       => { self.labels       = entry["value"].as_bool().unwrap_or(false); }
                    "mode"         => { if let Some(s) = entry["value"].as_str() { self.mode = s.to_string(); } }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        let vis = (size.rows as usize).saturating_sub(1);
        self.ensure_buffers(vis, size.cols as usize);
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let vis  = (size.rows as usize).saturating_sub(1);
        let cols = size.cols as usize;
        self.ensure_buffers(vis, cols);

        // Spectrum bars: N_BARS fixed, no resize by terminal width.
        if (self.gain - 1.0).abs() > f32::EPSILON {
            let scaled: Vec<f32> = audio.fft.iter().map(|v| v * self.gain).collect();
            self.bars.update(&scaled, dt);
        } else {
            self.bars.update(&audio.fft, dt);
        }

        // Exponential decay: brightness × e^(−persistence × dt).
        let decay = (-self.persistence * dt).exp();
        for row in &mut self.screen {
            for cell in row.iter_mut() {
                *cell *= decay;
            }
        }

        // Advance arm; write data across the full angular range swept this frame.
        let d_theta  = self.sweep_speed * 2.0 * PI * dt;
        let substeps = ((d_theta / (2.0 * PI) * 360.0).ceil() as usize).clamp(1, 20);
        for s in 0..substeps {
            let a = self.scan_angle + d_theta * (s as f32 / substeps as f32);
            self.trace_arm(a, vis, cols);
        }
        self.scan_angle = (self.scan_angle + d_theta).rem_euclid(2.0 * PI);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1);

        let cy   = vis as f32 / 2.0;
        let cx   = cols as f32 / 2.0;
        let maxr = cy.min(cx * 0.5).max(1.0);
        // Arm highlight: leading edge glows within this angular window (~4°).
        let arm_half: f32 = 0.07;

        let label_cells = if self.labels {
            build_label_cells(cy, cx, maxr, &self.mode, vis, cols)
        } else {
            Vec::new()
        };

        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 14);
            for c in 0..cols {
                let dy = r as f32 - cy;
                let dx = (c as f32 - cx) * 0.5; // physical x, aspect-corrected

                // Normalised radius: 1.0 = circle boundary.
                let rn = (dx * dx + dy * dy).sqrt() / maxr;

                // In normal mode clip to the circle; full_screen shows everything.
                if !self.full_screen && rn > 1.02 {
                    line.push(' ');
                    continue;
                }

                // Cell angle: 0 = 12 o'clock, clockwise.
                let theta_raw = dx.atan2(-dy);
                let theta     = if theta_raw < 0.0 { theta_raw + 2.0 * PI } else { theta_raw };

                // Phosphor buffer.
                let sb    = if r < self.screen.len() && c < self.screen[r].len()
                            { self.screen[r][c] } else { 0.0 };
                let sfrac = if r < self.screen_frac.len() && c < self.screen_frac[r].len()
                            { self.screen_frac[r][c] } else { 0.0 };

                // Leading-edge arm highlight (render-time only, not in buffer).
                let adiff = angle_diff(theta, self.scan_angle);
                let arm_b = if adiff < arm_half { 0.55 * (1.0 - adiff / arm_half) } else { 0.0 };

                let total_b = (sb + arm_b).min(1.0);

                // Range rings: four concentric circles, inner ones brighter.
                // Only drawn within the circle boundary (rn ≤ 1.01).
                let ring_color: Option<u8> = if self.rings && rn <= 1.01 {
                    let mut found = None;
                    for ri in 0..4u8 {
                        // ri 0=innermost(0.25) … 3=outermost(1.00)
                        if (rn - (ri + 1) as f32 * 0.25).abs() < 0.015 {
                            // Inner brighter: 240, 238, 236, 234.
                            found = Some(240 - ri * 2);
                            break;
                        }
                    }
                    found
                } else {
                    None
                };

                if total_b < 0.06 {
                    // Labels take priority, then rings, then blank.
                    let label_ch = label_cells.iter()
                        .find(|(lr, lc, _)| *lr == r && *lc == c)
                        .map(|(_, _, ch)| *ch);
                    if let Some(ch) = label_ch {
                        line.push_str(&format!("\x1b[38;5;245m{ch}\x1b[0m"));
                    } else if let Some(code) = ring_color {
                        line.push_str(&format!("\x1b[2m\x1b[38;5;{code}m·\x1b[0m"));
                    } else {
                        line.push(' ');
                    }
                    continue;
                }

                // Signal / arm: choose color source.
                let frac = if arm_b > sb { theta / (2.0 * PI) } else { sfrac };
                let code = radar_color(frac, total_b, &self.color_scheme);
                let ch   = if total_b > 0.85      { '█' }
                           else if total_b > 0.65 { '▓' }
                           else if total_b > 0.40 { '▒' }
                           else if total_b > 0.20 { '░' }
                           else                   { '·' };
                let bold = if total_b > 0.75 { "\x1b[1m" } else { "" };
                line.push_str(&format!("{bold}\x1b[38;5;{code}m{ch}\x1b[0m"));
            }
            lines.push(line);
        }

        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(RadarViz::new(""))]
}
