/// lissajous.rs — Full-terminal XY oscilloscope with beat-driven rotation.
///
/// ═══════════════════════════════════════════════════════════════════════════
///  OVERVIEW
/// ═══════════════════════════════════════════════════════════════════════════
///
/// The visualizer maps the left audio channel to the horizontal axis and the
/// right channel to the vertical axis.  Each audio sample becomes a point;
/// the persistence grid retains old points with decaying brightness, forming
/// the characteristic Lissajous figure.
///
/// The entire XY signal is rotated in signal-space by an angle that
/// accumulates based on a beat-onset detector.  On each detected beat the
/// angular velocity gets a kick; it then decays back to a slow baseline.
///
/// ═══════════════════════════════════════════════════════════════════════════
///  RENDERING LAYERS  (back → front)
/// ═══════════════════════════════════════════════════════════════════════════
///
///  1. Orbit reference rings
///  2. Radial spokes
///  3. Phase-dot constellation
///  4. Dead-centre nucleus
///  5. Vocal stars
///  6. Planets
///  7. Beat ripples
///  8. Spectrum shell
///  9. Persistence grid (Lissajous trace itself)
///
/// ═══════════════════════════════════════════════════════════════════════════
///  CONFIG FIELDS
/// ═══════════════════════════════════════════════════════════════════════════
///
///  gain             — linear amplitude multiplier on the L/R samples before
///                     plotting; controls how "wide" the figure spreads.
///  star_amplitude   — scales vocal-star outward velocity.  Higher = stars
///                     fly out faster and further.
///  rotation_speed   — scales both the idle rotation baseline and the per-beat
///                     angular kick.  Set to 0 to freeze rotation.
///  beat_sensitivity — divides the beat detection threshold.  >1.0 = more
///                     beats; <1.0 = only strong beats trigger.

// ── Index: data structs@82 · LissajousViz@118 · new@188 · impl@660 · config@666 · set_config@707 · tick@733 · render@805 · register@893
use std::collections::{HashMap, VecDeque};
use std::f32::consts::PI;

use rand::Rng;

use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar,
    AudioFrame, SpectrumBars, TermSize, Visualizer, FFT_SIZE,
};
use crate::visualizer_utils::{freq_to_bin, rms as calc_rms};

const CONFIG_VERSION: u64 = 1;

// ── Colour palettes ──────────────────────────────────────────────────────────

const LP_DEEP: &[u8] = &[17, 18, 19, 20, 21];
const LP_MID:  &[u8] = &[27, 33, 39, 45, 51];
const LP_HUE: &[u8] = &[
    196, 202, 208, 214, 220, 226, 154, 118, 82, 46,
    51,  45,  39,  33,  27,  21,  57,  93, 129, 165, 201,
];

// ── Planet configuration ─────────────────────────────────────────────────────

/// (band_lo_hz, band_hi_hz, orbit_radius_frac, colour_256)
/// Ordered innermost → outermost (highest freq → lowest freq).
const PLANET_BANDS: &[(f32, f32, f32, u8)] = &[
    (4_000.0, 12_000.0, 0.20, 141),
    (1_500.0,  4_000.0, 0.35,  51),
    (  500.0,  1_500.0, 0.50, 226),
    (  150.0,    500.0, 0.65,  82),
    (   40.0,    150.0, 0.80, 196),
    (   20.0,     40.0, 0.92,  57),
];

// ── Sub-structs ───────────────────────────────────────────────────────────────

struct VocalStar {
    angle:    f32,
    radius:   f32,
    vel_r:    f32,
    life:     f32,
    max_life: f32,
    colour:   u8,
}

struct Planet {
    angle:   f32,
    orbit_r: f32,
    lo_bin:  usize,
    hi_bin:  usize,
    energy:  f32,
    colour:  u8,
    trail:   VecDeque<(f32, f32)>,
}

struct Ripple {
    radius:     f32,
    brightness: f32,
}

// ── Geometry cache ────────────────────────────────────────────────────────────

type RingCache = Vec<(usize, usize, u8)>;

struct ShellCache {
    sin: Vec<f32>,
    cos: Vec<f32>,
    n:   usize,
}

// ── Main struct ───────────────────────────────────────────────────────────────

