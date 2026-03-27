/// missiles.rs — Missile Command / Penetrate-style audio visualizer.
///
/// Missiles rain down from the top; interceptors launch from surviving buildings
/// to shoot them down.  Buildings take damage from ground-level explosions and
/// slowly repair themselves during quiet passages.
///
/// Building types (deterministic from terminal width):
///   tower  — narrow (1–2 cols), tall (7–12 rows), antenna spire on top
///   office — medium (3–5 cols), mid-height (3–6 rows), lit/dark windows
///   block  — wide (4–7 cols), low (2–3 rows), plain facade
///
/// Audio mapping:
///   bass energy    → spawn rate and explosion radius
///   beat transient → burst of 1–3 extra missiles
///   overall level  → travel speed; quiet = repair; loud = window flicker

// ── Index: ThemeData@32 · theme_data@129 · entities@247 · MissilesViz@291 · new@330 · regen_city@373 · impl@579 · config@583 · set_config@687 · tick@727 · render@941 · register@1173
use std::collections::VecDeque;
use rand::Rng;
use crate::beat::{BeatDetector, BeatDetectorConfig};
use crate::visualizer::{
    merge_config, pad_frame, status_bar,
    AudioFrame, SpectrumBars, TermSize, Visualizer,
};

const CONFIG_VERSION:  u64   = 1;
const SPARK_LEN:       usize = 28;
const SPARK_CHARS: &[char]   = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

// ── Theme definitions ─────────────────────────────────────────────────────────

struct ThemeData {
    missile_palettes:   &'static [&'static [u8]],
    expl_colors:        &'static [u8],
    interceptor_colors: &'static [u8],
    city_shades:        &'static [u8],  // [base, accent, dark]
    ground_color:       u8,
    window_lit:         u8,
    window_dark:        u8,
    antenna_color:      u8,
}

// ── classic ───────────────────────────────────────────────────────────────────
const T_CL_MP0: &[u8] = &[231, 220, 214, 208, 202, 196, 160, 124, 88];
const T_CL_MP1: &[u8] = &[226, 220, 214, 178, 142, 106, 70, 34];
const T_CL_MP2: &[u8] = &[208, 202, 166, 130, 94, 58];
const T_CL_MISSILE_PALETTES: &[&[u8]] = &[T_CL_MP0, T_CL_MP1, T_CL_MP2];
const T_CL_EXPL: &[u8] = &[231, 230, 229, 228, 226, 220, 214, 208, 202, 196, 160, 124, 88, 52];
const T_CL_INT:  &[u8] = &[231, 159, 123, 87, 51, 45, 39, 33, 27, 21];

// ── neon ──────────────────────────────────────────────────────────────────────
const T_NE_MP0: &[u8] = &[231, 225, 219, 213, 207, 201, 165, 129, 93, 57];
const T_NE_MP1: &[u8] = &[231, 159, 123, 87, 51, 45, 39, 33, 27];
const T_NE_MP2: &[u8] = &[231, 193, 155, 118, 82, 46, 40, 34];
const T_NE_MP3: &[u8] = &[226, 190, 154, 148, 112, 106, 70];
const T_NE_MISSILE_PALETTES: &[&[u8]] = &[T_NE_MP0, T_NE_MP1, T_NE_MP2, T_NE_MP3];
const T_NE_EXPL: &[u8] = &[231, 225, 219, 213, 207, 201, 165, 129, 93, 57, 21];
const T_NE_INT:  &[u8] = &[226, 220, 214, 208, 202, 196, 160, 124];

// ── cold ──────────────────────────────────────────────────────────────────────
const T_CO_MP0: &[u8] = &[231, 195, 159, 123, 87, 51, 45, 39];
const T_CO_MP1: &[u8] = &[159, 123, 87, 51, 45, 39, 33, 27];
const T_CO_MP2: &[u8] = &[231, 189, 153, 117, 81, 45, 33, 21];
const T_CO_MISSILE_PALETTES: &[&[u8]] = &[T_CO_MP0, T_CO_MP1, T_CO_MP2];
const T_CO_EXPL: &[u8] = &[231, 195, 159, 123, 87, 51, 45, 39, 33, 27, 21, 20, 19, 18, 17];
const T_CO_INT:  &[u8] = &[226, 220, 214, 208, 202, 196, 160];

// ── retro ─────────────────────────────────────────────────────────────────────
const T_RE_MP0: &[u8] = &[231, 229, 227, 220, 214, 208, 172, 136, 130, 94, 58];
const T_RE_MP1: &[u8] = &[172, 136, 130, 94, 58, 52];
const T_RE_MISSILE_PALETTES: &[&[u8]] = &[T_RE_MP0, T_RE_MP1];
const T_RE_EXPL: &[u8] = &[231, 229, 227, 226, 220, 214, 208, 172, 136, 130, 94, 58, 52];
const T_RE_INT:  &[u8] = &[46, 40, 34, 28, 22];

// ── plasma ────────────────────────────────────────────────────────────────────
const T_PL_MP0: &[u8] = &[196, 160, 124, 88, 52];
const T_PL_MP1: &[u8] = &[202, 166, 130, 94, 58];
const T_PL_MP2: &[u8] = &[226, 190, 154, 118, 82];
const T_PL_MP3: &[u8] = &[46, 40, 34, 28, 22];
const T_PL_MP4: &[u8] = &[51, 45, 39, 33, 27];
const T_PL_MP5: &[u8] = &[21, 20, 19, 18, 17];
const T_PL_MP6: &[u8] = &[201, 165, 129, 93, 57];
const T_PL_MISSILE_PALETTES: &[&[u8]] = &[
    T_PL_MP0, T_PL_MP1, T_PL_MP2, T_PL_MP3, T_PL_MP4, T_PL_MP5, T_PL_MP6,
];
const T_PL_EXPL: &[u8] = &[231, 226, 220, 46, 51, 21, 201, 196, 160, 124, 88, 52];
const T_PL_INT:  &[u8] = &[231, 255, 253, 251, 249, 247, 245, 243, 241];

