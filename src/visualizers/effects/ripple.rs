/// ripple.rs — 2-D wave propagation on a height-field grid.
///
/// A simple finite-difference wave equation is solved each frame on a
/// downsampled grid.  Audio drops in amplitude energy as "stones" that
/// excite the medium; the resulting concentric interference rings spread
/// and dissipate according to the damping config.
///
/// Config:
///   damping      — 0.1–1.0: 0.1 = long-lived rings; 1.0 = rapid decay
///   drop_mode    — beat / continuous / center
///       beat:       a drop lands at a random position on each beat
///       continuous: every frame a soft drop at a random position
///       center:     always from the screen centre
///   color_scheme — heat / ocean / neon / spectrum / mono
///   ripple_shape — circle / diamond / cross (controls the initial wavefront)

// ── Index: wave_color@29 · add_impulse@60 · RippleViz@83 · new@106 · step_wave@138 · impl@164 · config@168 · set_config@214 · tick@239 · render@287 · register@326
use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, TermSize, Visualizer,
};
use crate::visualizer_utils::rms;

const CONFIG_VERSION: u64 = 1;

// ── Colour ────────────────────────────────────────────────────────────────────

fn wave_color(h: f32, scheme: &str) -> u8 {
    // h is signed −1..1; map to 0..1 for palette
    let f = h * 0.5 + 0.5;
    let f = f.clamp(0.0, 1.0);
    match scheme {
        "heat"   => {
            const HEAT: &[u8] = &[232, 52, 88, 124, 160, 196, 202, 208, 214, 220, 226, 231];
            let i = (f * (HEAT.len() - 1) as f32) as usize;
            HEAT[i.min(HEAT.len() - 1)]
        }
        "ocean"  => {
            const OCN: &[u8] = &[17, 18, 19, 20, 21, 27, 33, 39, 45, 51, 87, 123, 159, 195, 231];
            let i = (f * (OCN.len() - 1) as f32) as usize;
            OCN[i.min(OCN.len() - 1)]
        }
        "neon"   => {
            const NEO: &[u8] = &[201, 200, 165, 129, 93, 57, 21, 27, 33, 39, 45, 51, 87, 123, 159, 231];
            let i = (f * (NEO.len() - 1) as f32) as usize;
            NEO[i.min(NEO.len() - 1)]
        }
        "mono"   => {
            let level = (f * 23.0) as u8;
            232 + level
        }
        _        => specgrad(f),
    }
}

// ── Finite-difference wave helpers ────────────────────────────────────────────