pub struct LissajousViz {
    // ── Shared spectrum bars ──────────────────────────────────────────────────
    bars: SpectrumBars,

    // ── Raw audio samples ─────────────────────────────────────────────────────
    left:  Vec<f32>,
    right: Vec<f32>,

    // ── Persistence grid ──────────────────────────────────────────────────────
    brightness: Vec<Vec<f32>>,
    age:        Vec<Vec<f32>>,

    // ── Rotation (beat-driven) ────────────────────────────────────────────────
    rot_angle:    f32,
    rot_vel:      f32,
    rot_vel_max:  f32,
    rot_baseline: f32,

    // ── Hue animation ─────────────────────────────────────────────────────────
    hue_t: f32,

    // ── Beat onset detector ───────────────────────────────────────────────────
    beat_avg:        f32,
    beat_alpha:      f32,
    /// Base threshold (before beat_sensitivity scaling).
    beat_thresh:     f32,
    beat_min_dt:     f32,
    time_since_beat: f32,

    // ── Beat ripples ──────────────────────────────────────────────────────────
    ripples: Vec<Ripple>,

    // ── Spokes ───────────────────────────────────────────────────────────────
    spoke_phase: f32,
    rms_smooth:  f32,

    // ── Phase-dot constellation ───────────────────────────────────────────────
    phase_dots: Vec<(f32, f32)>,

    // ── Vocal stars ───────────────────────────────────────────────────────────
    vocal_stars:  Vec<VocalStar>,
    vocal_energy: f32,
    vocal_avg:    f32,
    vocal_lo_bin: usize,
    vocal_hi_bin: usize,

    // ── Planets ───────────────────────────────────────────────────────────────
    planets: Vec<Planet>,

    // ── Geometry caches ───────────────────────────────────────────────────────
    ring_cache:  Option<RingCache>,
    shell_cache: Option<ShellCache>,

    // ── Metadata ─────────────────────────────────────────────────────────────
    source:      String,
    cached_rows: usize,
    cached_cols: usize,

    // ── Config fields ─────────────────────────────────────────────────────────
    /// Linear amplitude multiplier on L/R samples before grid plotting.
    gain: f32,
    /// Multiplier on vocal-star outward velocity.
    star_amplitude: f32,
    /// Multiplier on rotation baseline drift and per-beat kick magnitude.
    rotation_speed: f32,
    /// Divides the beat-detection threshold: >1.0 = more sensitive.
    beat_sensitivity: f32,
}

impl LissajousViz {
    pub fn new(source: &str) -> Self {
        let mut rng = rand::thread_rng();

        let n_fft_bins = FFT_SIZE / 2 + 1;
        let vocal_lo  = freq_to_bin(300.0, n_fft_bins);
        let vocal_hi  = freq_to_bin(3400.0, n_fft_bins);

        let planets = PLANET_BANDS.iter().map(|&(flo, fhi, orbit_r, col)| {
            let lo = freq_to_bin(flo, n_fft_bins);
            let hi = freq_to_bin(fhi, n_fft_bins).max(lo + 1);
            Planet {
                angle:   rng.gen_range(0.0..2.0 * PI),
                orbit_r,
                lo_bin:  lo,
                hi_bin:  hi.max(lo + 1),
                energy:  0.0,
                colour:  col,
                trail:   VecDeque::with_capacity(20),
            }
        }).collect();

        let phase_dots = (0..24).map(|_| {
            (rng.gen_range(0.0..2.0 * PI), rng.gen_range(0.15f32..0.42))
        }).collect();

        Self {
            bars:            SpectrumBars::new(80),
            left:            vec![0.0; FFT_SIZE],
            right:           vec![0.0; FFT_SIZE],
            brightness:      Vec::new(),
            age:             Vec::new(),
            rot_angle:       0.0,
            rot_vel:         0.02,
            rot_vel_max:     3.8,
            rot_baseline:    0.02,
            hue_t:           0.0,
            beat_avg:        0.0,
            beat_alpha:      0.15,
            beat_thresh:     1.55,
            beat_min_dt:     0.18,
            time_since_beat: 999.0,
            ripples:         Vec::new(),
            spoke_phase:     0.0,
            rms_smooth:      0.0,
            phase_dots,
            vocal_stars:     Vec::new(),
            vocal_energy:    0.0,
            vocal_avg:       0.0,
            vocal_lo_bin:    vocal_lo,
            vocal_hi_bin:    vocal_hi.max(vocal_lo + 1),
            planets,
            ring_cache:      None,
            shell_cache:     None,
            source:          source.to_string(),
            cached_rows:     0,
            cached_cols:     0,
            gain:             1.0,
            star_amplitude:   1.0,
            rotation_speed:   1.0,
            beat_sensitivity: 1.0,
        }
    }

