/// pulsar.rs — Radial waveform ring with temporal history.
///
/// The audio waveform is drawn as a polar ring: one full revolution of the
/// circle represents one captured frame of audio samples.  Each sample is a
/// point on the circle displaced outward by its amplitude — silence produces
/// a perfect circle, loud transients create a spiky starburst.
///
/// A rolling history of recent frames is stored as a VecDeque.  Rings are
/// drawn from oldest (innermost, smallest radius) to newest (outermost,
/// largest radius).  Each ring is coloured by its RMS level:
///   dark green → bright green → yellow → red
///
/// Wobble applies a 3D tilt (rotation around the X and Y axes) to the ring
/// projection.  On each detected beat, angular velocity kicks are applied;
/// they decay smoothly so the ring swings and slowly settles flat again.
/// The projection is orthographic:
///
///   screen_x = cx + (cos(a)·cos(ty) + sin(a)·sin(tx)·sin(ty)) · rx · r
///   screen_y = cy −  sin(a)·cos(tx)                            · ry · r
///
/// Config:
///   gain       — amplitude multiplier applied to the waveform snapshot
///   ring_count — how many historical rings to display (3–12)
///   mode       — "pulsar" (rings only) or "pulsar_scope" (rings + mirrored
///                waveform scope centred in the display)
///   hue        — 0–255; shifts ring and scope colours around the palette
///   wobble     — 0.0–1.0; beat-driven 3D tilt of the ring projection

// ── Index: PulsarViz@47 · new@75 · rms_to_color@96 · tick_wobble@111 · impl@156 · config@160 · set_config@208 · tick@251 · render@284 · register@417
use std::collections::VecDeque;
use std::f32::consts::PI;

use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, TermSize, Visualizer,
};
use crate::visualizer_utils::rms;

const CONFIG_VERSION: u64 = 1;

const R_OUTER: f32 = 0.85;
const R_INNER: f32 = 0.38;

// ── Struct ────────────────────────────────────────────────────────────────────

pub struct PulsarViz {
    rings: VecDeque<(Vec<f32>, f32)>,   // (waveform snapshot, rms)

    // ── Audio / beat state ────────────────────────────────────────────────
    rms_smooth:      f32,
    beat_avg:        f32,
    time_since_beat: f32,
    beat_count:      u32,

    // ── 3D tilt state (wobble) ────────────────────────────────────────────
    tilt_x:  f32,   // rotation around X axis (radians) — tilts top/bottom
    tilt_y:  f32,   // rotation around Y axis (radians) — tilts left/right
    tilt_vx: f32,   // angular velocity around X (radians/second)
    tilt_vy: f32,   // angular velocity around Y (radians/second)

    cached_rows: usize,
    cached_cols: usize,
    source:      String,

    // ── Config fields ─────────────────────────────────────────────────────
    gain:       f32,
    ring_count: usize,
    mode_scope: bool,
    hue:        u8,
    wobble:     f32,
}

impl PulsarViz {
    pub fn new(source: &str) -> Self {
        Self {
            rings:           VecDeque::new(),
            rms_smooth:      0.0,
            beat_avg:        0.0,
            time_since_beat: 999.0,
            beat_count:      0,
            tilt_x:          0.0,
            tilt_y:          0.0,
            tilt_vx:         0.0,
            tilt_vy:         0.0,
            cached_rows:     0,
            cached_cols:     0,
            source:          source.to_string(),
            gain:            1.0,
            ring_count:      8,
            mode_scope:      false,
            hue:             0,
            wobble:          0.0,
        }
    }

    fn rms_to_color(rms: f32, hue: u8) -> u8 {
        let base  = rms.clamp(0.0, 1.0);
        let shift = hue as f32 / 255.0;
        specgrad((base + shift).fract())
    }

    fn reset_tilt(&mut self) {
        self.tilt_x  = 0.0;
        self.tilt_y  = 0.0;
        self.tilt_vx = 0.0;
        self.tilt_vy = 0.0;
    }