// ── sunset (dusk sky: magenta → orange → gold) ────────────────────────────────
const T_SU_MP0: &[u8] = &[231, 225, 219, 213, 207, 171, 135, 99,  63,  57];   // magenta
const T_SU_MP1: &[u8] = &[231, 222, 216, 210, 204, 198, 162, 126, 90];        // rose-orange
const T_SU_MP2: &[u8] = &[226, 220, 214, 208, 202, 166, 130, 94];             // gold-amber
const T_SU_MISSILE_PALETTES: &[&[u8]] = &[T_SU_MP0, T_SU_MP1, T_SU_MP2];
const T_SU_EXPL: &[u8] = &[231, 225, 219, 213, 207, 201, 165, 129, 93, 57, 56, 55];
const T_SU_INT:  &[u8] = &[231, 195, 159, 123, 87, 51, 45];   // cool ice (contrast)

// ── toxic (radioactive green) ─────────────────────────────────────────────────
const T_TO_MP0: &[u8] = &[231, 193, 155, 118, 82, 46, 40, 34, 28];            // acid lime
const T_TO_MP1: &[u8] = &[226, 190, 154, 148, 112, 76, 70, 64];               // yellow-green
const T_TO_MP2: &[u8] = &[154, 148, 112, 76,  70,  64, 22];                   // dim lime
const T_TO_MISSILE_PALETTES: &[&[u8]] = &[T_TO_MP0, T_TO_MP1, T_TO_MP2];
const T_TO_EXPL: &[u8] = &[231, 193, 155, 118, 82, 46, 40, 34, 28, 22, 16];
const T_TO_INT:  &[u8] = &[226, 220, 214, 208, 202, 196, 160];   // orange (biohazard contrast)

// ── cyber (Blade Runner amber on dark blue) ───────────────────────────────────
const T_CY_MP0: &[u8] = &[231, 229, 220, 214, 208, 202, 166, 130, 94, 88];    // amber-white
const T_CY_MP1: &[u8] = &[214, 208, 202, 196, 160, 124, 88,  52];             // hot orange
const T_CY_MP2: &[u8] = &[226, 220, 178, 142, 100, 58];                       // gold
const T_CY_MISSILE_PALETTES: &[&[u8]] = &[T_CY_MP0, T_CY_MP1, T_CY_MP2];
const T_CY_EXPL: &[u8] = &[231, 229, 227, 220, 214, 208, 202, 166, 130, 94, 88, 52];
const T_CY_INT:  &[u8] = &[51, 45, 39, 33, 27, 21, 20, 19];   // electric blue

// ── void (stark near-monochrome) ─────────────────────────────────────────────
const T_VO_MP0: &[u8] = &[231, 255, 254, 253, 252, 251, 250, 249, 248];       // bright white
const T_VO_MP1: &[u8] = &[252, 248, 244, 240, 236, 234];                      // dim gray
const T_VO_MISSILE_PALETTES: &[&[u8]] = &[T_VO_MP0, T_VO_MP1];
const T_VO_EXPL: &[u8] = &[231, 255, 254, 252, 248, 244, 240, 238, 236, 234, 233, 232];
const T_VO_INT:  &[u8] = &[231, 252, 248, 244, 241, 238, 235];

// ── candy (soft pastels) ──────────────────────────────────────────────────────
const T_CA_MP0: &[u8] = &[231, 225, 219, 218, 212, 206, 200, 163, 127];       // rose-pink
const T_CA_MP1: &[u8] = &[231, 189, 183, 177, 171, 135, 99,  93];             // lavender
const T_CA_MP2: &[u8] = &[231, 195, 153, 117, 111, 75,  69,  33];             // baby blue
const T_CA_MP3: &[u8] = &[231, 222, 216, 210, 174, 138, 102, 66];             // peach
const T_CA_MISSILE_PALETTES: &[&[u8]] = &[T_CA_MP0, T_CA_MP1, T_CA_MP2, T_CA_MP3];
const T_CA_EXPL: &[u8] = &[231, 225, 219, 213, 207, 201, 171, 135, 99, 93, 57];
const T_CA_INT:  &[u8] = &[231, 195, 159, 123, 87, 51, 45];   // mint-cyan

fn theme_data(name: &str) -> ThemeData {
    match name {
        "neon"   => ThemeData {
            missile_palettes:   T_NE_MISSILE_PALETTES,
            expl_colors:        T_NE_EXPL,
            interceptor_colors: T_NE_INT,
            city_shades:  &[55, 93, 54],
            ground_color: 54,
            window_lit:   201, window_dark: 53, antenna_color: 226,
        },
        "cold"   => ThemeData {
            missile_palettes:   T_CO_MISSILE_PALETTES,
            expl_colors:        T_CO_EXPL,
            interceptor_colors: T_CO_INT,
            city_shades:  &[24, 31, 17],
            ground_color: 17,
            window_lit:   159, window_dark: 18, antenna_color: 51,
        },
        "retro"  => ThemeData {
            missile_palettes:   T_RE_MISSILE_PALETTES,
            expl_colors:        T_RE_EXPL,
            interceptor_colors: T_RE_INT,
            city_shades:  &[58, 94, 52],
            ground_color: 52,
            window_lit:   220, window_dark: 52, antenna_color: 220,
        },
        "plasma" => ThemeData {
            missile_palettes:   T_PL_MISSILE_PALETTES,
            expl_colors:        T_PL_EXPL,
            interceptor_colors: T_PL_INT,
            city_shades:  &[240, 244, 236],
            ground_color: 238,
            window_lit:   231, window_dark: 235, antenna_color: 231,
        },
        "sunset" => ThemeData {
            missile_palettes:   T_SU_MISSILE_PALETTES,
            expl_colors:        T_SU_EXPL,
            interceptor_colors: T_SU_INT,
            city_shades:  &[96, 90, 54],   // muted mauve / dusty purple / dark
            ground_color: 53,
            window_lit:   218, window_dark: 89, antenna_color: 225,
        },
        "toxic"  => ThemeData {
            missile_palettes:   T_TO_MISSILE_PALETTES,
            expl_colors:        T_TO_EXPL,
            interceptor_colors: T_TO_INT,
            city_shades:  &[22, 28, 16],   // dark green / slightly lighter / near-black
            ground_color: 22,
            window_lit:   82,  window_dark: 22,  antenna_color: 46,
        },
        "cyber"  => ThemeData {
            missile_palettes:   T_CY_MISSILE_PALETTES,
            expl_colors:        T_CY_EXPL,
            interceptor_colors: T_CY_INT,
            city_shades:  &[17, 18, 16],   // near-black blue / very dark blue / black
            ground_color: 16,
            window_lit:   208, window_dark: 52, antenna_color: 214,
        },
        "void"   => ThemeData {
            missile_palettes:   T_VO_MISSILE_PALETTES,
            expl_colors:        T_VO_EXPL,
            interceptor_colors: T_VO_INT,
            city_shades:  &[240, 244, 236],
            ground_color: 234,
            window_lit:   252, window_dark: 237, antenna_color: 231,
        },
        "candy"  => ThemeData {
            missile_palettes:   T_CA_MISSILE_PALETTES,
            expl_colors:        T_CA_EXPL,
            interceptor_colors: T_CA_INT,
            city_shades:  &[135, 99, 93],  // soft purple / medium / dark purple
            ground_color: 93,
            window_lit:   225, window_dark: 96, antenna_color: 218,
        },
        _        => ThemeData {  // classic
            missile_palettes:   T_CL_MISSILE_PALETTES,
            expl_colors:        T_CL_EXPL,
            interceptor_colors: T_CL_INT,
            city_shades:  &[34, 40, 28],
            ground_color: 28,
            window_lit:   226, window_dark: 58, antenna_color: 46,
        },
    }
}