    // ── Grid helpers ──────────────────────────────────────────────────────────

    fn ensure_grid(&mut self, vis: usize, cols: usize) {
        if self.brightness.len() != vis
            || self.brightness.first().map_or(0, |r| r.len()) != cols
        {
            self.brightness = vec![vec![0.0f32; cols]; vis];
            self.age        = vec![vec![1.0f32; cols]; vis];
        }
    }

    fn n_planets_for(rows: usize, cols: usize) -> usize {
        let area = rows * cols;
        if      area < 2_000  { 3 }
        else if area < 6_000  { 4 }
        else if area < 12_000 { 5 }
        else                  { 6 }
    }

    fn accent(&self) -> u8 {
        LP_HUE[(self.hue_t * LP_HUE.len() as f32) as usize % LP_HUE.len()]
    }

    fn accent2(&self) -> u8 {
        let i = (self.hue_t * LP_HUE.len() as f32) as usize;
        LP_HUE[(i + LP_HUE.len() / 3) % LP_HUE.len()]
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  TICK SUBSYSTEMS
    // ─────────────────────────────────────────────────────────────────────────

    fn tick_beat(&mut self, mono: &[f32], dt: f32) {
        let rms = calc_rms(mono);

        self.beat_avg = self.beat_alpha * rms + (1.0 - self.beat_alpha) * self.beat_avg;
        self.time_since_beat += dt;

        // beat_sensitivity > 1.0 → lower effective threshold → more beats
        let effective_thresh = self.beat_thresh / self.beat_sensitivity.max(0.1);

        let is_beat = rms > effective_thresh * self.beat_avg
            && self.time_since_beat > self.beat_min_dt
            && rms > 0.01;

        if is_beat {
            self.time_since_beat = 0.0;
            let kick_dir = if self.rot_angle.sin() >= 0.0 { 1.0f32 } else { -1.0 };
            // rotation_speed scales the angular kick magnitude
            let kick_mag = (0.8 + rms * 4.0) * self.rotation_speed;
            self.rot_vel = (self.rot_vel + kick_dir * kick_mag)
                .clamp(-self.rot_vel_max, self.rot_vel_max);
            self.ripples.push(Ripple { radius: 0.0, brightness: 1.0 });
        }

        // Decay rot_vel toward the (scaled) baseline
        let baseline_scaled = self.rot_baseline * self.rotation_speed;
        let sign    = self.rot_vel.signum();
        let new_vel = self.rot_vel - sign * 1.8 * dt;
        self.rot_vel = if new_vel.abs() < baseline_scaled {
            baseline_scaled
        } else {
            new_vel
        };

        self.rot_angle   = (self.rot_angle + self.rot_vel * dt).rem_euclid(2.0 * PI);
        self.hue_t       = self.rot_angle / (2.0 * PI);
        self.spoke_phase = (self.spoke_phase + dt * 0.35).rem_euclid(2.0 * PI);

        for r in &mut self.ripples {
            r.radius     += dt * 1.4;
            r.brightness -= dt * 2.2;
        }
        self.ripples.retain(|r| r.brightness > 0.0 && r.radius < 1.3);
    }

    fn tick_rms(&mut self, mono: &[f32]) {
        let rms = calc_rms(mono);
        self.rms_smooth = 0.7 * self.rms_smooth + 0.3 * rms;
    }

    fn tick_vocal_stars(&mut self, fft: &[f32], dt: f32) {
        let mut rng = rand::thread_rng();

        let lo = self.vocal_lo_bin;
        let hi = self.vocal_hi_bin.min(fft.len());
        let v_rms = if hi > lo {
            let slice = &fft[lo..hi];
            (slice.iter().map(|v| v * v).sum::<f32>() / slice.len() as f32).sqrt() * 60.0
        } else {
            0.0
        };

        let a_v = if v_rms > self.vocal_energy { 0.55 } else { 0.20 };
        self.vocal_energy = a_v * v_rms + (1.0 - a_v) * self.vocal_energy;
        self.vocal_avg    = 0.02 * self.vocal_energy + 0.98 * self.vocal_avg;

        let onset_ratio = self.vocal_energy / self.vocal_avg.max(1e-6);
        let is_onset    = onset_ratio > 1.35 && self.vocal_energy > 0.04;

        if is_onset {
            let n_new = ((1.0 + (onset_ratio - 1.35) * 10.0).min(6.0)) as usize;
            let warm: &[u8] = &[231, 230, 229, 228, 227, 226, 220, 214];
            for _ in 0..n_new {
                // star_amplitude scales the outward velocity
                let base_vel = 0.18 + self.vocal_energy * 0.55 + rng.gen_range(0.0..0.12);
                self.vocal_stars.push(VocalStar {
                    angle:    rng.gen_range(0.0..2.0 * PI),
                    radius:   0.02,
                    vel_r:    base_vel * self.star_amplitude,
                    life:     0.6 + rng.gen_range(0.0..0.5),
                    max_life: 1.1,
                    colour:   warm[rng.gen_range(0..warm.len())],
                });
            }
        }

        if self.vocal_energy > 0.06 && rng.r#gen::<f32>() < self.vocal_energy * 0.4 {
            let cool: &[u8] = &[195, 159, 231, 230, 229];
            let base_vel = 0.10 + self.vocal_energy * 0.30;
            self.vocal_stars.push(VocalStar {
                angle:    rng.gen_range(0.0..2.0 * PI),
                radius:   0.01,
                vel_r:    base_vel * self.star_amplitude,
                life:     0.4 + rng.gen_range(0.0..0.3),
                max_life: 0.7,
                colour:   cool[rng.gen_range(0..cool.len())],
            });
        }

        for s in &mut self.vocal_stars {
            s.radius += s.vel_r * dt;
            s.life   -= dt;
        }
        self.vocal_stars.retain(|s| s.life > 0.0 && s.radius < 1.05);
    }