    /// Beat detection + 3D tilt physics.
    fn tick_wobble(&mut self, rms: f32, dt: f32) {
        if self.wobble < 0.001 { return; }

        self.beat_avg        = self.beat_avg * 0.93 + rms * 0.07;
        self.time_since_beat += dt;

        let is_beat = rms > self.beat_avg * 1.5
            && self.time_since_beat > 0.20
            && rms > 0.015;

        if is_beat {
            self.time_since_beat = 0.0;

            // Golden-angle rotation so successive beats spread evenly
            let dir = self.beat_count as f32 * 2.399_963;
            // Angular impulse in radians/second; ~PI/4 total swing at wobble=1
            let kick = self.wobble * 3.5;
            self.tilt_vx += dir.sin() * kick;
            self.tilt_vy += dir.cos() * kick;
            self.beat_count = self.beat_count.wrapping_add(1);
        }

        // Velocity decay: halves in ~0.4 s regardless of frame rate
        let damp = 0.96f32.powf(dt * 45.0);
        self.tilt_vx *= damp;
        self.tilt_vy *= damp;

        // Integrate angular velocity → tilt angle
        self.tilt_x += self.tilt_vx * dt;
        self.tilt_y += self.tilt_vy * dt;

        // Gentle spring: tilt angles also drift back toward flat on their own
        let spring = 0.985f32.powf(dt * 45.0);
        self.tilt_x *= spring;
        self.tilt_y *= spring;

        // Hard cap so the ring never collapses fully to a line
        let max_tilt = PI * 0.30 * self.wobble; // up to ~54° at wobble=1
        self.tilt_x = self.tilt_x.clamp(-max_tilt, max_tilt);
        self.tilt_y = self.tilt_y.clamp(-max_tilt, max_tilt);
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for PulsarViz {
    fn name(&self)        -> &str { "pulsar" }
    fn description(&self) -> &str { "Radial waveform ring — concentric history, scope overlay, beat wobble" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "pulsar",
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
                    "name": "ring_count",
                    "display_name": "Ring Count",
                    "type": "int",
                    "value": 8,
                    "min": 3,
                    "max": 12
                },
                {
                    "name": "mode",
                    "display_name": "Mode",
                    "type": "enum",
                    "value": "pulsar",
                    "variants": ["pulsar", "pulsar_scope"]
                },
                {
                    "name": "hue",
                    "display_name": "Hue",
                    "type": "int",
                    "value": 0,
                    "min": 0,
                    "max": 255
                },
                {
                    "name": "wobble",
                    "display_name": "Wobble",
                    "type": "float",
                    "value": 0.0,
                    "min": 0.0,
                    "max": 1.0
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
                    "gain" => {
                        self.gain = entry["value"].as_f64().unwrap_or(1.0) as f32;
                    }
                    "ring_count" => {
                        let v = entry["value"].as_i64()
                            .or_else(|| entry["value"].as_f64().map(|f| f as i64))
                            .unwrap_or(8);
                        self.ring_count = v.clamp(3, 12) as usize;
                    }
                    "mode" => {
                        self.mode_scope = entry["value"].as_str() == Some("pulsar_scope");
                    }
                    "hue" => {
                        let v = entry["value"].as_i64()
                            .or_else(|| entry["value"].as_f64().map(|f| f as i64))
                            .unwrap_or(0);
                        self.hue = v.clamp(0, 255) as u8;
                    }
                    "wobble" => {
                        self.wobble = entry["value"].as_f64().unwrap_or(0.0) as f32;
                        if self.wobble < 0.001 { self.reset_tilt(); }
                    }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        self.rings.clear();
        self.reset_tilt();
        self.cached_rows = size.rows as usize;
        self.cached_cols = size.cols as usize;
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let rows = size.rows as usize;
        let cols = size.cols as usize;

        if rows != self.cached_rows || cols != self.cached_cols {
            self.rings.clear();
            self.reset_tilt();
            self.cached_rows = rows;
            self.cached_cols = cols;
        }

        let rms = rms(&audio.mono);
        self.rms_smooth = 0.75 * self.rms_smooth + 0.25 * rms;

        self.tick_wobble(rms, dt);

        let n_snap   = cols.max(64).min(512);
        let src      = &audio.mono;
        let waveform: Vec<f32> = if src.is_empty() {
            vec![0.0; n_snap]
        } else {
            (0..n_snap).map(|i| {
                let idx = (i * src.len() / n_snap).min(src.len() - 1);
                src[idx] * self.gain
            }).collect()
        };

        self.rings.push_front((waveform, self.rms_smooth));
        while self.rings.len() > self.ring_count {
            self.rings.pop_back();
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let cx = cols as f32 / 2.0;
        let cy = vis  as f32 / 2.0;
        let rx = cols as f32 / 2.0 * 0.92;
        let ry = vis  as f32 / 2.0 * 0.46;

        // Precompute 3D rotation trig for the wobble tilt.
        // Orthographic projection of a circle rotated around X by tilt_x
        // and Y by tilt_y:
        //   proj_x = cos(a)·cos(ty) + sin(a)·sin(tx)·sin(ty)
        //   proj_y = sin(a)·cos(tx)
        let cos_tx = self.tilt_x.cos();
        let sin_tx = self.tilt_x.sin();
        let cos_ty = self.tilt_y.cos();
        let sin_ty = self.tilt_y.sin();

        let mut grid: Vec<Vec<Option<(char, u8, bool)>>> =
            vec![vec![None; cols]; vis];

        let n_rings = self.rings.len();

        // Draw rings oldest → newest (newer overwrites older)
        for age_idx in (0..n_rings).rev() {
            let (wave, ring_rms) = &self.rings[age_idx];
            if wave.is_empty() { continue; }

            let age_frac = if n_rings > 1 {
                age_idx as f32 / (n_rings - 1) as f32
            } else {
                0.0
            };

            let r_base = R_INNER + (1.0 - age_frac) * (R_OUTER - R_INNER);
            let color  = Self::rms_to_color(*ring_rms, self.hue);
            let dim    = age_frac > 0.5;

            let n_samples = wave.len();
            for (si, &amp) in wave.iter().enumerate() {
                let angle        = 2.0 * PI * si as f32 / n_samples as f32;
                let displacement = amp.clamp(-1.0, 1.0) * 0.12;
                let r_total      = (r_base + displacement).clamp(0.05, 1.15);

                let cos_a = angle.cos();
                let sin_a = angle.sin();

                // 3D orthographic projection with X/Y axis tilt
                let proj_x = cos_a * cos_ty + sin_a * sin_tx * sin_ty;
                let proj_y = sin_a * cos_tx;

                let sx = cx + proj_x * rx * r_total;
                let sy = cy - proj_y * ry * r_total;

                let xi = sx.round() as isize;
                let yi = sy.round() as isize;

                if yi < 0 || yi >= vis as isize || xi < 0 || xi >= cols as isize {
                    continue;
                }

                let amp_abs = amp.abs();
                let ch = if amp_abs > 0.7 { '#' }
                         else if amp_abs > 0.4 { '*' }
                         else if amp_abs > 0.15 { '+' }
                         else { '.' };

                grid[yi as usize][xi as usize] = Some((ch, color, !dim));
            }
        }

        // ── Scope overlay ─────────────────────────────────────────────────────
        if self.mode_scope {
            if let Some((wave, _)) = self.rings.front() {
                let scope_cr  = vis / 2;
                let scope_half = ((ry * R_INNER * 0.85) as usize).max(1);
                let scope_col  = specgrad(self.hue as f32 / 255.0);
                let n          = wave.len().max(1);

                for c in 0..cols {
                    if scope_cr < vis && grid[scope_cr][c].is_none() {
                        grid[scope_cr][c] = Some(('-', 236, false));
                    }
                }

                for c in 0..cols {
                    let si  = (c * n / cols).min(n - 1);
                    let amp = wave[si].abs().clamp(0.0, 1.0);
                    let h   = (amp * scope_half as f32).round() as usize;

                    for dy in 1..=h {
                        let top = scope_cr.saturating_sub(dy);
                        let bot = (scope_cr + dy).min(vis - 1);
                        let ch  = if dy == h { '▪' } else { '│' };
                        let bld = dy == h;
                        grid[top][c] = Some((ch, scope_col, bld));
                        if bot != top {
                            grid[bot][c] = Some((ch, scope_col, bld));
                        }
                    }
                }
            }
        }

        // ── Flatten to strings ────────────────────────────────────────────────
        let mut lines = Vec::with_capacity(rows);
        for r in 0..vis {
            let mut line = String::with_capacity(cols * 14);
            for c in 0..cols {
                match grid[r][c] {
                    Some((ch, color, bright)) => {
                        let pfx = if bright { "\x1b[1m" } else { "\x1b[2m" };
                        line.push_str(&format!("{pfx}\x1b[38;5;{color}m{ch}\x1b[0m"));
                    }
                    None => line.push(' '),
                }
            }
            lines.push(line);
        }

        let rms_pct  = (self.rms_smooth * 100.0).min(100.0) as u32;
        let mode_str = if self.mode_scope { "scope" } else { "rings" };
        let extra    = format!(" | {} | {} rings | rms {:3}%",
            mode_str, self.rings.len(), rms_pct);
        lines.push(status_bar(cols, fps, self.name(), &self.source, &extra));
        pad_frame(lines, rows, cols)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(PulsarViz::new(""))]
}