// ── Building metadata ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
struct ColMeta {
    shade_idx: u8,
    windows:   bool,
    rel_col:   u8,
    antenna_h: u8,
    seed:      u32,
}

fn win_lit(rel_col: u8, row_from_ground: usize, seed: u32, phase: f32) -> bool {
    let p = (phase * 5.0) as u32;
    ((rel_col as u32).wrapping_mul(1009)
        .wrapping_add((row_from_ground as u32).wrapping_mul(1013))
        .wrapping_add(seed)
        .wrapping_add(p)) % 7 < 5
}

// ── Deterministic star field ──────────────────────────────────────────────────

/// Returns `Some((char, color))` if this cell should show a star.
fn star_at(c: usize, r: usize) -> Option<(char, u8)> {
    let h = (c as u64).wrapping_mul(2654435761)
             .wrapping_add((r as u64).wrapping_mul(2246822519))
             .wrapping_mul(6364136223846793005);
    let pct = h % 200;
    if      pct < 1  { Some(('✦', 240)) }
    else if pct < 5  { Some(('·', 236)) }
    else if pct < 8  { Some(('·', 237)) }
    else             { None }
}

// ── Data types ────────────────────────────────────────────────────────────────

struct Missile {
    id:          u64,
    x:           f32,
    y:           f32,
    dx:          f32,
    vy:          f32,
    palette_idx: usize,
    intercepted: bool,
}

struct Interceptor {
    x:          f32,
    y:          f32,
    vx:         f32,
    vy:         f32,
    target_id:  u64,
    tx:         f32,
    ty:         f32,
    launch_col: usize,
    dead:       bool,
}

struct Explosion {
    cx:            f32,
    cy:            f32,
    radius:        f32,
    max_radius:    f32,
    life:          f32,
    smoke_spawned: bool,
}

/// Upward-drifting smoke particle left by large explosions.
struct Smoke {
    x:    f32,
    y:    f32,
    vx:   f32,
    vy:   f32,   // negative = upward
    life: f32,   // 1.0 → 0.0
}

// ── Main struct ───────────────────────────────────────────────────────────────

pub struct MissilesViz {
    missiles:     Vec<Missile>,
    interceptors: Vec<Interceptor>,
    explosions:   Vec<Explosion>,
    smoke:        Vec<Smoke>,
    bars:         SpectrumBars,
    source:       String,
    next_id:      u64,
    beat:         BeatDetector,
    spawn_cool:   f32,
    // City
    city:         Vec<u8>,
    city_target:  Vec<u8>,
    city_meta:    Vec<ColMeta>,
    city_regrow:  Vec<f32>,
    city_cols:    usize,
    win_phase:    f32,
    // Stats
    audio_history:       VecDeque<f32>,
    missiles_intercepted: u32,
    missiles_hit:         u32,
    // Config
    gain:            f32,
    speed:           f32,
    intercept_rate:  f32,
    intercept_speed: f32,
    show_stats:      bool,
    stars_enabled:   bool,
    smoke_enabled:   bool,
    trail_length:    usize,
    explosion_scale: f32,
    max_missiles:    usize,
    diagonal:        String,
    city_density:    String,
    city_density_cur: String,  // last value used for regen (triggers regen on change)
    theme:           String,
}

impl MissilesViz {
    pub fn new(source: &str) -> Self {
        Self {
            missiles:     Vec::new(),
            interceptors: Vec::new(),
            explosions:   Vec::new(),
            smoke:        Vec::new(),
            bars:         SpectrumBars::new(80),
            source:       source.to_string(),
            next_id:      1,
            beat: BeatDetector::new({
                let mut cfg = BeatDetectorConfig::simple();
                cfg.cooldown_secs = 0.10;
                cfg.min_onset = 0.003;
                cfg.avg_alpha = 0.12;
                cfg
            }),
            spawn_cool:   0.0,
            city:         Vec::new(),
            city_target:  Vec::new(),
            city_meta:    Vec::new(),
            city_regrow:  Vec::new(),
            city_cols:    0,
            win_phase:    0.0,
            audio_history:        VecDeque::new(),
            missiles_intercepted: 0,
            missiles_hit:         0,
            gain:            1.0,
            speed:           1.0,
            intercept_rate:  0.55,
            intercept_speed: 1.0,
            show_stats:      true,
            stars_enabled:   true,
            smoke_enabled:   true,
            trail_length:    12,
            explosion_scale: 1.0,
            max_missiles:    60,
            diagonal:        "mixed".to_string(),
            city_density:    "normal".to_string(),
            city_density_cur: String::new(),
            theme:           "classic".to_string(),
        }
    }