/// Add a shaped impulse at grid cell (gy, gx).
fn add_impulse(grid: &mut Vec<Vec<f32>>, gy: usize, gx: usize, amp: f32, shape: &str) {
    let gh = grid.len();
    let gw = if gh > 0 { grid[0].len() } else { return };

    let points: &[(isize, isize, f32)] = match shape {
        "diamond" => &[(0,0,1.0),(-1,0,0.7),(1,0,0.7),(0,-1,0.7),(0,1,0.7),
                       (-2,0,0.3),(2,0,0.3),(0,-2,0.3),(0,2,0.3)],
        "cross"   => &[(0,0,1.0),(-1,0,1.0),(1,0,1.0),(0,-1,1.0),(0,1,1.0),
                       (-2,0,0.5),(2,0,0.5),(0,-2,0.5),(0,2,0.5),
                       (-3,0,0.2),(3,0,0.2),(0,-3,0.2),(0,3,0.2)],
        _         => &[(0,0,1.0),(-1,0,0.7),(1,0,0.7),(0,-1,0.7),(0,1,0.7),
                       (-1,-1,0.5),(-1,1,0.5),(1,-1,0.5),(1,1,0.5)],  // circle-ish
    };

    for &(dy, dx, w) in points {
        let ny = (gy as isize + dy).clamp(0, gh as isize - 1) as usize;
        let nx = (gx as isize + dx).clamp(0, gw as isize - 1) as usize;
        grid[ny][nx] += amp * w;
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct RippleViz {
    /// Current height field.
    cur:       Vec<Vec<f32>>,
    /// Previous frame's height field.
    prv:       Vec<Vec<f32>>,
    /// Grid dimensions (downsampled from terminal).
    gw:        usize,
    gh:        usize,
    // Beat detection
    rms_avg:   f32,
    beat_cool: f32,   // seconds since last beat
    // Simple LCG-like deterministic pseudo-random state
    rng:       u64,
    source:    String,
    // config
    gain:         f32,
    damping:      f32,
    drop_mode:    String,
    color_scheme: String,
    ripple_shape: String,
}

impl RippleViz {
    pub fn new(source: &str) -> Self {
        Self {
            cur:          Vec::new(),
            prv:          Vec::new(),
            gw:           0,
            gh:           0,
            rms_avg:      0.0,
            beat_cool:    0.0,
            rng:          0x9e3779b97f4a7c15,
            source:       source.to_string(),
            gain:         1.0,
            damping:      0.5,
            drop_mode:    "beat".to_string(),
            color_scheme: "spectrum".to_string(),
            ripple_shape: "circle".to_string(),
        }
    }

    fn ensure_grid(&mut self, gh: usize, gw: usize) {
        if self.gh == gh && self.gw == gw { return; }
        self.cur = vec![vec![0.0f32; gw]; gh];
        self.prv = vec![vec![0.0f32; gw]; gh];
        self.gh  = gh;
        self.gw  = gw;
    }

    fn rand_next(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng as f32) / (u64::MAX as f32)
    }

    fn step_wave(&mut self, damping: f32) {
        let gh = self.gh;
        let gw = self.gw;
        if gh < 3 || gw < 3 { return; }

        // Precompute propagation coefficient: c² * dt² where c²·dt² ≈ 0.5 for stability
        let c2 = 0.48f32;
        let damp = (1.0 - damping * 0.06).clamp(0.80, 0.995);

        // Swap cur → prv, compute new cur in place using a scratch approach
        let mut nxt = vec![vec![0.0f32; gw]; gh];
        for r in 1..gh-1 {
            for c in 1..gw-1 {
                let laplacian = self.cur[r-1][c] + self.cur[r+1][c]
                              + self.cur[r][c-1] + self.cur[r][c+1]
                              - 4.0 * self.cur[r][c];
                nxt[r][c] = (2.0 * self.cur[r][c] - self.prv[r][c] + c2 * laplacian) * damp;
            }
        }
        self.prv = std::mem::replace(&mut self.cur, nxt);
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for RippleViz {
    fn name(&self)        -> &str { "ripple" }
    fn description(&self) -> &str { "2-D wave propagation excited by audio beats" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "ripple",
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
                    "name": "damping",
                    "display_name": "Damping",
                    "type": "float",
                    "value": 0.5,
                    "min": 0.1,
                    "max": 1.0
                },
                {
                    "name": "drop_mode",
                    "display_name": "Drop Mode",
                    "type": "enum",
                    "value": "beat",
                    "variants": ["beat", "continuous", "center"]
                },
                {
                    "name": "color_scheme",
                    "display_name": "Color Scheme",
                    "type": "enum",
                    "value": "spectrum",
                    "variants": ["heat", "ocean", "neon", "spectrum", "mono"]
                },
                {
                    "name": "ripple_shape",
                    "display_name": "Ripple Shape",
                    "type": "enum",
                    "value": "circle",
                    "variants": ["circle", "diamond", "cross"]
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
                    "gain"         => { self.gain         = entry["value"].as_f64().unwrap_or(1.0) as f32; }
                    "damping"      => { self.damping      = entry["value"].as_f64().unwrap_or(0.5) as f32; }
                    "drop_mode"    => { if let Some(s) = entry["value"].as_str() { self.drop_mode    = s.to_string(); } }
                    "color_scheme" => { if let Some(s) = entry["value"].as_str() { self.color_scheme = s.to_string(); } }
                    "ripple_shape" => { if let Some(s) = entry["value"].as_str() { self.ripple_shape = s.to_string(); } }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        let vis  = (size.rows as usize).saturating_sub(1).max(1);
        let cols = size.cols as usize;
        self.ensure_grid(vis, cols);
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let vis  = (size.rows as usize).saturating_sub(1).max(1);
        let cols = size.cols as usize;
        let gh   = vis;
        let gw   = cols;
        self.ensure_grid(gh, gw);

        let rms = rms(&audio.mono);
        let beat_threshold = self.rms_avg * 1.5;
        self.rms_avg = 0.92 * self.rms_avg + 0.08 * rms;
        self.beat_cool += dt;

        let amp = (rms * self.gain * 3.0).clamp(0.0, 1.5);

        match self.drop_mode.as_str() {
            "beat" => {
                let is_beat = rms > beat_threshold && rms > 0.01 && self.beat_cool > 0.18;
                if is_beat {
                    self.beat_cool = 0.0;
                    let gy = (self.rand_next() * (gh - 2) as f32) as usize + 1;
                    let gx = (self.rand_next() * (gw - 2) as f32) as usize + 1;
                    let shape = self.ripple_shape.clone();
                    add_impulse(&mut self.cur, gy, gx, amp, &shape);
                }
            }
            "continuous" => {
                let gy = (self.rand_next() * (gh - 2) as f32) as usize + 1;
                let gx = (self.rand_next() * (gw - 2) as f32) as usize + 1;
                let shape = self.ripple_shape.clone();
                add_impulse(&mut self.cur, gy, gx, amp * 0.3, &shape);
            }
            _ /* center */ => {
                if rms > 0.005 {
                    let gy = gh / 2;
                    let gx = gw / 2;
                    let shape = self.ripple_shape.clone();
                    add_impulse(&mut self.cur, gy, gx, amp, &shape);
                }
            }
        }

        // Run multiple simulation steps per frame if dt is large
        let steps = ((dt * 45.0).round() as usize).clamp(1, 4);
        for _ in 0..steps {
            self.step_wave(self.damping);
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let mut lines = Vec::with_capacity(rows);

        for r in 0..vis {
            let mut line = String::with_capacity(cols * 14);
            let grid_row = if r < self.cur.len() { &self.cur[r] } else { &[] as &[f32] };

            for c in 0..cols {
                let h = if c < grid_row.len() { grid_row[c] } else { 0.0 };
                let ha = h.abs();

                if ha < 0.04 {
                    line.push(' ');
                    continue;
                }

                let code = wave_color(h, &self.color_scheme);
                let ch = if ha > 0.80 { '█' }
                         else if ha > 0.55 { '▓' }
                         else if ha > 0.30 { '▒' }
                         else if ha > 0.12 { '░' }
                         else { '·' };
                let bold = if ha > 0.65 { "\x1b[1m" } else { "" };
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
    vec![Box::new(RippleViz::new(""))]
}