    fn tick_planets(&mut self, fft: &[f32], dt: f32, n_visible: usize) {
        self.planets.truncate(n_visible);

        for p in &mut self.planets {
            let lo = p.lo_bin;
            let hi = p.hi_bin.min(fft.len());
            let raw_e = if hi > lo {
                let slice = &fft[lo..hi];
                (slice.iter().map(|v| v * v).sum::<f32>() / slice.len() as f32).sqrt() * 80.0
            } else { 0.0 };
            let raw_e = raw_e.min(1.0);

            let a_p = if raw_e > p.energy { 0.50 } else { 0.15 };
            p.energy = a_p * raw_e + (1.0 - a_p) * p.energy;

            let baseline = 0.55 * (1.0 - p.orbit_r) + 0.06;
            let omega    = baseline + p.energy * 1.8;
            let old_angle = p.angle;
            p.angle = (p.angle + omega * dt).rem_euclid(2.0 * PI);

            p.trail.push_front((old_angle, 1.0));
            if p.trail.len() > 18 { p.trail.pop_back(); }

            for (_, alpha) in &mut p.trail { *alpha *= 0.82; }
            while p.trail.back().map_or(false, |&(_, a)| a < 0.05) {
                p.trail.pop_back();
            }
        }
    }

    fn tick_grid(&mut self, vis: usize, cols: usize, dt: f32) {
        let cx = (cols - 1) as f32 / 2.0;
        let cy = (vis  - 1) as f32 / 2.0;

        let ca = self.rot_angle.cos();
        let sa = self.rot_angle.sin();

        let half_x = cx * 0.96;
        let half_y = cy * 0.96;

        let decay = (0.84 - self.rms_smooth * 0.12).clamp(0.72, 0.92);
        for row in &mut self.brightness { for v in row { *v *= decay; } }
        for row in &mut self.age        { for v in row { *v = (*v + dt * 0.9).min(1.0); } }

        for i in 0..self.left.len().min(FFT_SIZE) {
            // Apply gain before coordinate mapping
            let lv = self.left [i] * self.gain;
            let rv = self.right[i] * self.gain;

            let xr =  ca * lv + sa * rv;
            let yr = -sa * lv + ca * rv;

            let xi = (xr  * half_x + cx).round().clamp(0.0, (cols - 1) as f32) as usize;
            let yi = (-yr * half_y + cy).round().clamp(0.0, (vis  - 1) as f32) as usize;

            self.brightness[yi][xi] = 1.0;
            self.age       [yi][xi] = 0.0;

            const NEIGHBOURS: &[(isize, isize, f32)] = &[
                (-1, 0, 0.55), (1, 0, 0.55), (0, -1, 0.45), (0, 1, 0.45),
            ];
            for &(dr, dc, w) in NEIGHBOURS {
                let ny = (yi as isize + dr).clamp(0, vis  as isize - 1) as usize;
                let nx = (xi as isize + dc).clamp(0, cols as isize - 1) as usize;
                if self.brightness[ny][nx] < w {
                    self.brightness[ny][nx] = w;
                    self.age       [ny][nx] = 0.1;
                }
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  RENDER HELPERS
    // ─────────────────────────────────────────────────────────────────────────

    #[inline(always)]
    fn fg(code: u8) -> String { format!("\x1b[38;5;{code}m") }

    fn polar_to_screen(
        angle: f32, r_frac: f32,
        rx_full: f32, ry_full: f32,
        icx: usize, icy: usize,
        vis: usize, cols: usize,
    ) -> Option<(usize, usize)> {
        let xd =  angle.cos() * r_frac;
        let yd = -angle.sin() * r_frac;
        let rc = (icy as f32 + yd * ry_full).round() as isize;
        let cc = (icx as f32 + xd * rx_full).round() as isize;
        if rc >= 0 && rc < vis as isize && cc >= 0 && cc < cols as isize {
            Some((rc as usize, cc as usize))
        } else {
            None
        }
    }

    fn build_detail(
        &self,
        vis: usize, cols: usize,
        icx: usize, icy: usize,
        rx_full: f32, ry_full: f32,
        accent: u8, accent2: u8,
    ) -> HashMap<(usize, usize), (char, u8, bool)> {
        let mut detail: HashMap<(usize, usize), (char, u8, bool)> =
            HashMap::with_capacity(1024);

        // ── Layer 1: Orbit reference rings ────────────────────────────────────
        if let Some(cache) = &self.ring_cache {
            for &(r, c, ring_col) in cache {
                detail.entry((r, c)).or_insert(('.', ring_col, false));
            }
        }

        // ── Layer 2: Radial spokes ────────────────────────────────────────────
        let spoke_len = (0.10 + self.rms_smooth * 0.50).min(1.0);
        const N_SPOKES: usize = 8;
        const N_STEPS:  usize = 18;
        for si in 0..N_SPOKES {
            let a       = self.spoke_phase + si as f32 * (2.0 * PI / N_SPOKES as f32);
            let sin_a   = a.sin();
            let cos_a   = a.cos();
            let abs_sin = sin_a.abs();
            let ch_base = if abs_sin > 0.7 { '|' } else { '-' };

            for step in 0..N_STEPS {
                let frac = 0.03 + (spoke_len - 0.03) * step as f32 / (N_STEPS - 1) as f32;
                let rc = (icy as f32 - sin_a * ry_full * frac).round() as isize;
                let cc = (icx as f32 + cos_a * rx_full * frac).round() as isize;
                if rc < 0 || rc >= vis as isize || cc < 0 || cc >= cols as isize { continue; }
                let (rc, cc) = (rc as usize, cc as usize);

                let bright = 1.0 - frac / spoke_len;
                let ch  = if frac < 0.06 { '+' } else { ch_base };
                let col = if bright > 0.7 { accent } else if bright > 0.4 { accent2 } else { 238 };
                detail.insert((rc, cc), (ch, col, bright > 0.6));
            }
        }

        // ── Layer 3: Phase-dot constellation ─────────────────────────────────
        let rms = self.rms_smooth;
        for &(base_a, r_frac) in &self.phase_dots {
            let a    = base_a + self.rot_angle;
            let rdot = r_frac * (0.6 + rms * 0.9);
            if let Some((rc, cc)) = Self::polar_to_screen(
                a, rdot, rx_full, ry_full, icx, icy, vis, cols,
            ) {
                let col = if r_frac < 0.28 { accent } else { accent2 };
                detail.insert((rc, cc), ('*', col, true));
            }
        }

        // ── Layer 4: Dead-centre nucleus ──────────────────────────────────────
        let nuc_r = (self.rms_smooth * 3.5).round() as isize;
        for dr in -nuc_r..=nuc_r {
            for dc in -nuc_r..=nuc_r {
                let dist = ((dr * dr) as f32 + (dc as f32 * 0.5).powi(2)).sqrt();
                if dist <= nuc_r as f32 + 0.5 {
                    let rc = (icy as isize + dr).clamp(0, vis  as isize - 1) as usize;
                    let cc = (icx as isize + dc).clamp(0, cols as isize - 1) as usize;
                    let ch = if dist < 0.8 { '@' } else if dist < 1.5 { '#' } else { '*' };
                    detail.insert((rc, cc), (ch, accent, true));
                }
            }
        }

        // ── Layer 5: Vocal stars ──────────────────────────────────────────────
        for s in &self.vocal_stars {
            let life_frac = s.life / s.max_life.max(1e-6);
            if let Some((rc, cc)) = Self::polar_to_screen(
                s.angle, s.radius, rx_full, ry_full, icx, icy, vis, cols,
            ) {
                let ch   = if life_frac > 0.65 { '*' } else if life_frac > 0.30 { '+' } else { '.' };
                let bold = life_frac > 0.50;
                detail.insert((rc, cc), (ch, s.colour, bold));
            }
            let trail_r = (s.radius - s.vel_r * 0.04).max(0.0);
            if life_frac > 0.40 {
                if let Some((rc2, cc2)) = Self::polar_to_screen(
                    s.angle, trail_r, rx_full, ry_full, icx, icy, vis, cols,
                ) {
                    detail.entry((rc2, cc2)).or_insert(('.', s.colour, false));
                }
            }
        }

        // ── Layer 6: Planets ──────────────────────────────────────────────────
        for p in &self.planets {
            for &(t_angle, t_alpha) in &p.trail {
                if let Some((rc, cc)) = Self::polar_to_screen(
                    t_angle, p.orbit_r, rx_full, ry_full, icx, icy, vis, cols,
                ) {
                    let trail_col = if t_alpha > 0.65 { p.colour }
                                    else if t_alpha > 0.35 { 240 }
                                    else { 236 };
                    let existing = detail.get(&(rc, cc));
                    if existing.is_none() || existing.map_or(false, |e| e.0 == '.') {
                        detail.insert((rc, cc), ('.', trail_col, false));
                    }
                }
            }
            if let Some((rc, cc)) = Self::polar_to_screen(
                p.angle, p.orbit_r, rx_full, ry_full, icx, icy, vis, cols,
            ) {
                detail.insert((rc, cc), ('o', p.colour, true));
            }
        }

        // ── Layer 7: Beat ripples ─────────────────────────────────────────────
        for rp in &self.ripples {
            if rp.brightness <= 0.0 || rp.radius <= 0.0 { continue; }

            let (rp_ch, rp_col, rp_bold) = if rp.brightness > 0.70 {
                ('o', accent, true)
            } else if rp.brightness > 0.35 {
                ('+', accent2, false)
            } else {
                let i = (rp.brightness * LP_MID.len() as f32) as usize;
                ('.', LP_MID[i.min(LP_MID.len() - 1)], false)
            };

            let rx_rp = rx_full * rp.radius;
            let ry_rp = ry_full * rp.radius;
            let steps = ((rx_rp + ry_rp) * 3.0).max(48.0) as usize;

            for i in 0..steps {
                let a  = 2.0 * PI * i as f32 / steps as f32;
                let rc = (icy as f32 - a.sin() * ry_rp).round() as isize;
                let cc = (icx as f32 + a.cos() * rx_rp).round() as isize;
                if rc < 0 || rc >= vis as isize || cc < 0 || cc >= cols as isize { continue; }
                let key = (rc as usize, cc as usize);
                let existing = detail.get(&key);
                if existing.is_none() || existing.map_or(false, |e| matches!(e.0, '.' | '-' | '|')) {
                    detail.insert(key, (rp_ch, rp_col, rp_bold));
                }
            }
        }

        // ── Layer 8: Spectrum shell ───────────────────────────────────────────
        let n_spec = self.bars.smoothed.len();
        let shell = match &self.shell_cache {
            Some(sc) if sc.n == n_spec => sc,
            _ => return detail,
        };
        let shell_r_base  = 0.94f32;
        const SHELL_STEPS: usize = 5;

        for si in 0..n_spec {
            let e = self.bars.smoothed[si];
            if e < 0.01 { continue; }

            let frac  = si as f32 / (n_spec - 1).max(1) as f32;
            let code  = specgrad(frac);
            let bold  = e > 0.6;
            let t_len = e * 0.10;

            for step in 0..SHELL_STEPS {
                let df     = step as f32 / (SHELL_STEPS - 1) as f32;
                let frac_r = shell_r_base + df * t_len;
                let rc = (icy as f32 - shell.sin[si] * ry_full * frac_r).round() as isize;
                let cc = (icx as f32 + shell.cos[si] * rx_full * frac_r).round() as isize;
                if rc >= 0 && rc < vis as isize && cc >= 0 && cc < cols as isize {
                    let key = (rc as usize, cc as usize);
                    let ch  = if bold { '|' } else { '.' };
                    detail.entry(key).or_insert((ch, code, bold));
                }
            }
        }

        detail
    }
}

impl Visualizer for LissajousViz {
    fn name(&self)        -> &str { "lissajous" }
    fn description(&self) -> &str { "Full-terminal XY scope — beat rotation, planets, vocal stars, ripples" }

    // ── Config ────────────────────────────────────────────────────────────────

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "lissajous",
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
                    "name": "star_amplitude",
                    "display_name": "Star Amplitude",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.0,
                    "max": 2.0
                },
                {
                    "name": "rotation_speed",
                    "display_name": "Rotation Speed",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.0,
                    "max": 5.0
                },
                {
                    "name": "beat_sensitivity",
                    "display_name": "Beat Sensitivity",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.0,
                    "max": 2.0
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
                    "gain"             => self.gain             = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "star_amplitude"   => self.star_amplitude   = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "rotation_speed"   => self.rotation_speed   = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "beat_sensitivity" => self.beat_sensitivity = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    fn on_resize(&mut self, size: TermSize) {
        self.bars.resize(size.cols as usize);
        self.ring_cache  = None;
        self.shell_cache = None;
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        if rows != self.cached_rows || cols != self.cached_cols {
            self.bars.resize(cols);
            self.ring_cache  = None;
            self.shell_cache = None;
            self.cached_rows = rows;
            self.cached_cols = cols;
        }

        self.bars.update(&audio.fft, dt);

        // Gain is applied in tick_grid when mapping samples to grid coordinates;
        // the raw copies are stored here so the gain can be changed dynamically.
        self.left .clone_from(&audio.left);
        self.right.clone_from(&audio.right);

        self.ensure_grid(vis, cols);

        self.tick_beat(&audio.mono, dt);
        self.tick_rms (&audio.mono);
        self.tick_vocal_stars(&audio.fft, dt);

        let n_vis = Self::n_planets_for(rows, cols);
        self.tick_planets(&audio.fft, dt, n_vis);

        // Warm geometry caches (computed here under &mut self so render only needs &self)
        {
            let cx      = (cols - 1) as f32 / 2.0;
            let cy      = (vis  - 1) as f32 / 2.0;
            let rx_full = cx * 0.96;
            let ry_full = cy * 0.96;
            let icx     = cols / 2;
            let icy     = vis  / 2;

            if self.ring_cache.is_none() {
                let mut cache = Vec::new();
                for &(frac, ring_col) in &[(0.25f32, 235u8), (0.52, 236), (0.80, 237)] {
                    let rx    = rx_full * frac;
                    let ry    = ry_full * frac;
                    let steps = ((rx + ry) * 2.5).max(64.0) as usize;
                    for i in 0..steps {
                        let a  = 2.0 * PI * i as f32 / steps as f32;
                        let rc = (icy as f32 - a.sin() * ry).round() as isize;
                        let cc = (icx as f32 + a.cos() * rx).round() as isize;
                        if rc >= 0 && rc < vis as isize && cc >= 0 && cc < cols as isize {
                            cache.push((rc as usize, cc as usize, ring_col));
                        }
                    }
                }
                self.ring_cache = Some(cache);
            }

            let n_spec  = self.bars.smoothed.len();
            let rebuild = self.shell_cache.as_ref().map_or(true, |sc| sc.n != n_spec);
            if rebuild {
                let sin_vals: Vec<f32> = (0..n_spec).map(|i| {
                    (i as f32 * 2.0 * PI / n_spec as f32 - PI / 2.0).sin()
                }).collect();
                let cos_vals: Vec<f32> = (0..n_spec).map(|i| {
                    (i as f32 * 2.0 * PI / n_spec as f32 - PI / 2.0).cos()
                }).collect();
                self.shell_cache = Some(ShellCache { sin: sin_vals, cos: cos_vals, n: n_spec });
            }
        }

        self.tick_grid(vis, cols, dt);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = rows.saturating_sub(1).max(1);

        let icx = cols / 2;
        let icy = vis  / 2;
        let cx  = (cols - 1) as f32 / 2.0;
        let cy  = (vis  - 1) as f32 / 2.0;
        let rx_full = cx * 0.96;
        let ry_full = cy * 0.96;

        let accent  = self.accent();
        let accent2 = self.accent2();

        let detail = self.build_detail(vis, cols, icx, icy, rx_full, ry_full, accent, accent2);

        // Group detail by row for O(detail/row) lookup
        let mut detail_by_row: HashMap<usize, Vec<(usize, char, u8, bool)>> = HashMap::new();
        for (&(r, c), &(ch, col, bold)) in &detail {
            if r < vis {
                detail_by_row.entry(r).or_default().push((c, ch, col, bold));
            }
        }

        let mut lines = Vec::with_capacity(rows);
        let n_mid  = LP_MID.len();
        let n_deep = LP_DEEP.len();

        for r in 0..vis {
            let mut row_chars: Vec<Option<String>> = vec![None; cols];

            let brow: &[f32] = if r < self.brightness.len() { &self.brightness[r] } else { &[] };
            let arow: &[f32] = if r < self.age.len()        { &self.age[r]        } else { &[] };

            for c in 0..cols {
                let b = if c < brow.len() { brow[c] } else { 0.0 };
                if b <= 0.06 { continue; }

                let a_val = if c < arow.len() { arow[c] } else { 1.0 };

                let code = if a_val < 0.15 {
                    accent
                } else if a_val < 0.45 {
                    let i = (a_val * n_mid as f32) as usize;
                    LP_MID[i.min(n_mid - 1)]
                } else {
                    let i = (a_val * n_deep as f32) as usize;
                    LP_DEEP[i.min(n_deep - 1)]
                };

                let ch   = if b > 0.88 { '@' } else if b > 0.65 { '#' }
                           else if b > 0.40 { '*' } else if b > 0.20 { '+' }
                           else { '.' };
                let bold = if b > 0.70 { "\x1b[1m" } else { "" };
                row_chars[c] = Some(format!("{bold}\x1b[38;5;{code}m{ch}\x1b[0m"));
            }

            if let Some(entries) = detail_by_row.get(&r) {
                for &(c, ch, col, bold) in entries {
                    if c < cols && row_chars[c].is_none() {
                        let pfx = if bold { "\x1b[1m" } else { "\x1b[2m" };
                        row_chars[c] = Some(format!("{pfx}{}{ch}\x1b[0m", Self::fg(col)));
                    }
                }
            }

            let line: String = row_chars.into_iter()
                .map(|cell| cell.unwrap_or_else(|| " ".to_string()))
                .collect();
            lines.push(line);
        }

        // ── Status bar ────────────────────────────────────────────────────────
        let vel_deg = self.rot_vel * 180.0 / PI;
        let ang_deg = (self.rot_angle * 180.0 / PI) as u32 % 360;
        let beat_ind = if !self.ripples.is_empty() {
            format!("{}\x1b[1m●\x1b[0m", Self::fg(accent))
        } else {
            " ".to_string()
        };
        let extra = format!(" | {beat_ind} {ang_deg:3}° {vel_deg:+.1}°/s");
        lines.push(status_bar(cols, fps, self.name(), &self.source, &extra));

        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(LissajousViz::new(""))]
}