    fn regen_city(&mut self, cols: usize) {
        let density = self.city_density.as_str();
        // Vary LCG seed so different densities produce different layouts
        let dseed: u64 = match density { "sparse" => 0xAABB, "dense" => 0xCCDD, _ => 0x1122 };
        let mut lcg: u64 = (0x5851_f42d_4c95_7f2d ^ (cols as u64 * 6364136223846793005))
                           .wrapping_add(dseed);
        let next = |s: &mut u64| -> f32 {
            *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((*s >> 33) as f32) / (u32::MAX as f32)
        };

        // Density-dependent parameters
        let gap_fill   = match density { "sparse" => 0.35f32, "dense" => 0.72, _ => 0.55 };
        let max_gap    = match density { "sparse" => 4usize,  "dense" => 1,    _ => 2    };
        let h_tower    = match density { "sparse" => (5.0f32, 3.0), "dense" => (9.0, 5.0), _ => (7.0, 5.0) };
        let h_office   = match density { "sparse" => (2.0f32, 2.0), "dense" => (3.0, 4.0), _ => (3.0, 3.0) };
        let h_block    = match density { "sparse" => (1.0f32, 1.0), "dense" => (2.0, 2.0), _ => (2.0, 1.5) };

        let mut city = vec![0u8;              cols];
        let mut meta = vec![ColMeta::default(); cols];
        let mut c    = 0usize;

        while c < cols {
            let gap = 1 + (next(&mut lcg) * max_gap as f32) as usize;
            c += gap;
            if c >= cols { break; }

            if next(&mut lcg) > gap_fill { c += 1; continue; }

            let btype = next(&mut lcg);
            let seed  = (lcg >> 32) as u32;
            let shade = (next(&mut lcg) * 3.0) as u8;

            let (w, h, ant_h, has_windows) = if btype < 0.15 {
                let w   = 1 + (next(&mut lcg) * 1.5) as usize;
                let h   = (h_tower.0 + next(&mut lcg) * h_tower.1) as u8;
                let ant = 1 + (next(&mut lcg) * 2.5) as u8;
                (w, h, ant, w >= 2)
            } else if btype < 0.45 {
                let w = 3 + (next(&mut lcg) * 2.5) as usize;
                let h = (h_office.0 + next(&mut lcg) * h_office.1) as u8;
                (w, h, 0u8, true)
            } else {
                let w = 4 + (next(&mut lcg) * 4.0) as usize;
                let h = (h_block.0 + next(&mut lcg) * h_block.1) as u8;
                (w, h, 0u8, false)
            };

            let w = w.min(cols - c);
            if w == 0 { c += 1; continue; }

            for i in 0..w {
                city[c + i] = h;
                let is_edge  = i == 0 || i + 1 == w;
                let win_col  = has_windows && !is_edge && w > 2;
                let this_ant = if ant_h > 0 && i == w / 2 { ant_h } else { 0 };
                meta[c + i] = ColMeta {
                    shade_idx: shade,
                    windows:   win_col,
                    rel_col:   i as u8,
                    antenna_h: this_ant,
                    seed,
                };
            }
            c += w;
        }

        self.city_target     = city.clone();
        self.city            = city;
        self.city_meta       = meta;
        self.city_regrow     = vec![0.0f32; cols];
        self.city_cols       = cols;
        self.city_density_cur = self.city_density.clone();
    }

    fn blast_city(&mut self, cx: f32, cy: f32, radius: f32, ground: usize) {
        if cy < (ground as f32) - radius * 1.2 { return; }
        let cols        = self.city.len();
        let horiz_reach = (radius * 2.2) as isize;
        let cx_i        = cx as isize;
        for dc in -horiz_reach..=horiz_reach {
            let c = cx_i + dc;
            if c < 0 || c as usize >= cols { continue; }
            let c       = c as usize;
            let dist    = (dc as f32 * 0.5).abs();
            let falloff = (1.0 - dist / (horiz_reach as f32 * 0.5 + 0.1)).max(0.0);
            let dmg     = (falloff * 3.0 + 0.5) as u8;
            self.city[c] = self.city[c].saturating_sub(dmg);
            if c < self.city_regrow.len() { self.city_regrow[c] = 0.0; }
        }
    }

    fn random_dx(rng: &mut impl Rng, diagonal: &str) -> f32 {
        let tier: f32 = rng.gen_range(0.0..1.0);
        let mag = match diagonal {
            "straight" => {
                // Mostly vertical, rare gentle slope, never steep
                if tier < 0.70      { rng.gen_range(0.00f32..0.05) }
                else                { rng.gen_range(0.05f32..0.18) }
            }
            "wild" => {
                // Rare verticals, mostly steep/extreme
                if tier < 0.08      { rng.gen_range(0.00f32..0.08) }
                else if tier < 0.25 { rng.gen_range(0.08f32..0.30) }
                else if tier < 0.55 { rng.gen_range(0.30f32..0.65) }
                else                { rng.gen_range(0.65f32..1.10) }
            }
            _ /* mixed */ => {
                if tier < 0.35      { rng.gen_range(0.00f32..0.08) }
                else if tier < 0.65 { rng.gen_range(0.08f32..0.30) }
                else if tier < 0.85 { rng.gen_range(0.30f32..0.60) }
                else                { rng.gen_range(0.60f32..0.90) }
            }
        };
        if rng.gen_range(0u8..2) == 0 { mag } else { -mag }
    }

    fn city_health(&self) -> f32 {
        let cur: usize = self.city.iter().map(|&h| h as usize).sum();
        let tgt: usize = self.city_target.iter().map(|&h| h as usize).sum();
        if tgt == 0 { 1.0 } else { (cur as f32 / tgt as f32).clamp(0.0, 1.0) }
    }

    /// Build the rich stats line.
    ///
    /// Layout (left → right):
    ///   ♫ [sparkline]  city [bar] [%]  ↓[hits] ✓[ok]   [name · fps]  [hints]
    fn stats_line(&self, cols: usize, fps: f32) -> String {
        // ── Audio sparkline ───────────────────────────────────────────────────
        let spark_len = SPARK_LEN.min(self.audio_history.len());
        let history: Vec<f32> = self.audio_history.iter()
            .rev().take(spark_len).cloned().collect::<Vec<_>>()
            .into_iter().rev().collect();
        let peak = history.iter().cloned().fold(0.0f32, f32::max).max(0.001);

        let mut spark_str  = String::new();
        let mut spark_vis  = 0usize;
        spark_str.push_str("♫ ");
        spark_vis += 2;
        for v in &history {
            let t   = v / peak;
            let idx = ((t * (SPARK_CHARS.len() - 1) as f32) as usize).min(SPARK_CHARS.len() - 1);
            let col = if t > 0.85 { 196u8 }
                      else if t > 0.65 { 214 }
                      else if t > 0.40 { 226 }
                      else if t > 0.20 { 51  }
                      else             { 27  };
            spark_str.push_str(&format!("\x1b[38;5;{col}m{}", SPARK_CHARS[idx]));
            spark_vis += 1;
        }
        spark_str.push_str("\x1b[0m");

        // ── City health bar ───────────────────────────────────────────────────
        let health     = self.city_health();
        let bar_width  = 8usize;
        let filled     = (health * bar_width as f32).round() as usize;
        let bar_color  = if health > 0.6 { 46u8 } else if health > 0.3 { 220 } else { 196 };
        let mut city_str = String::new();
        let mut city_vis = 0usize;
        city_str.push_str("  city ");
        city_vis += 7;
        city_str.push_str(&format!("\x1b[38;5;{bar_color}m"));
        for i in 0..bar_width {
            city_str.push(if i < filled { '█' } else { '░' });
            city_vis += 1;
        }
        city_str.push_str("\x1b[0m");
        let pct_str  = format!(" {:3.0}%", health * 100.0);
        city_str.push_str(&pct_str);
        city_vis += pct_str.len();

        // ── Hit / intercept counters ──────────────────────────────────────────
        let total    = self.missiles_hit + self.missiles_intercepted;
        let acc_pct  = if total == 0 { 0 } else {
            (self.missiles_intercepted as f32 / total as f32 * 100.0).round() as u32
        };
        let counter_raw = format!(
            "  \x1b[38;5;196m↓{}\x1b[0m \x1b[38;5;46m✓{}\x1b[0m \x1b[38;5;240m{}%\x1b[0m",
            self.missiles_hit, self.missiles_intercepted, acc_pct
        );
        let counter_vis = 2
            + 1 + self.missiles_hit.to_string().len()
            + 1 + 1 + self.missiles_intercepted.to_string().len()
            + 1 + acc_pct.to_string().len() + 1;

        // ── Name + fps ────────────────────────────────────────────────────────
        let name_fps     = format!("  {} · {}fps  ", self.name(), fps as u32);
        let name_fps_vis = name_fps.len();

        // ── Hints ─────────────────────────────────────────────────────────────
        let hints     = "  [Esc] visualizers  [F1] settings  [q] quit  ";
        let hints_vis = hints.len();

        // ── Assemble ──────────────────────────────────────────────────────────
        let content_vis = spark_vis + city_vis + counter_vis + name_fps_vis;
        let total_vis   = content_vis + hints_vis;
        let padding     = if cols > total_vis { " ".repeat(cols - total_vis) } else { String::new() };

        format!(
            "\x1b[2m\x1b[38;5;240m{spark_str}{city_str}{counter_raw}{name_fps}{padding}{hints}\x1b[0m"
        )
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for MissilesViz {
    fn name(&self)        -> &str { "missiles" }
    fn description(&self) -> &str { "Missile Command: audio-driven missile rain, interceptors, and city damage" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "missiles",
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
                    "type": "float",
                    "value": 1.0,
                    "min": 0.2,
                    "max": 3.0
                },
                {
                    "name": "intercept_rate",
                    "display_name": "Intercept %",
                    "type": "float",
                    "value": 0.55,
                    "min": 0.0,
                    "max": 1.0
                },
                {
                    "name": "intercept_speed",
                    "display_name": "Interceptor Speed",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.3,
                    "max": 3.0
                },
                {
                    "name": "max_missiles",
                    "display_name": "Max Missiles",
                    "type": "int",
                    "value": 60,
                    "min": 5,
                    "max": 80
                },
                {
                    "name": "trail_length",
                    "display_name": "Trail Length",
                    "type": "int",
                    "value": 12,
                    "min": 2,
                    "max": 20
                },
                {
                    "name": "explosion_scale",
                    "display_name": "Explosion Scale",
                    "type": "float",
                    "value": 1.0,
                    "min": 0.2,
                    "max": 3.0
                },
                {
                    "name": "diagonal",
                    "display_name": "Diagonal",
                    "type": "enum",
                    "value": "mixed",
                    "variants": ["straight", "mixed", "wild"]
                },
                {
                    "name": "city_density",
                    "display_name": "City Density",
                    "type": "enum",
                    "value": "normal",
                    "variants": ["sparse", "normal", "dense"]
                },
                {
                    "name": "stars_enabled",
                    "display_name": "Stars",
                    "type": "bool",
                    "value": true
                },
                {
                    "name": "smoke_enabled",
                    "display_name": "Smoke",
                    "type": "bool",
                    "value": true
                },
                {
                    "name": "show_stats",
                    "display_name": "Show Stats",
                    "type": "bool",
                    "value": true
                },
                {
                    "name": "theme",
                    "display_name": "Theme",
                    "type": "enum",
                    "value": "classic",
                    "variants": ["classic", "neon", "cold", "retro", "plasma", "sunset", "toxic", "cyber", "void", "candy"]
                }
            ]
        }).to_string()
    }

    fn set_config(&mut self, json: &str) -> Result<String, String> {
        let merged = merge_config(&self.get_default_config(), json);
        let val: serde_json::Value = serde_json::from_str(&merged)
            .map_err(|e| format!("JSON parse error: {e}"))?;
        if let Some(cfg) = val["config"].as_array() {
            for entry in cfg {
                match entry["name"].as_str() {
                    Some("gain")            => self.gain            = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    Some("speed")           => self.speed           = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    Some("intercept_rate")  => self.intercept_rate  = entry["value"].as_f64().unwrap_or(0.4) as f32,
                    Some("intercept_speed") => self.intercept_speed = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    Some("max_missiles")    => self.max_missiles    = entry["value"].as_i64().unwrap_or(40) as usize,
                    Some("trail_length")    => self.trail_length    = entry["value"].as_i64().unwrap_or(9) as usize,
                    Some("explosion_scale") => self.explosion_scale = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    Some("stars_enabled")   => self.stars_enabled   = entry["value"].as_bool().unwrap_or(true),
                    Some("smoke_enabled")   => self.smoke_enabled   = entry["value"].as_bool().unwrap_or(true),
                    Some("show_stats")      => self.show_stats      = entry["value"].as_bool().unwrap_or(true),
                    Some("diagonal")        => {
                        if let Some(s) = entry["value"].as_str() { self.diagonal = s.to_string(); }
                    }
                    Some("city_density")    => {
                        if let Some(s) = entry["value"].as_str() { self.city_density = s.to_string(); }
                    }
                    Some("theme")           => {
                        if let Some(s) = entry["value"].as_str() { self.theme = s.to_string(); }
                    }
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        self.bars.resize(size.cols as usize);
        if self.city_cols != size.cols as usize {
            self.regen_city(size.cols as usize);
        }
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        let rows   = size.rows as usize;
        let cols   = size.cols as usize;
        let vis    = rows.saturating_sub(1);
        let ground = vis.saturating_sub(1);

        if self.city_cols != cols || self.city_density != self.city_density_cur {
            self.regen_city(cols);
        }

        self.bars.resize(cols);
        let scaled: Vec<f32> = audio.fft.iter().map(|v| v * self.gain).collect();
        self.bars.update(&scaled, dt);

        let n       = self.bars.smoothed.len().max(1);
        let bass    = self.bars.smoothed[..n / 6].iter().copied().sum::<f32>()
                      / (n / 6).max(1) as f32;
        let overall = self.bars.smoothed.iter().copied().sum::<f32>() / n as f32;

        // Audio history for sparkline
        self.audio_history.push_back(overall);
        while self.audio_history.len() > SPARK_LEN { self.audio_history.pop_front(); }

        self.win_phase += (0.3 + overall * 2.5) * dt;

        self.beat.update(&audio.fft, dt);
        let is_beat = self.beat.is_beat();

        let n_palettes = theme_data(&self.theme).missile_palettes.len();

        // ── Spawn missiles ────────────────────────────────────────────────────
        self.spawn_cool -= dt;
        let base_interval = (0.32 - bass * 0.38 - overall * 0.22).clamp(0.04, 0.36);
        if (self.spawn_cool <= 0.0 || is_beat) && self.missiles.len() < self.max_missiles {
            let count = if is_beat { rand::thread_rng().gen_range(2usize..=6) } else { 1 };
            let mut rng = rand::thread_rng();
            for _ in 0..count {
                let x           = rng.gen_range(0..cols) as f32;
                let dx          = Self::random_dx(&mut rng, &self.diagonal.clone());
                let vy          = (vis as f32) * (0.28 + overall * 0.50) * self.speed;
                let palette_idx = rng.gen_range(0..n_palettes);
                let id          = self.next_id;
                self.next_id   += 1;
                self.missiles.push(Missile { id, x, y: 0.0, dx, vy, palette_idx, intercepted: false });

                if rng.r#gen::<f32>() < self.intercept_rate {
                    let launch_c = (0..cols)
                        .filter(|&c| c < self.city.len() && self.city[c] > 0)
                        .min_by_key(|&c| (c as isize - x as isize).unsigned_abs())
                        .unwrap_or(x as usize);
                    let h        = if launch_c < self.city.len() { self.city[launch_c] as usize } else { 0 };
                    let launch_y = (ground.saturating_sub(h)) as f32;
                    let rows_left = vis as f32 / vy.max(0.001);
                    let target_c  = (x + dx * vy * rows_left).clamp(0.0, (cols - 1) as f32);
                    let ddx       = target_c - launch_c as f32;
                    let ddy       = ground as f32 - launch_y;
                    let dist      = (ddx * ddx + ddy * ddy).sqrt().max(0.001);
                    let isp       = vis as f32 * 0.80 * self.speed * self.intercept_speed;
                    self.interceptors.push(Interceptor {
                        x: launch_c as f32, y: launch_y,
                        vx: (ddx / dist) * isp, vy: (ddy / dist) * isp,
                        target_id: id, tx: x, ty: 0.0,
                        launch_col: launch_c, dead: false,
                    });
                }
            }
            self.spawn_cool = base_interval;
        }

        // ── Update interceptors ───────────────────────────────────────────────
        let missile_snap: Vec<(u64, f32, f32)> =
            self.missiles.iter().map(|m| (m.id, m.x, m.y)).collect();
        let isp = vis as f32 * 0.80 * self.speed * self.intercept_speed;

        for int_ in &mut self.interceptors {
            if let Some(&(_, mx, my)) = missile_snap.iter().find(|&&(id, _, _)| id == int_.target_id) {
                int_.tx = mx; int_.ty = my;
                let ddx  = mx - int_.x;
                let ddy  = my - int_.y;
                let dist = (ddx * ddx + ddy * ddy).sqrt().max(0.001);
                int_.vx  = (ddx / dist) * isp;
                int_.vy  = (ddy / dist) * isp;
            }
            int_.x += int_.vx * dt;
            int_.y += int_.vy * dt;
            let alive = missile_snap.iter().any(|&(id, _, _)| id == int_.target_id);
            if int_.y < -2.0 || int_.x < -2.0 || int_.x > cols as f32 + 2.0
                || (!alive && int_.y >= int_.ty)
            {
                int_.dead = true;
            }
        }

        // ── Detect interceptor hits ───────────────────────────────────────────
        let mut int_remove: Vec<usize>      = Vec::new();
        let mut mis_remove: Vec<usize>      = Vec::new();
        let mut small_expl: Vec<(f32, f32)> = Vec::new();

        'outer: for (ii, int_) in self.interceptors.iter().enumerate() {
            if int_.dead { int_remove.push(ii); continue; }
            for (mi, m) in self.missiles.iter().enumerate() {
                let dr = int_.y - m.y;
                let dc = (int_.x - m.x) * 0.5;
                if (dr * dr + dc * dc).sqrt() < 1.8 {
                    int_remove.push(ii);
                    mis_remove.push(mi);
                    small_expl.push((int_.x, int_.y));
                    continue 'outer;
                }
            }
        }
        for &mi in &mis_remove {
            if mi < self.missiles.len() { self.missiles[mi].intercepted = true; }
        }
        self.missiles_intercepted += mis_remove.len() as u32;
        int_remove.sort_unstable(); int_remove.dedup();
        for &ii in int_remove.iter().rev() {
            if ii < self.interceptors.len() { self.interceptors.swap_remove(ii); }
        }
        for (sx, sy) in small_expl {
            self.explosions.push(Explosion {
                cx: sx, cy: sy, radius: 0.0,
                max_radius: (3.5 + bass * 6.0).clamp(3.0, 12.0),
                life: 1.0, smoke_spawned: true,  // no smoke for small blasts
            });
        }

        // ── Update missiles ───────────────────────────────────────────────────
        let mut to_explode: Vec<(f32, f32, f32)> = Vec::new();
        self.missiles.retain_mut(|m| {
            if m.intercepted { return false; }
            m.y += m.vy * dt;
            m.x  = (m.x + m.dx * m.vy * dt).clamp(0.0, (cols - 1) as f32);
            let col = m.x as usize;
            let imp = {
                let c0 = col;
                let c1 = col.saturating_sub(1);
                let c2 = (col + 1).min(cols - 1);
                [c0, c1, c2].iter().map(|&c| {
                    let h = if c < self.city.len() { self.city[c] as usize } else { 0 };
                    ground.saturating_sub(h)
                }).min().unwrap_or(ground)
            };
            if m.y as usize >= imp || m.y as usize >= vis {
                let max_r = ((5.0 + bass * 15.0 + overall * 9.0) * self.explosion_scale).clamp(2.0, 32.0);
                to_explode.push((m.x, m.y.min(imp as f32), max_r));
                false
            } else {
                true
            }
        });
        self.missiles_hit += to_explode.len() as u32;
        for (cx, cy, max_r) in to_explode {
            self.blast_city(cx, cy, max_r, ground);
            self.explosions.push(Explosion {
                cx, cy, radius: 0.0, max_radius: max_r, life: 1.0,
                smoke_spawned: false,
            });
        }

        // ── Update explosions + spawn smoke ───────────────────────────────────
        let mut new_smoke: Vec<Smoke> = Vec::new();
        let mut rng = rand::thread_rng();
        for e in &mut self.explosions {
            if e.radius < e.max_radius {
                e.radius += e.max_radius * 5.5 * dt;
                if e.radius > e.max_radius { e.radius = e.max_radius; }
            } else {
                // Spawn smoke once when the ring peaks, only for large blasts
                if !e.smoke_spawned && e.max_radius > 3.0 && self.smoke_enabled {
                    let count = 8 + (e.max_radius / 2.5) as usize;
                    for _ in 0..count {
                        let angle = rng.gen_range(0.0f32..std::f32::consts::TAU);
                        let r     = rng.gen_range(0.0f32..e.max_radius * 0.6);
                        new_smoke.push(Smoke {
                            x:    e.cx + angle.cos() * r * 2.0,
                            y:    e.cy + angle.sin() * r,
                            vx:   rng.gen_range(-0.8f32..0.8),
                            vy:   rng.gen_range(-2.5f32..-0.5),
                            life: rng.gen_range(0.6f32..1.4),
                        });
                    }
                    e.smoke_spawned = true;
                }
                e.life -= dt * 1.8;
            }
        }
        self.explosions.retain(|e| e.life > 0.0);
        self.smoke.extend(new_smoke);

        // ── Update smoke ──────────────────────────────────────────────────────
        for s in &mut self.smoke {
            s.x    += s.vx * dt;
            s.y    += s.vy * dt;
            s.life -= dt * 0.7;
        }
        self.smoke.retain(|s| s.life > 0.0);

        // ── Regrow buildings ──────────────────────────────────────────────────
        let quiet_factor = (1.0 - overall * 3.0).clamp(0.0, 1.0);
        let regrow_rate  = 0.25 * quiet_factor;
        if regrow_rate > 0.0 && self.city.len() == self.city_target.len() {
            for c in 0..self.city.len() {
                if self.city[c] < self.city_target[c] {
                    self.city_regrow[c] += regrow_rate * dt;
                    if self.city_regrow[c] >= 1.0 {
                        self.city[c]        += 1;
                        self.city_regrow[c]  = 0.0;
                    }
                }
            }
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows   = size.rows as usize;
        let cols   = size.cols as usize;
        let vis    = rows.saturating_sub(1);
        let ground = vis.saturating_sub(1);

        let td       = theme_data(&self.theme);
        let n_shades = td.city_shades.len().max(1);

        let mut grid: Vec<Vec<(char, u8, bool)>> = vec![vec![(' ', 0, false); cols]; vis];

        // ── Stars ─────────────────────────────────────────────────────────────
        if self.stars_enabled {
            let sky_limit = vis.saturating_sub(vis / 4);
            for r in 0..sky_limit {
                for c in 0..cols {
                    if let Some((ch, color)) = star_at(c, r) {
                        grid[r][c] = (ch, color, false);
                    }
                }
            }
        }

        // ── Explosions ────────────────────────────────────────────────────────
        for e in &self.explosions {
            // On the very first frames (life > 0.80) fill the interior solid for a
            // bright camera-flash effect; once the ring expands, hollow it out.
            let inner   = if e.life > 0.80 { 0.0 } else { (e.radius - 1.4).max(0.0) };
            let outer   = e.radius + 0.8;
            let row_min = (e.cy - outer - 1.0).max(0.0) as usize;
            let row_max = ((e.cy + outer + 1.0) as usize + 1).min(vis);
            let col_min = (e.cx - (outer + 1.0) * 2.0).max(0.0) as usize;
            let col_max = ((e.cx + (outer + 1.0) * 2.0) as usize + 1).min(cols);

            for r in row_min..row_max {
                for c in col_min..col_max {
                    let dr   = r as f32 - e.cy;
                    let dc   = (c as f32 - e.cx) * 0.5;
                    let dist = (dr * dr + dc * dc).sqrt();
                    if dist < inner || dist > outer { continue; }

                    let ring_frac = if e.life > 0.80 {
                        // filled flash: intensity falls off from centre outward
                        (1.0 - dist / outer.max(0.1)).max(0.0)
                    } else {
                        1.0 - ((dist - e.radius).abs() / 1.4).min(1.0)
                    };
                    let intensity = ring_frac * e.life;
                    if intensity < 0.06 { continue; }

                    let ch = if intensity > 0.88 { '@' }
                             else if intensity > 0.72 { '#' }
                             else if intensity > 0.52 { '*' }
                             else if intensity > 0.30 { '+' }
                             else { '·' };
                    let pi    = ((1.0 - intensity) * (td.expl_colors.len() - 1) as f32) as usize;
                    let color = td.expl_colors[pi.min(td.expl_colors.len() - 1)];
                    if grid[r][c].1 < 100 || intensity > 0.45 {
                        grid[r][c] = (ch, color, intensity > 0.55);
                    }
                }
            }
            if e.radius < e.max_radius * 0.45 {
                let cr = e.cy as usize;
                let cc = e.cx as usize;
                if cr < vis && cc < cols { grid[cr][cc] = ('@', 231, true); }
            }
        }

        // ── Smoke ─────────────────────────────────────────────────────────────
        for s in self.smoke.iter().filter(|_| self.smoke_enabled) {
            let sr = s.y as usize;
            let sc = s.x as usize;
            if sr >= vis || sc >= cols { continue; }
            if grid[sr][sc].0 != ' ' && grid[sr][sc].1 > 100 { continue; } // don't cover missiles
            let color = if s.life > 0.8 { 250u8 }
                        else if s.life > 0.55 { 246 }
                        else if s.life > 0.35 { 242 }
                        else { 238 };
            grid[sr][sc] = ('·', color, false);
        }

        // ── Missiles ─────────────────────────────────────────────────────────
        for m in &self.missiles {
            let tip_r = m.y as usize;
            let tip_c = m.x as usize;
            if tip_r >= vis || tip_c >= cols { continue; }

            let pal = td.missile_palettes[m.palette_idx % td.missile_palettes.len()];
            grid[tip_r][tip_c] = ('*', pal[0], true);

            let trail_ch = if m.dx.abs() < 0.08        { '|' }
                           else if m.dx.abs() < 0.45   { if m.dx > 0.0 { '\\' } else { '/' } }
                           else                         { if m.dx > 0.0 { '»' } else { '«' } };
            let trail = self.trail_length;
            for t in 1..=trail {
                let tr = match tip_r.checked_sub(t) { Some(r) => r, None => break };
                let tc = (m.x - m.dx * t as f32).round() as isize;
                if tc < 0 || tc as usize >= cols { continue; }
                let tc        = tc as usize;
                let trail_idx = (t * pal.len() / (trail + 1)).min(pal.len() - 1);
                // Overwrite stars/smoke but not other missiles/explosions
                if grid[tr][tc].1 < 150 {
                    grid[tr][tc] = (trail_ch, pal[trail_idx], false);
                }
            }
        }

        // ── Interceptors ─────────────────────────────────────────────────────
        let ipal = td.interceptor_colors;
        for int_ in &self.interceptors {
            let tip_r = int_.y as usize;
            let tip_c = int_.x as usize;
            if tip_r >= vis || tip_c >= cols { continue; }

            grid[tip_r][tip_c] = ('^', ipal[0], true);

            let inv_len = (int_.vx * int_.vx + int_.vy * int_.vy).sqrt().max(0.001);
            let step_r  = -int_.vy / inv_len;
            let step_c  = -int_.vx / inv_len;
            let trail   = 4usize;
            for t in 1..=trail {
                let tr_f = int_.y + step_r * t as f32;
                let tc_f = int_.x + step_c * t as f32;
                if tr_f < 0.0 { break; }
                let tr = tr_f as usize;
                let tc = tc_f.round() as isize;
                if tr >= vis || tc < 0 || tc as usize >= cols { continue; }
                let tc  = tc as usize;
                let idx = (t * ipal.len() / (trail + 1)).min(ipal.len() - 1);
                let ch  = if step_c.abs() < 0.15 { '|' }
                          else if step_c > 0.0   { '\\' }
                          else                   { '/' };
                if grid[tr][tc].1 < 150 { grid[tr][tc] = (ch, ipal[idx], false); }
            }
        }

        // ── City silhouette ───────────────────────────────────────────────────
        // Collect which columns have an active interceptor launched from them
        // (used to show launch-pad markers)
        let launch_cols: Vec<usize> = self.interceptors.iter()
            .filter(|i| !i.dead)
            .map(|i| i.launch_col)
            .collect();

        for c in 0..cols.min(self.city.len()) {
            let cur_h = self.city[c] as usize;
            let tgt_h = self.city_target[c] as usize;
            let m     = if c < self.city_meta.len() { self.city_meta[c] } else { ColMeta::default() };
            let base_color = td.city_shades[(m.shade_idx as usize) % n_shades];

            // Antenna
            if cur_h > 0 && cur_h == tgt_h && m.antenna_h > 0 {
                for a in 1..=(m.antenna_h as usize) {
                    let r = ground.saturating_sub(cur_h + a);
                    if r >= vis || grid[r][c].0 != ' ' { continue; }
                    let ch = if a == m.antenna_h as usize { '╻' } else { '│' };
                    grid[r][c] = (ch, td.antenna_color, a == m.antenna_h as usize);
                }
            }

            // Launch pad marker — shown at the top of the building when an
            // interceptor is currently in flight from this column
            if cur_h > 0 && launch_cols.contains(&c) {
                let pad_r = ground.saturating_sub(cur_h);
                if pad_r < vis && grid[pad_r][c].0 == ' ' {
                    grid[pad_r][c] = ('╦', td.antenna_color, true);
                }
            }

            // Building body
            for row_off in 0..cur_h {
                let r = ground.saturating_sub(row_off + 1);
                if r >= vis || grid[r][c].0 != ' ' { continue; }

                let is_top     = row_off + 1 == cur_h;
                let is_damaged = is_top && cur_h < tgt_h;
                let is_gnd     = row_off == 0;
                let can_window = m.windows && !is_top && !is_gnd;

                let (ch, color, bold) = if is_damaged {
                    let rubble = if (c + row_off) % 2 == 0 { '▄' } else { '░' };
                    (rubble, td.city_shades[2 % n_shades], false)
                } else if is_top {
                    ('▀', base_color, false)
                } else if can_window {
                    if win_lit(m.rel_col, row_off, m.seed, self.win_phase) {
                        ('▓', td.window_lit, false)
                    } else {
                        ('░', td.window_dark, false)
                    }
                } else if is_gnd && tgt_h > 0 {
                    ('█', td.city_shades[2 % n_shades], false)
                } else {
                    ('█', base_color, false)
                };
                grid[r][c] = (ch, color, bold);
            }

            if ground < vis && grid[ground][c].0 == ' ' {
                grid[ground][c] = ('▄', td.ground_color, false);
            }
        }

        // ── Assemble strings ──────────────────────────────────────────────────
        let mut lines = Vec::with_capacity(rows);
        for r in 0..vis {
            let mut line = String::with_capacity(cols * 14);
            for c in 0..cols {
                let (ch, color, bold) = grid[r][c];
                if ch == ' ' && color == 0 {
                    line.push(' ');
                } else {
                    let b = if bold { "\x1b[1m" } else { "" };
                    line.push_str(&format!("{b}\x1b[38;5;{color}m{ch}\x1b[0m"));
                }
            }
            lines.push(line);
        }

        let bottom = if self.show_stats {
            self.stats_line(cols, fps)
        } else {
            status_bar(cols, fps, self.name(), &self.source, "")
        };
        lines.push(bottom);
        pad_frame(lines, rows, cols)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(MissilesViz::new(""))]
}
