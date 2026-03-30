/// missiles.rs — Missile Command / Penetrate-style audio visualizer.
///
/// Missiles rain down from the top; interceptors launch from surviving buildings
/// to shoot them down.  Buildings take damage from ground-level explosions and
/// slowly repair themselves during quiet passages.
///
/// Building types (deterministic from terminal width):
///   obelisk    — very narrow (1), very tall (20–28), large antenna, no windows
///   skyscraper — narrow (1–2), very tall (14–22), windows, prominent antenna
///   tower      — narrow (1–2), medium-tall (7–12), small antenna
///   office     — medium (3–5), stepped crown silhouette, lit/dark windows
///   cathedral  — wide (5–8), peaked center profile, windows on interior cols
///   factory    — wide (4–7), flat roof, chimney antenna on last column
///   block      — wide (4–8), low (2–4), brutalist flat, no windows
///   slab       — very wide (8–14), brutalist megablock, windows on all interior cols
///   ziggurat   — wide (6–10), strictly pyramidal, no windows, no antenna
///
/// Audio mapping:
///   bass energy       → spawn rate, explosion radius
///   treble energy     → interceptor speed
///   overall level     → missile speed; quiet = city repair; loud = window flicker
///   stereo pan (L/R)  → missiles bias toward the louder channel
///   beat transient    → burst of 2–6 extra missiles
///   sustained loud    → early bomber sortie trigger (>5 s above 40%)
///   sustained silence → lull (no spawns); on return, wave-start burst of 3–7
///
/// ── Table of contents ────────────────────────────────────────────────────────
///
///   ~L63   Constants (CONFIG_VERSION, SPARK_LEN, SPARK_CHARS, gameplay consts)
///   ~L85   ThemeData struct + 10 theme constant blocks (classic … candy)
///   ~L182  theme_data() — name → ThemeData lookup
///   ~L289  CellKind enum — terrain cell render type
///   ~L302  TerrainCell struct — per-cell terrain data
///   ~L312  win_lit()    — deterministic window on/off
///   ~L323  star_at()    — deterministic star field
///   ~L336  Data types   — Missile, Interceptor, Explosion, Smoke, Bomber, Shockwave, Crater
///   ~L404  MissilesViz struct + fields (grouped: entities, audio, city/terrain, stats, config, runtime)
///   ~L474  TickCtx struct — per-frame context passed to tick sub-methods
///   ~L486  impl MissilesViz (public + city helpers)
///   ~L487    new()
///   ~L551    regen_city() — LCG city layout + 9 building types stamped into terrain grid
///   ~L756    blast_city() — radius-based cell damage on terrain grid
///   ~L781    check_structural_collapse() — collapse cells above blown base
///   ~L797    random_dx()  — tiered diagonal distribution
///   ~L822    city_health() — fraction of intact terrain cells
///   ~L836    stats_line() — sparkline, city bar, mercy indicator, counters, fps
///   ~L925  Visualizer impl
///   ~L926    name() / description()
///   ~L929    get_default_config()
///   ~L961    set_config()
///   ~L1004   on_resize()
///   ~L1011   tick()     — coordinator; calls tick sub-methods in order
///   ~L1028   render()   — coordinator; calls render sub-methods in order
///   ~L1075 impl MissilesViz (private sub-methods)
///   ~L1080   tick_audio()           — FFT → bass/overall/treble, rms, beat, stereo pan
///   ~L1120   tick_lull()            — silence timer, lull flag, sustained-loud timer
///   ~L1149   tick_spawn()           — normal spawn + lull-just-ended wave burst
///   ~L1233   tick_mirv()            — MIRV splitting + child interceptors
///   ~L1293   tick_bomber()          — bomber spawn, movement, drop
///   ~L1373   tick_interceptors()    — steering, turn-rate limit, mid-blast kill, trails
///   ~L1455   tick_hits()            — direct hit + splash kill detection
///   ~L1504   tick_missiles()        — advance missiles, ground impact, explosions/scorch
///   ~L1565   tick_effects()         — explosions grow/fade/smoke, shockwaves, scorch, craters
///   ~L1628   tick_city()            — terrain repair, recovery flash, window flicker
///   ~L1691   surface_row()          — screen-space surface height for a terrain column
///   ~L1707   render_stars()
///   ~L1736   render_explosions()
///   ~L1784   render_shockwaves()
///   ~L1809   render_smoke()
///   ~L1824   render_entry_streaks()
///   ~L1842   render_missiles()
///   ~L1870   render_interceptor_trails()
///   ~L1882   render_interceptors()
///   ~L1913   render_bombers()
///   ~L1929   render_city()          — draw terrain grid with cell-kind dispatch
///   ~L2044 register()

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

// ── Gameplay / physics constants ──────────────────────────────────────────────
const INTERCEPT_HIT_RADIUS:        f32 = 1.8;   // direct-hit threshold in tick_hits
const SPLASH_RADIUS:               f32 = 3.5;   // splash-kill radius in tick_hits
const INTERCEPTOR_BLAST_THRESHOLD: f32 = 0.85;  // fraction of explosion radius that kills an interceptor
const INTERCEPTOR_BLAST_MIN_FRAC:  f32 = 0.25;  // explosion must be >25% grown to kill interceptors
const MERCY_HEALTH_THRESHOLD:      f32 = 0.20;  // below this health mercy kicks in
const MERCY_MIN_FACTOR:            f32 = 0.20;  // minimum spawn/missile factor under mercy
const MIRV_ALTITUDE_FRAC:          f32 = 0.38;  // fraction of vis rows where MIRV splits
const MIRV_CHILD_INTERCEPT_RATE:   f32 = 0.35;  // multiplier on intercept_rate for MIRV children
const LULL_THRESHOLD:              f32 = 2.0;   // seconds of silence before lull triggers
const LOUD_THRESHOLD:              f32 = 0.40;  // overall level for "sustained loud"
const LOUD_TIMER_LIMIT:            f32 = 5.0;   // seconds of loud before bomber trigger
const TURN_RATE_MAX:               f32 = std::f32::consts::PI * 1.5; // max interceptor turn rate per second — 270°/s
const EXPLOSION_GROW_RATE:         f32 = 5.5;   // multiplier for explosion radius growth per tick
const SCORCH_FADE_RATE:            f32 = 0.15;  // per-second scorch fade factor

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

/// Determines what character and color a terrain cell renders as.
#[derive(Clone, Copy, Default, PartialEq)]
enum CellKind {
    #[default]
    Empty,    // air / void — not rendered
    Solid,    // intact wall — '█'
    Top,      // building crown — '▀'
    Window,   // window — '▓' lit / '░' dark
    Antenna,  // spire segment — '│', tip '╻'
    Cracked,  // damaged but standing — '▒'
    Blown,    // structural void — '·' (faint, visible hole)
    Rubble,   // collapsed debris at ground — '▄'
}

#[derive(Clone, Copy, Default)]
struct TerrainCell {
    kind:  CellKind,
    color: u8,
    lit:   bool,   // only used for Window cells
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
    mirv_split:  bool,
    heavy:       bool,   // bomber-dropped: faster fall, larger explosion
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

/// Horizontal aircraft that crosses the sky and drops missiles.
struct Bomber {
    x:         f32,
    y:         f32,
    vx:        f32,   // negative = left-to-right entry from right side; positive = right side entry
    drop_cool: f32,
    dead:      bool,
}

/// Fast-expanding pressure shockwave ring from a ground-level explosion.
struct Shockwave {
    cx:         f32,
    cy:         f32,
    radius:     f32,
    max_radius: f32,
    life:       f32,
}

/// Persistent crater that suppresses building regrowth, collapsing inward from edges over time.
struct Crater {
    cx:     f32,   // center column (float)
    radius: f32,   // current suppression radius in columns; shrinks over time
}

// ── Main struct ───────────────────────────────────────────────────────────────

pub struct MissilesViz {
    // ── Entity collections ────────────────────────────────────────────────────
    missiles:     Vec<Missile>,
    interceptors: Vec<Interceptor>,
    explosions:   Vec<Explosion>,
    smoke:        Vec<Smoke>,
    bombers:      Vec<Bomber>,
    shockwaves:   Vec<Shockwave>,
    scorch:       Vec<f32>,
    craters:      Vec<Crater>,

    // ── Audio processing ──────────────────────────────────────────────────────
    bars:          SpectrumBars,
    beat:          BeatDetector,
    spawn_cool:    f32,
    audio_history: VecDeque<f32>,

    // ── City / terrain state ──────────────────────────────────────────────────
    city_cols:      usize,
    win_phase:      f32,

    // ── Terrain grid ──────────────────────────────────────────────────────────
    terrain:        Vec<Vec<TerrainCell>>,  // [col][row_from_ground]; row 0 = ground level
    terrain_origin: Vec<Vec<CellKind>>,     // original cell kinds for repair target
    terrain_repair: Vec<Vec<f32>>,          // per-cell repair progress accumulator

    // ── Statistics ────────────────────────────────────────────────────────────
    missiles_intercepted: u32,
    missiles_hit:         u32,

    // ── Config ────────────────────────────────────────────────────────────────
    gain:             f32,
    speed:            f32,
    intercept_rate:   f32,
    intercept_speed:  f32,
    show_stats:       bool,
    stars_enabled:    bool,
    smoke_enabled:    bool,
    trail_length:     usize,
    explosion_scale:  f32,
    max_missiles:     usize,
    diagonal:         String,
    city_density:     String,
    city_density_cur: String,  // last value used for regen (triggers regen on change)
    theme:            String,
    star_layers:      u8,
    rubble_enabled:   bool,
    mirv_enabled:     bool,
    mirv_chance:      f32,
    bomber_enabled:   bool,
    shockwave_enabled: bool,
    scorch_enabled:   bool,
    regrow_speed:     f32,
    speed_variance:   f32,
    crater_enabled:   bool,

    // ── Runtime state ─────────────────────────────────────────────────────────
    intercept_trails:     Vec<(f32, f32, f32)>,  // (x, y, life 0→1)
    sustained_loud_timer: f32,                    // seconds above LOUD_THRESHOLD
    silence_timer:        f32,                    // seconds below 0.03 overall
    in_lull:              bool,                   // currently in quiet lull
    recovery_flash:       f32,                    // 1→0, city recovery flourish
    city_health_last:     f32,                    // previous frame city health
    entry_streaks:        Vec<(f32, f32)>,        // (x, life) missile entry streaks
    bomber_cool:          f32,
    source:  String,
    next_id: u64,
}

/// Per-frame context derived from audio + size; passed to tick sub-methods.
struct TickCtx {
    cols:       usize,
    vis:        usize,
    ground:     usize,
    bass:       f32,
    overall:    f32,
    treble:     f32,
    stereo_pan: f32,
    is_beat:    bool,
    dt:         f32,
}

impl MissilesViz {
    pub fn new(source: &str) -> Self {
        Self {
            missiles:     Vec::new(),
            interceptors: Vec::new(),
            explosions:   Vec::new(),
            smoke:        Vec::new(),
            bombers:      Vec::new(),
            shockwaves:   Vec::new(),
            scorch:       Vec::new(),
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
            star_layers:       2,
            rubble_enabled:    true,
            mirv_enabled:      true,
            mirv_chance:       0.25,
            bomber_enabled:    true,
            shockwave_enabled: true,
            scorch_enabled:    true,
            regrow_speed:      1.0,
            speed_variance:    0.0,
            intercept_trails:     Vec::new(),
            sustained_loud_timer: 0.0,
            silence_timer:        0.0,
            in_lull:              false,
            recovery_flash:       0.0,
            city_health_last:     0.0,
            entry_streaks:        Vec::new(),
            bomber_cool:    0.0,
            craters:       Vec::new(),
            crater_enabled: true,
            terrain:        Vec::new(),
            terrain_origin: Vec::new(),
            terrain_repair: Vec::new(),
        }
    }

    fn regen_city(&mut self, cols: usize) {
        let density = self.city_density.as_str();
        // Vary LCG seed so different densities produce different layouts
        let dseed: u64 = match density { "sparse" => 0xAABB, "dense" => 0xCCDD, _ => 0x1122 };
        let mut lcg: u64 = (0x5851_f42d_4c95_7f2d ^ (cols as u64).wrapping_mul(6364136223846793005))
                           .wrapping_add(dseed);
        let next = |s: &mut u64| -> f32 {
            *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((*s >> 33) as f32) / (u32::MAX as f32)
        };

        // Resolve shade palette from theme so stored colors are actual ANSI codes
        let td       = theme_data(&self.theme);
        let n_shades = td.city_shades.len().max(1);

        // Density-dependent parameters
        let gap_fill   = match density { "sparse" => 0.35f32, "dense" => 0.72, _ => 0.55 };
        let max_gap    = match density { "sparse" => 4usize,  "dense" => 1,    _ => 2    };
        let h_tower    = match density { "sparse" => (5.0f32, 3.0), "dense" => (9.0, 5.0), _ => (7.0, 5.0) };
        let h_office   = match density { "sparse" => (2.0f32, 2.0), "dense" => (3.0, 4.0), _ => (3.0, 3.0) };
        let h_block    = match density { "sparse" => (1.0f32, 1.0), "dense" => (2.0, 2.0), _ => (2.0, 1.5) };

        // ── Initialize terrain grid ────────────────────────────────────────────
        self.terrain = vec![vec![]; cols];

        let mut c = 0usize;

        // Helper: stamp body cells for a column of height h with optional windows/antenna
        // Returns the vec of TerrainCells for that column.
        let stamp_col = |h: usize, has_windows: bool, shade_color: u8,
                         ant_h: usize, antenna_color: u8, seed: u32, phase: f32,
                         rel_col: u8| -> Vec<TerrainCell> {
            let mut cells = Vec::with_capacity(h + ant_h);
            for row in 0..h {
                let kind = if row == h - 1 {
                    CellKind::Top
                } else if has_windows && row > 0 && win_lit(rel_col, row, seed, phase) {
                    CellKind::Window
                } else {
                    CellKind::Solid
                };
                cells.push(TerrainCell { kind, color: shade_color, lit: false });
            }
            for _ in 0..ant_h {
                cells.push(TerrainCell { kind: CellKind::Antenna, color: antenna_color, lit: false });
            }
            cells
        };

        while c < cols {
            let gap = 1 + (next(&mut lcg) * max_gap as f32) as usize;
            c += gap;
            if c >= cols { break; }

            if next(&mut lcg) > gap_fill { c += 1; continue; }

            let btype       = next(&mut lcg);
            let seed        = (lcg >> 32) as u32;
            let shade_idx   = (next(&mut lcg) * n_shades as f32) as usize % n_shades;
            let shade       = td.city_shades[shade_idx];

            // ── Building type dispatch ────────────────────────────────────────────
            // 9 types: obelisk, skyscraper, tower, office, cathedral, factory, block, slab, ziggurat
            enum BldType { Obelisk, Skyscraper, Tower, Office, Cathedral, Factory, Block, Slab, Ziggurat }
            let bld_type = if btype < 0.06 {
                BldType::Obelisk
            } else if btype < 0.16 {
                BldType::Skyscraper
            } else if btype < 0.27 {
                BldType::Tower
            } else if btype < 0.44 {
                BldType::Office
            } else if btype < 0.58 {
                BldType::Cathedral
            } else if btype < 0.71 {
                BldType::Factory
            } else if btype < 0.82 {
                BldType::Block
            } else if btype < 0.92 {
                BldType::Slab
            } else {
                BldType::Ziggurat
            };

            // Antenna color placeholder (will resolve from theme at render time,
            // but we store a known color index; 0 = use shade color)
            // We use a fixed representative value; render_city will use td.antenna_color.
            // For terrain we store 0 to mean "antenna native" and render handles it.
            let ant_native = 46u8; // placeholder; overridden in render by td.antenna_color

            match bld_type {
                BldType::Obelisk => {
                    // 1 col wide, very tall, large antenna, no windows
                    let h   = (20.0 + next(&mut lcg) * 8.0) as usize;
                    let ant = 5 + (next(&mut lcg) * 3.0) as usize;
                    if c >= cols { c += 1; continue; }
                    self.terrain[c] = stamp_col(h, false, shade, ant, ant_native, seed, 0.0, 0);
                    c += 1;
                }
                BldType::Skyscraper => {
                    let w   = 1 + (next(&mut lcg) * 1.5) as usize;
                    let h   = (14.0 + next(&mut lcg) * 8.0) as usize;
                    let ant = 2 + (next(&mut lcg) * 2.0) as usize;
                    let w   = w.min(cols - c);
                    if w == 0 { c += 1; continue; }
                    for i in 0..w {
                        let is_edge  = i == 0 || i + 1 == w;
                        let this_ant = if i == w / 2 { ant } else { 0 };
                        let has_win  = w >= 2 && !is_edge;
                        self.terrain[c + i] = stamp_col(h, has_win, shade, this_ant, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
                BldType::Tower => {
                    let w   = 1 + (next(&mut lcg) * 1.5) as usize;
                    let h   = (h_tower.0 + next(&mut lcg) * h_tower.1) as usize;
                    let ant = 1 + (next(&mut lcg) * 2.0) as usize;
                    let w   = w.min(cols - c);
                    if w == 0 { c += 1; continue; }
                    for i in 0..w {
                        let this_ant = if i == w / 2 { ant } else { 0 };
                        self.terrain[c + i] = stamp_col(h, false, shade, this_ant, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
                BldType::Office => {
                    // Stepped crown: interior cols taller
                    let w = (3 + (next(&mut lcg) * 2.5) as usize).min(cols - c);
                    if w == 0 { c += 1; continue; }
                    let h = (h_office.0 + next(&mut lcg) * h_office.1) as usize;
                    for i in 0..w {
                        let center = (w as isize - 1) / 2;
                        let dist   = (i as isize - center).unsigned_abs();
                        let steps  = (w / 2).saturating_sub(dist);
                        let col_h  = h + steps;
                        let is_edge = i == 0 || i + 1 == w;
                        let has_win = !is_edge && w > 2;
                        self.terrain[c + i] = stamp_col(col_h, has_win, shade, 0, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
                BldType::Cathedral => {
                    // Peaked center: center col tallest, each outward col 1 row shorter
                    let w      = (5 + (next(&mut lcg) * 3.0) as usize).min(cols - c);
                    if w == 0 { c += 1; continue; }
                    let peak_h = (8.0 + next(&mut lcg) * 6.0) as usize;
                    for i in 0..w {
                        let dist_from_center = (i as isize - (w / 2) as isize).unsigned_abs();
                        let col_h = peak_h.saturating_sub(dist_from_center).max(2);
                        let is_edge = i == 0 || i + 1 == w;
                        let has_win = !is_edge;
                        self.terrain[c + i] = stamp_col(col_h, has_win, shade, 0, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
                BldType::Factory => {
                    let w   = (4 + (next(&mut lcg) * 3.0) as usize).min(cols - c);
                    if w == 0 { c += 1; continue; }
                    let h   = (3.0 + next(&mut lcg) * 2.0) as usize;
                    let ant = 3 + (next(&mut lcg) * 2.0) as usize;
                    for i in 0..w {
                        // chimney antenna on last column only, no windows
                        let this_ant = if i + 1 == w { ant } else { 0 };
                        self.terrain[c + i] = stamp_col(h, false, shade, this_ant, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
                BldType::Block => {
                    let w = (4 + (next(&mut lcg) * 4.0) as usize).min(cols - c);
                    if w == 0 { c += 1; continue; }
                    let h = (h_block.0 + next(&mut lcg) * h_block.1) as usize;
                    for i in 0..w {
                        self.terrain[c + i] = stamp_col(h, false, shade, 0, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
                BldType::Slab => {
                    // Brutalist megablock: very wide, interior cols have windows
                    let w = (8 + (next(&mut lcg) * 6.0) as usize).min(cols - c);
                    if w == 0 { c += 1; continue; }
                    let h = (4.0 + next(&mut lcg) * 4.0) as usize;
                    for i in 0..w {
                        let is_edge = i == 0 || i + 1 == w;
                        self.terrain[c + i] = stamp_col(h, !is_edge, shade, 0, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
                BldType::Ziggurat => {
                    // Strictly pyramidal: center tallest, each col outward 1 row shorter
                    let w      = (6 + (next(&mut lcg) * 4.0) as usize).min(cols - c);
                    if w == 0 { c += 1; continue; }
                    let center_h = (8.0 + next(&mut lcg) * 6.0) as usize;
                    for i in 0..w {
                        let dist_from_center = (i as isize - (w / 2) as isize).unsigned_abs();
                        let col_h = center_h.saturating_sub(dist_from_center).max(1);
                        self.terrain[c + i] = stamp_col(col_h, false, shade, 0, ant_native, seed, 0.0, i as u8);
                    }
                    c += w;
                }
            }
        }

        // Snapshot origin and init repair
        self.terrain_origin = self.terrain.iter().map(|col| col.iter().map(|cell| cell.kind).collect()).collect();
        self.terrain_repair = self.terrain.iter().map(|col| vec![0.0f32; col.len()]).collect();

        self.city_cols        = cols;
        self.city_density_cur = self.city_density.clone();
    }

    fn blast_city(&mut self, cx: f32, cy: f32, max_r: f32, ground: usize) {
        let cy_g = ground as f32 - cy;   // convert screen-row to row_from_ground
        let cols = self.terrain.len();
        for col in 0..cols {
            for row in 0..self.terrain[col].len() {
                let dc = (col as f32 - cx) * 0.5;
                let dr = row as f32 - cy_g;
                let dist = (dc * dc + dr * dr).sqrt();
                if dist >= max_r { continue; }
                let cell = &mut self.terrain[col][row];
                match cell.kind {
                    CellKind::Empty | CellKind::Blown | CellKind::Rubble => {}
                    _ => {
                        if dist < max_r * 0.35 {
                            cell.kind = CellKind::Blown;
                        } else if dist < max_r * 0.65 {
                            cell.kind = CellKind::Cracked;
                        }
                    }
                }
            }
        }
        self.check_structural_collapse();
    }

    fn check_structural_collapse(&mut self) {
        for col in 0..self.terrain.len() {
            // Scan bottom-up: the first Blown cell breaks the load path from the
            // ground, so every cell above it is unsupported and collapses to rubble.
            let mut support_broken = false;
            for row in 0..self.terrain[col].len() {
                if support_broken {
                    let k = &mut self.terrain[col][row].kind;
                    if !matches!(*k, CellKind::Empty | CellKind::Blown | CellKind::Rubble) {
                        *k = CellKind::Rubble;
                    }
                } else if self.terrain[col][row].kind == CellKind::Blown {
                    support_broken = true;
                }
            }
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
        let (alive, total) = self.terrain.iter().flatten()
            .filter(|c| c.kind != CellKind::Empty)
            .fold((0usize, 0usize), |(a, t), cell| {
                let intact = !matches!(cell.kind, CellKind::Blown | CellKind::Rubble);
                (a + intact as usize, t + 1)
            });
        if total == 0 { 1.0 } else { alive as f32 / total as f32 }
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

        // ── Mercy indicator ───────────────────────────────────────────────────
        let mercy_factor = (health / MERCY_HEALTH_THRESHOLD).clamp(0.0, 1.0);
        let (mercy_str, mercy_vis) = if mercy_factor < 1.0 {
            let phase = (self.win_phase * 3.0).sin();
            let col   = if phase > 0.0 { 214u8 } else { 208 };
            (format!("  \x1b[38;5;{col}m⚡MERCY\x1b[0m"), 8usize)
        } else {
            (String::new(), 0)
        };

        // ── Name + fps ────────────────────────────────────────────────────────
        let name_fps     = format!("  {} · {}fps  ", self.name(), fps as u32);
        let name_fps_vis = name_fps.len();

        // ── Hints ─────────────────────────────────────────────────────────────
        let hints     = "  [Esc] visualizers  [F1] settings  [q] quit  ";
        let hints_vis = hints.len();

        // ── Assemble ──────────────────────────────────────────────────────────
        let content_vis = spark_vis + city_vis + counter_vis + mercy_vis + name_fps_vis;
        let total_vis   = content_vis + hints_vis;
        let padding     = if cols > total_vis { " ".repeat(cols - total_vis) } else { String::new() };

        format!(
            "\x1b[2m\x1b[38;5;240m{spark_str}{city_str}{counter_raw}{mercy_str}{name_fps}{padding}{hints}\x1b[0m"
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
                { "name": "gain",             "display_name": "Gain",                    "type": "float", "value": 1.0,   "min": 0.0,  "max": 4.0 },
                { "name": "speed",            "display_name": "Speed",                   "type": "float", "value": 1.0,   "min": 0.2,  "max": 3.0 },
                { "name": "intercept_rate",   "display_name": "[Defense] Intercept %",   "type": "float", "value": 0.55,  "min": 0.0,  "max": 1.0 },
                { "name": "intercept_speed",  "display_name": "[Defense] Speed",         "type": "float", "value": 1.0,   "min": 0.3,  "max": 3.0 },
                { "name": "max_missiles",     "display_name": "[Missiles] Max",          "type": "int",   "value": 60,    "min": 5,    "max": 80  },
                { "name": "trail_length",     "display_name": "[Missiles] Trail Length", "type": "int",   "value": 12,    "min": 2,    "max": 20  },
                { "name": "diagonal",         "display_name": "[Missiles] Diagonal",     "type": "enum",  "value": "mixed", "variants": ["straight","mixed","wild"] },
                { "name": "mirv_enabled",     "display_name": "[Missiles] MIRV",         "type": "bool",  "value": true  },
                { "name": "mirv_chance",      "display_name": "[Missiles] MIRV Chance",  "type": "float", "value": 0.25,  "min": 0.0,  "max": 1.0 },
                { "name": "bomber_enabled",   "display_name": "[Missiles] Bomber",       "type": "bool",  "value": true  },
                { "name": "explosion_scale",  "display_name": "[FX] Explosion Scale",    "type": "float", "value": 1.0,   "min": 0.2,  "max": 3.0 },
                { "name": "shockwave_enabled","display_name": "[FX] Shockwaves",         "type": "bool",  "value": true  },
                { "name": "scorch_enabled",   "display_name": "[FX] Scorch Marks",       "type": "bool",  "value": true  },
                { "name": "smoke_enabled",    "display_name": "[FX] Smoke",              "type": "bool",  "value": true  },
                { "name": "stars_enabled",    "display_name": "[Sky] Stars",             "type": "bool",  "value": true  },
                { "name": "star_layers",      "display_name": "[Sky] Star Layers",       "type": "int",   "value": 2,     "min": 1,    "max": 3   },
                { "name": "city_density",     "display_name": "[City] Density",          "type": "enum",  "value": "normal", "variants": ["sparse","normal","dense"] },
                { "name": "rubble_enabled",   "display_name": "[City] Rubble",           "type": "bool",  "value": true  },
                { "name": "crater_enabled",   "display_name": "[City] Craters",          "type": "bool",  "value": true  },
                { "name": "regrow_speed",     "display_name": "[City] Regrow Speed",     "type": "float", "value": 1.0,   "min": 0.0,  "max": 5.0 },
                { "name": "speed_variance",   "display_name": "[Missiles] Speed Variance","type": "float", "value": 0.0,  "min": 0.0,  "max": 1.0 },
                { "name": "show_stats",       "display_name": "Show Stats",              "type": "bool",  "value": true  },
                { "name": "theme",            "display_name": "Theme",                   "type": "enum",  "value": "classic", "variants": ["classic","neon","cold","retro","plasma","sunset","toxic","cyber","void","candy"] }
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
                    Some("mirv_enabled")      => self.mirv_enabled      = entry["value"].as_bool().unwrap_or(true),
                    Some("mirv_chance")       => self.mirv_chance        = entry["value"].as_f64().unwrap_or(0.25) as f32,
                    Some("bomber_enabled")    => self.bomber_enabled     = entry["value"].as_bool().unwrap_or(true),
                    Some("shockwave_enabled") => self.shockwave_enabled  = entry["value"].as_bool().unwrap_or(true),
                    Some("scorch_enabled")    => self.scorch_enabled     = entry["value"].as_bool().unwrap_or(true),
                    Some("star_layers")       => self.star_layers        = entry["value"].as_i64().unwrap_or(2) as u8,
                    Some("rubble_enabled")    => self.rubble_enabled     = entry["value"].as_bool().unwrap_or(true),
                    Some("crater_enabled")    => self.crater_enabled     = entry["value"].as_bool().unwrap_or(true),
                    Some("regrow_speed")      => self.regrow_speed       = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    Some("speed_variance")    => self.speed_variance     = entry["value"].as_f64().unwrap_or(0.0) as f32,
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
        if self.city_cols != size.cols as usize || self.city_density != self.city_density_cur {
            self.regen_city(size.cols as usize);
        }
        self.bars.resize(size.cols as usize);
        let ctx = self.tick_audio(audio, dt, size);
        let lull_just_ended = self.tick_lull(&ctx);
        self.tick_spawn(&ctx, lull_just_ended);
        self.tick_mirv(&ctx);
        self.tick_bomber(&ctx);
        self.tick_interceptors(&ctx);
        self.tick_hits(&ctx);
        self.tick_missiles(&ctx);
        self.tick_effects(&ctx);
        self.tick_city(&ctx);
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows   = size.rows as usize;
        let cols   = size.cols as usize;
        let vis    = rows.saturating_sub(1);
        let ground = vis.saturating_sub(1);
        let td     = theme_data(&self.theme);

        let mut grid: Vec<Vec<(char, u8, bool)>> = vec![vec![(' ', 0, false); cols]; vis];

        self.render_stars(&mut grid, cols, vis);
        self.render_explosions(&mut grid, cols, vis, &td);
        self.render_shockwaves(&mut grid, cols, vis);
        self.render_smoke(&mut grid, cols, vis);
        self.render_entry_streaks(&mut grid, cols, vis);
        self.render_missiles(&mut grid, cols, vis, &td);
        self.render_interceptor_trails(&mut grid, cols, vis);
        self.render_interceptors(&mut grid, cols, vis, &td);
        self.render_bombers(&mut grid, cols, vis);
        self.render_city(&mut grid, cols, vis, ground, &td);

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

// ── MissilesViz private sub-methods ───────────────────────────────────────────

impl MissilesViz {
    // ── tick sub-methods ──────────────────────────────────────────────────────

    // ── tick_audio ────────────────────────────────────────────────────────────
    /// Computes bass/overall/treble, updates audio state, returns TickCtx.
    fn tick_audio(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) -> TickCtx {
        let rows   = size.rows as usize;
        let cols   = size.cols as usize;
        let vis    = rows.saturating_sub(1);
        let ground = vis.saturating_sub(1);

        let scaled: Vec<f32> = audio.fft.iter().map(|v| v * self.gain).collect();
        self.bars.update(&scaled, dt);

        let n       = self.bars.smoothed.len().max(1);
        let bass    = self.bars.smoothed[..n / 6].iter().copied().sum::<f32>()
                      / (n / 6).max(1) as f32;
        let overall = self.bars.smoothed.iter().copied().sum::<f32>() / n as f32;
        let treble  = self.bars.smoothed[n * 2 / 3..].iter().copied().sum::<f32>()
                      / (n / 3).max(1) as f32;

        // Audio history for sparkline
        self.audio_history.push_back(overall);
        while self.audio_history.len() > SPARK_LEN { self.audio_history.pop_front(); }

        self.win_phase    += (0.3 + overall * 2.5) * dt;

        // Resize scorch if needed
        if self.scorch.len() != cols { self.scorch.resize(cols, 0.0); }

        self.beat.update(&audio.fft, dt);
        let is_beat = self.beat.is_beat();

        // ── Stereo pan ────────────────────────────────────────────────────────
        let l_rms = { let s = &audio.left;  if s.is_empty() { 0.0f32 } else { s.iter().map(|v| v*v).sum::<f32>() / s.len() as f32 } };
        let r_rms = { let s = &audio.right; if s.is_empty() { 0.0f32 } else { s.iter().map(|v| v*v).sum::<f32>() / s.len() as f32 } };
        let total_rms = l_rms + r_rms;
        // pan: 0.0 = fully right, 0.5 = center, 1.0 = fully left
        let stereo_pan = if total_rms > 0.0001 { l_rms / total_rms } else { 0.5 };

        TickCtx { cols, vis, ground, bass, overall, treble, stereo_pan, is_beat, dt }
    }

    // ── tick_lull ─────────────────────────────────────────────────────────────
    /// Updates silence/lull timers and sustained-loud timer. Returns `lull_just_ended`.
    fn tick_lull(&mut self, ctx: &TickCtx) -> bool {
        let TickCtx { overall, dt, .. } = *ctx;

        if overall < 0.03 {
            self.silence_timer += dt;
        } else {
            self.silence_timer = (self.silence_timer - dt * 3.0).max(0.0);
        }
        let was_lull  = self.in_lull;
        self.in_lull  = self.silence_timer > LULL_THRESHOLD;
        let lull_just_ended = was_lull && !self.in_lull;

        // ── Sustained loud timer → early bomber trigger ───────────────────────
        if overall > LOUD_THRESHOLD {
            self.sustained_loud_timer += dt;
            if self.sustained_loud_timer > LOUD_TIMER_LIMIT && self.bomber_enabled
                && self.bombers.is_empty() && self.bomber_cool > 3.0 {
                self.bomber_cool = 0.0;
                self.sustained_loud_timer = 0.0;
            }
        } else {
            self.sustained_loud_timer = (self.sustained_loud_timer - dt * 0.5).max(0.0);
        }

        lull_just_ended
    }

    // ── tick_spawn ────────────────────────────────────────────────────────────
    /// Normal missile spawn logic plus lull-just-ended wave burst.
    fn tick_spawn(&mut self, ctx: &TickCtx, lull_just_ended: bool) {
        let TickCtx { cols, vis, ground, bass, overall, treble, stereo_pan, is_beat, dt } = *ctx;
        let n_palettes = theme_data(&self.theme).missile_palettes.len();

        self.spawn_cool -= dt;
        // Mercy: throttle spawns when city health is critically low so buildings
        // can recover.  Full spawn rate above 20% health; 5× slower near zero.
        let health        = self.city_health();
        let mercy_factor  = (health / MERCY_HEALTH_THRESHOLD).clamp(0.0, 1.0);
        let effective_max = ((self.max_missiles as f32) * mercy_factor.max(MERCY_MIN_FACTOR)) as usize;
        let base_interval = (0.32 - bass * 0.38 - overall * 0.22).clamp(0.04, 0.36);
        let spawn_interval = base_interval / mercy_factor.max(MERCY_MIN_FACTOR);
        if !self.in_lull && (self.spawn_cool <= 0.0 || is_beat) && self.missiles.len() < effective_max {
            let count = if is_beat {
                let max_burst = ((6.0 * mercy_factor) as usize).max(1);
                rand::thread_rng().gen_range(1usize..=max_burst)
            } else { 1 };
            let mut rng = rand::thread_rng();
            for _ in 0..count {
                let stereo_bias = (stereo_pan - 0.5).abs() * 2.0;
                let x = if rng.r#gen::<f32>() < stereo_bias {
                    if stereo_pan > 0.5 { rng.gen_range(0..cols.max(2) / 2) as f32 }
                    else { rng.gen_range(cols.max(2) / 2..cols.max(2)) as f32 }
                } else {
                    rng.gen_range(0..cols.max(1)) as f32
                };
                let dx          = Self::random_dx(&mut rng, &self.diagonal.clone());
                let vy_base     = (vis as f32) * (0.28 + overall * 0.50) * self.speed;
                let vy_scale    = if self.speed_variance > 0.0 {
                    let v = self.speed_variance;
                    rng.gen_range((1.0 - v).max(0.1)..=(1.0 + v))
                } else { 1.0 };
                let vy          = vy_base * vy_scale;
                let palette_idx = rng.gen_range(0..n_palettes);
                let id          = self.next_id;
                self.next_id   += 1;
                self.missiles.push(Missile { id, x, y: 0.0, dx, vy, palette_idx, intercepted: false, mirv_split: false, heavy: false });
                self.entry_streaks.push((x, 0.5));

                if rng.r#gen::<f32>() < self.intercept_rate {
                    let launch_c = (0..cols)
                        .filter(|&c| c < self.terrain.len() && !self.terrain[c].is_empty()
                            && self.terrain[c].iter().any(|cell| !matches!(cell.kind, CellKind::Empty | CellKind::Blown | CellKind::Rubble)))
                        .min_by_key(|&c| (c as isize - x as isize).unsigned_abs())
                        .unwrap_or(x as usize);
                    let launch_y = self.surface_row(launch_c, ground) as f32;
                    let rows_left = vis as f32 / vy.max(0.001);
                    let target_c  = (x + dx * vy * rows_left).clamp(0.0, (cols - 1) as f32);
                    let ddx       = target_c - launch_c as f32;
                    let ddy       = ground as f32 - launch_y;
                    let dist      = (ddx * ddx + ddy * ddy).sqrt().max(0.001);
                    let isp       = vis as f32 * 0.80 * self.speed * self.intercept_speed * (1.0 + treble * 1.5);
                    self.interceptors.push(Interceptor {
                        x: launch_c as f32, y: launch_y,
                        vx: (ddx / dist) * isp, vy: (ddy / dist) * isp,
                        target_id: id, tx: x, ty: 0.0,
                        launch_col: launch_c, dead: false,
                    });
                }
            }
            self.spawn_cool = spawn_interval;
        }

        // Wave start: lull just ended → spawn a burst
        if lull_just_ended {
            let mut rng = rand::thread_rng();
            let count = rng.gen_range(3usize..=7);
            let n_palettes = theme_data(&self.theme).missile_palettes.len();
            for _ in 0..count {
                if self.missiles.len() >= self.max_missiles { break; }
                let x   = rng.gen_range(0..cols.max(1)) as f32;
                let dx  = Self::random_dx(&mut rng, &self.diagonal.clone());
                let vy  = (vis as f32) * (0.30 + overall * 0.40) * self.speed;
                let id  = self.next_id; self.next_id += 1;
                self.missiles.push(Missile { id, x, y: 0.0, dx, vy,
                    palette_idx: rng.gen_range(0..n_palettes),
                    intercepted: false, mirv_split: false, heavy: false });
                self.entry_streaks.push((x, 0.5));
            }
        }
    }

    // ── tick_mirv ─────────────────────────────────────────────────────────────
    /// MIRV splitting and opportunistic child interceptors.
    fn tick_mirv(&mut self, ctx: &TickCtx) {
        if !self.mirv_enabled { return; }
        let TickCtx { cols, vis, ground, treble, dt, .. } = *ctx;
        let n_palettes = theme_data(&self.theme).missile_palettes.len();

        let mirv_y = vis as f32 * MIRV_ALTITUDE_FRAC;
        let mut new_children: Vec<Missile>              = Vec::new();
        let mut child_info:   Vec<(u64, f32, f32, f32)> = Vec::new(); // (id,x,y,vy)
        for m in &mut self.missiles {
            if !m.mirv_split && m.y >= mirv_y && rand::thread_rng().r#gen::<f32>() < self.mirv_chance * dt * 4.0 {
                m.mirv_split = true;
                let mut rng = rand::thread_rng();
                let n_children = rng.gen_range(3usize..=4);
                for i in 0..n_children {
                    let spread = (i as f32 - (n_children - 1) as f32 * 0.5) * 0.25;
                    let child  = Missile {
                        id:          self.next_id,
                        x:           m.x,
                        y:           m.y,
                        dx:          m.dx + spread,
                        vy:          m.vy * 1.15,
                        palette_idx: (m.palette_idx + i + 1) % n_palettes,
                        intercepted: false,
                        mirv_split:  true,
                        heavy:       m.heavy,
                    };
                    child_info.push((child.id, child.x, child.y, child.vy));
                    new_children.push(child);
                    self.next_id += 1;
                }
            }
        }
        self.missiles.extend(new_children);

        // Opportunistic intercept of MIRV children at reduced rate (MIRV_CHILD_INTERCEPT_RATE).
        // Multiple interceptors may claim the same child — that's fine.
        let mut rng = rand::thread_rng();
        for (cid, cx, cy, _cvy) in child_info {
            if rng.r#gen::<f32>() >= self.intercept_rate * MIRV_CHILD_INTERCEPT_RATE { continue; }
            let launch_c = (0..cols)
                .filter(|&c| c < self.terrain.len() && !self.terrain[c].is_empty()
                    && self.terrain[c].iter().any(|cell| !matches!(cell.kind, CellKind::Empty | CellKind::Blown | CellKind::Rubble)))
                .min_by_key(|&c| (c as isize - cx as isize).unsigned_abs())
                .unwrap_or(cx as usize);
            let launch_y = self.surface_row(launch_c, ground) as f32;
            let ddx      = cx - launch_c as f32;
            let ddy      = ground as f32 - launch_y;
            let dist     = (ddx * ddx + ddy * ddy).sqrt().max(0.001);
            let isp_m    = vis as f32 * 0.80 * self.speed * self.intercept_speed * (1.0 + treble * 1.5);
            self.interceptors.push(Interceptor {
                x: launch_c as f32, y: launch_y,
                vx: (ddx / dist) * isp_m, vy: (ddy / dist) * isp_m,
                target_id: cid, tx: cx, ty: cy,
                launch_col: launch_c, dead: false,
            });
        }
    }

    // ── tick_bomber ───────────────────────────────────────────────────────────
    /// Bomber spawn, movement, and missile drop logic.
    fn tick_bomber(&mut self, ctx: &TickCtx) {
        if !self.bomber_enabled { return; }
        let TickCtx { cols, vis, ground, treble, dt, .. } = *ctx;
        let n_palettes = theme_data(&self.theme).missile_palettes.len();

        self.bomber_cool -= dt;
        if self.bomber_cool <= 0.0 && self.bombers.is_empty() {
            let go_right = rand::thread_rng().r#gen::<bool>();
            let bx = if go_right { -(3.0f32) } else { cols as f32 + 3.0 };
            let by = (vis as f32 * 0.08).max(2.0);
            let spd = cols as f32 * 0.04 * self.speed;
            self.bombers.push(Bomber {
                x: bx, y: by,
                vx: if go_right { spd } else { -spd },
                drop_cool: 0.8,
                dead: false,
            });
            self.bomber_cool = 18.0 + rand::thread_rng().gen_range(0.0f32..12.0);
        }
        let diag = self.diagonal.clone();
        // Pre-compute surface rows to avoid borrow conflicts inside the bomber loop
        let bomber_surface_rows: Vec<usize> = (0..cols).map(|c| self.surface_row(c, ground)).collect();
        let mut new_bomber_missiles:     Vec<Missile>     = Vec::new();
        let mut new_bomber_interceptors: Vec<Interceptor> = Vec::new();
        let mut rng = rand::thread_rng();
        for b in &mut self.bombers {
            b.x += b.vx * dt;
            b.drop_cool -= dt;
            if b.drop_cool <= 0.0 && self.missiles.len() + new_bomber_missiles.len() < self.max_missiles {
                let dx = Self::random_dx(&mut rng, &diag);
                let vy = vis as f32 * 0.62 * self.speed;  // ~2.2× standard base speed
                new_bomber_missiles.push(Missile {
                    id:          self.next_id,
                    x:           b.x,
                    y:           b.y + 1.0,
                    dx,
                    vy,
                    palette_idx: rng.gen_range(0..n_palettes),
                    intercepted: false,
                    mirv_split:  false,
                    heavy:       true,
                });
                self.entry_streaks.push((b.x, 0.5));
                self.next_id += 1;
                b.drop_cool = 1.2 + rng.gen_range(0.0f32..0.8);

                // Interceptor for bomber missiles at half intercept rate
                let new_id    = self.next_id - 1;
                let missile_x = b.x;
                let missile_vy = vy;
                if rng.r#gen::<f32>() < self.intercept_rate * 0.5 {
                    let launch_c = (0..cols)
                        .filter(|&c| c < self.terrain.len() && !self.terrain[c].is_empty()
                            && self.terrain[c].iter().any(|cell| !matches!(cell.kind, CellKind::Empty | CellKind::Blown | CellKind::Rubble)))
                        .min_by_key(|&c| (c as isize - missile_x as isize).unsigned_abs())
                        .unwrap_or(missile_x as usize);
                    let launch_y  = bomber_surface_rows.get(launch_c).copied().unwrap_or(ground) as f32;
                    let rows_left = vis as f32 / missile_vy.max(0.001);
                    let target_c  = (missile_x + dx * missile_vy * rows_left).clamp(0.0, (cols - 1) as f32);
                    let ddx       = target_c - launch_c as f32;
                    let ddy       = ground as f32 - launch_y;
                    let dist      = (ddx * ddx + ddy * ddy).sqrt().max(0.001);
                    let isp       = vis as f32 * 0.80 * self.speed * self.intercept_speed * (1.0 + treble * 1.5);
                    new_bomber_interceptors.push(Interceptor {
                        x: launch_c as f32, y: launch_y,
                        vx: (ddx / dist) * isp, vy: (ddy / dist) * isp,
                        target_id: new_id, tx: missile_x, ty: 0.0,
                        launch_col: launch_c, dead: false,
                    });
                }
            }
            if b.x < -5.0 || b.x > cols as f32 + 5.0 { b.dead = true; }
        }
        self.missiles.extend(new_bomber_missiles);
        self.interceptors.extend(new_bomber_interceptors);
        self.bombers.retain(|b| !b.dead);
    }

    // ── tick_interceptors ─────────────────────────────────────────────────────
    /// Interceptor steering (turn-rate limit), movement, mid-blast kill, trail emit/decay.
    fn tick_interceptors(&mut self, ctx: &TickCtx) {
        let TickCtx { cols, vis, treble, dt, .. } = *ctx;

        let missile_snap: Vec<(u64, f32, f32)> =
            self.missiles.iter().map(|m| (m.id, m.x, m.y)).collect();
        let isp = vis as f32 * 0.80 * self.speed * self.intercept_speed * (1.0 + treble * 1.5);

        for int_ in &mut self.interceptors {
            // ── Determine desired velocity ─────────────────────────────────────
            let (desired_vx, desired_vy) =
                if let Some(&(_, mx, my)) = missile_snap.iter().find(|&&(id,_,_)| id == int_.target_id) {
                    // Target alive — keep tracking
                    int_.tx = mx; int_.ty = my;
                    let ddx = mx - int_.x; let ddy = my - int_.y;
                    let d   = (ddx*ddx + ddy*ddy).sqrt().max(0.001);
                    (ddx/d * isp, ddy/d * isp)
                } else if let Some(&(new_id, nx, ny)) = missile_snap.iter().min_by_key(|&&(_,mx,my)| {
                    let dr = int_.y - my; let dc = (int_.x - mx) * 0.5;
                    ((dr*dr + dc*dc).sqrt() * 1000.0) as i64
                }) {
                    // Target gone — retarget nearest surviving missile
                    int_.target_id = new_id;
                    int_.tx = nx; int_.ty = ny;
                    let ddx = nx - int_.x; let ddy = ny - int_.y;
                    let d   = (ddx*ddx + ddy*ddy).sqrt().max(0.001);
                    (ddx/d * isp, ddy/d * isp)
                } else {
                    // No missiles — coast to old impact point
                    (int_.vx, int_.vy)
                };

            // ── Turn-rate limit: max ~270 °/s, prevents 180° snap on retarget ──
            let cur_spd = (int_.vx * int_.vx + int_.vy * int_.vy).sqrt();
            if cur_spd > 0.5 {
                use std::f32::consts::{PI, TAU};
                let cur_ang = int_.vy.atan2(int_.vx);
                let tgt_ang = desired_vy.atan2(desired_vx);
                let mut diff = tgt_ang - cur_ang;
                if diff >  PI { diff -= TAU; }
                if diff < -PI { diff += TAU; }
                let turn = diff.clamp(-TURN_RATE_MAX * dt, TURN_RATE_MAX * dt);
                let ang  = cur_ang + turn;
                int_.vx  = ang.cos() * isp;
                int_.vy  = ang.sin() * isp;
            } else {
                int_.vx = desired_vx;
                int_.vy = desired_vy;
            }

            int_.x += int_.vx * dt;
            int_.y += int_.vy * dt;

            // ── Destroyed by a nearby midair explosion ─────────────────────────
            let in_blast = self.explosions.iter().any(|e| {
                if e.radius < e.max_radius * INTERCEPTOR_BLAST_MIN_FRAC { return false; }
                let dr = e.cy - int_.y;
                let dc = (e.cx - int_.x) * 0.5;
                (dr*dr + dc*dc).sqrt() < e.radius * INTERCEPTOR_BLAST_THRESHOLD
            });

            let alive = missile_snap.iter().any(|&(id,_,_)| id == int_.target_id);
            if in_blast
                || int_.y < -2.0 || int_.x < -2.0 || int_.x > cols as f32 + 2.0
                || (!alive && int_.y >= int_.ty)
            {
                int_.dead = true;
            }
        }

        // Emit trail particles for active interceptors
        for int_ in &self.interceptors {
            if !int_.dead {
                self.intercept_trails.push((int_.x, int_.y, 0.25));
            }
        }
        // Decay trails
        for t in &mut self.intercept_trails { t.2 -= dt * 3.0; }
        self.intercept_trails.retain(|t| t.2 > 0.0);
    }

    // ── tick_hits ─────────────────────────────────────────────────────────────
    /// Detect interceptor-missile hits; launch small explosions.
    fn tick_hits(&mut self, ctx: &TickCtx) {
        let TickCtx { bass, .. } = *ctx;

        let mut int_remove: Vec<usize>      = Vec::new();
        let mut mis_remove: Vec<usize>      = Vec::new();
        let mut small_expl: Vec<(f32, f32)> = Vec::new();

        // Each interceptor that scores a direct hit (< INTERCEPT_HIT_RADIUS cells) also
        // splash-kills any other missiles within SPLASH_RADIUS cells.
        for (ii, int_) in self.interceptors.iter().enumerate() {
            if int_.dead { int_remove.push(ii); continue; }
            let primary_hit = self.missiles.iter().any(|m| {
                let dr = int_.y - m.y;
                let dc = (int_.x - m.x) * 0.5;
                (dr * dr + dc * dc).sqrt() < INTERCEPT_HIT_RADIUS
            });
            if primary_hit {
                int_remove.push(ii);
                small_expl.push((int_.x, int_.y));
                for (mi, m) in self.missiles.iter().enumerate() {
                    let dr = int_.y - m.y;
                    let dc = (int_.x - m.x) * 0.5;
                    if (dr * dr + dc * dc).sqrt() < SPLASH_RADIUS {
                        mis_remove.push(mi);
                    }
                }
            }
        }
        mis_remove.sort_unstable();
        mis_remove.dedup();
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
    }

    // ── tick_missiles ─────────────────────────────────────────────────────────
    /// Advance missile positions; detect ground impacts; spawn explosions/shockwaves/scorch/craters.
    fn tick_missiles(&mut self, ctx: &TickCtx) {
        let TickCtx { cols, vis, ground, bass, overall, dt, .. } = *ctx;

        // Pre-compute surface rows to avoid borrow conflicts inside retain_mut
        let surface_rows: Vec<usize> = (0..cols).map(|c| self.surface_row(c, ground)).collect();

        let mut to_explode: Vec<(f32, f32, f32, bool)> = Vec::new();
        self.missiles.retain_mut(|m| {
            if m.intercepted { return false; }
            m.y += m.vy * dt;
            m.x  = (m.x + m.dx * m.vy * dt).clamp(0.0, (cols - 1) as f32);
            let col = m.x as usize;
            let imp = {
                let c0 = col;
                let c1 = col.saturating_sub(1);
                let c2 = (col + 1).min(cols - 1);
                [c0, c1, c2].iter().map(|&c| surface_rows.get(c).copied().unwrap_or(ground)).min().unwrap_or(ground)
            };
            if m.y as usize >= imp || m.y as usize >= vis {
                let heavy_mult = if m.heavy { 2.2 } else { 1.0 };
                let max_r = ((5.0 + bass * 15.0 + overall * 9.0) * self.explosion_scale * heavy_mult).clamp(2.0, 50.0);
                to_explode.push((m.x, m.y.min(imp as f32), max_r, m.heavy));
                false
            } else {
                true
            }
        });
        self.missiles_hit += to_explode.len() as u32;
        for (cx, cy, max_r, m_heavy) in to_explode {
            self.blast_city(cx, cy, max_r, ground);
            self.explosions.push(Explosion {
                cx, cy, radius: 0.0, max_radius: max_r, life: 1.0,
                smoke_spawned: false,
            });
            if self.shockwave_enabled {
                self.shockwaves.push(Shockwave {
                    cx, cy,
                    radius: 0.0,
                    max_radius: max_r * 2.2,
                    life: 1.0,
                });
            }
            if self.scorch_enabled {
                let reach = (max_r * 1.5) as isize;
                let cxi = cx as isize;
                for dc in -reach..=reach {
                    let sc = (cxi + dc) as usize;
                    if sc < self.scorch.len() {
                        let falloff = 1.0 - (dc.abs() as f32 / reach.max(1) as f32);
                        self.scorch[sc] = (self.scorch[sc] + falloff * 0.9).min(1.0);
                    }
                }
            }
            if self.crater_enabled && m_heavy {
                self.craters.push(Crater { cx, radius: max_r * 1.8 });
            }
        }
    }

    // ── tick_effects ──────────────────────────────────────────────────────────
    /// Update explosions (grow/fade/smoke spawn), smoke drift, shockwave expand, scorch fade, crater shrink.
    fn tick_effects(&mut self, ctx: &TickCtx) {
        let TickCtx { dt, .. } = *ctx;

        // ── Update explosions + spawn smoke ───────────────────────────────────
        let mut new_smoke: Vec<Smoke> = Vec::new();
        let mut rng = rand::thread_rng();
        for e in &mut self.explosions {
            if e.radius < e.max_radius {
                e.radius += e.max_radius * EXPLOSION_GROW_RATE * dt;
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

        // ── Update shockwaves ─────────────────────────────────────────────────
        for sw in &mut self.shockwaves {
            sw.radius += sw.max_radius * 4.0 * dt;
            sw.life   -= dt * 2.2;
        }
        self.shockwaves.retain(|sw| sw.life > 0.0 && sw.radius <= sw.max_radius);

        // ── Fade scorch marks ─────────────────────────────────────────────────
        for s in &mut self.scorch {
            *s *= 1.0 - SCORCH_FADE_RATE * dt;
            if *s < 0.02 { *s = 0.0; }
        }

        // ── Shrink craters (edges clear first, center last) ───────────────────
        for cr in &mut self.craters {
            cr.radius -= (0.4 + cr.radius * 0.05) * dt;
        }
        self.craters.retain(|cr| cr.radius > 0.5);
    }

    // ── tick_city ─────────────────────────────────────────────────────────────
    /// Building regrow, recovery flash, entry streak decay, window flicker.
    fn tick_city(&mut self, ctx: &TickCtx) {
        let TickCtx { overall, dt, .. } = *ctx;

        // ── Regrow terrain cells ───────────────────────────────────────────────
        let quiet_factor = (1.0 - overall * 3.0).clamp(0.0, 1.0);
        let regrow_rate  = 0.25 * quiet_factor * self.regrow_speed;

        if regrow_rate > 0.0 {
            for col in 0..self.terrain.len() {
                // Check crater suppression
                let crater_suppressed = self.crater_enabled && self.craters.iter().any(|cr| {
                    (col as f32 - cr.cx).abs() < cr.radius
                });
                if crater_suppressed { continue; }

                if self.terrain_origin[col].len() != self.terrain[col].len() { continue; }

                for row in 0..self.terrain[col].len() {
                    let orig = self.terrain_origin[col][row];
                    if orig == CellKind::Empty { continue; }
                    let current = self.terrain[col][row].kind;
                    let next_kind = match current {
                        CellKind::Blown   => Some(CellKind::Cracked),
                        CellKind::Cracked => Some(orig),
                        CellKind::Rubble  => Some(CellKind::Empty),
                        _                 => None,
                    };
                    if let Some(target) = next_kind {
                        self.terrain_repair[col][row] += regrow_rate * dt;
                        if self.terrain_repair[col][row] >= 1.0 {
                            self.terrain_repair[col][row] = 0.0;
                            self.terrain[col][row].kind = target;
                        }
                    }
                }
            }
        }

        // ── Recovery flash ────────────────────────────────────────────────────
        let health = self.city_health();
        if self.city_health_last < 0.80 && health >= 0.80 {
            self.recovery_flash = 1.0;
        }
        self.city_health_last = health;
        self.recovery_flash   = (self.recovery_flash - dt * 0.4).max(0.0);

        // ── Entry streak decay ────────────────────────────────────────────────
        for s in &mut self.entry_streaks { s.1 -= dt * 2.5; }
        self.entry_streaks.retain(|s| s.1 > 0.0);

        // ── Window flicker ────────────────────────────────────────────────────
        for col in 0..self.terrain.len() {
            for row in 0..self.terrain[col].len() {
                if self.terrain[col][row].kind == CellKind::Window {
                    self.terrain[col][row].lit = self.recovery_flash > 0.0
                        || win_lit(col as u8, row, (col as u32).wrapping_mul(0x9e3779b9), self.win_phase);
                }
            }
        }
    }

    /// Returns the screen-space row of the first occupied (non-Empty, non-Blown) cell
    /// in the given column, from the top down. Returns `ground` if the column is clear.
    fn surface_row(&self, col: usize, ground: usize) -> usize {
        if col >= self.terrain.len() { return ground; }
        let cells = &self.terrain[col];
        // Find the highest non-empty, non-blown cell (highest row_from_ground index)
        for row_g in (0..cells.len()).rev() {
            match cells[row_g].kind {
                CellKind::Empty | CellKind::Blown => continue,
                _ => return ground.saturating_sub(row_g + 1),
            }
        }
        ground  // column is empty or fully blown
    }

    // ── render sub-methods ────────────────────────────────────────────────────

    // ── render_stars ──────────────────────────────────────────────────────────
    fn render_stars(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize) {
        if !self.stars_enabled || self.star_layers == 0 { return; }
        let sky_limit = vis.saturating_sub(vis / 4);
        for r in 0..sky_limit {
            for c in 0..cols {
                if grid[r][c].0 != ' ' { continue; }
                let h = (c as u64).wrapping_mul(2654435761)
                         .wrapping_add((r as u64).wrapping_mul(2246822519))
                         .wrapping_mul(6364136223846793005);
                let pct = h % 200;
                let cell = match self.star_layers {
                    1 => if pct < 1  { Some(('✦', 244u8)) } else { None },
                    2 => if      pct < 1  { Some(('✦', 240)) }
                         else if pct < 5  { Some(('·', 236)) }
                         else             { None },
                    _ => if      pct < 1  { Some(('✦', 240)) }
                         else if pct < 5  { Some(('·', 236)) }
                         else if pct < 8  { Some(('·', 237)) }
                         else if pct < 12 { Some(('·', 234)) }
                         else             { None },
                };
                if let Some((ch, color)) = cell {
                    grid[r][c] = (ch, color, false);
                }
            }
        }
    }

    // ── render_explosions ─────────────────────────────────────────────────────
    fn render_explosions(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize, td: &ThemeData) {
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
    }

    // ── render_shockwaves ─────────────────────────────────────────────────────
    fn render_shockwaves(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize) {
        for sw in &self.shockwaves {
            let outer = sw.radius + 0.6;
            let inner = (sw.radius - 0.6).max(0.0);
            let row_min = (sw.cy - outer - 1.0).max(0.0) as usize;
            let row_max = ((sw.cy + outer + 1.0) as usize + 1).min(vis);
            let col_min = (sw.cx - (outer + 1.0) * 2.0).max(0.0) as usize;
            let col_max = ((sw.cx + (outer + 1.0) * 2.0) as usize + 1).min(cols);
            for r in row_min..row_max {
                for c in col_min..col_max {
                    let dr   = r as f32 - sw.cy;
                    let dc   = (c as f32 - sw.cx) * 0.5;
                    let dist = (dr * dr + dc * dc).sqrt();
                    if dist < inner || dist > outer { continue; }
                    if grid[r][c].0 != ' ' && grid[r][c].1 > 50 { continue; }
                    let alpha = (1.0 - ((dist - sw.radius).abs() / 0.7).min(1.0)) * sw.life;
                    if alpha < 0.2 { continue; }
                    let color = if sw.life > 0.6 { 231u8 } else if sw.life > 0.35 { 250 } else { 244 };
                    grid[r][c] = ('·', color, false);
                }
            }
        }
    }

    // ── render_smoke ──────────────────────────────────────────────────────────
    fn render_smoke(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize) {
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
    }

    // ── render_entry_streaks ──────────────────────────────────────────────────
    fn render_entry_streaks(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize) {
        for &(sx, life) in &self.entry_streaks {
            let sc = sx as usize;
            if sc >= cols { continue; }
            let streak_len = ((life / 0.5) * 10.0) as usize;
            for dl in 0..streak_len.min(cols) {
                let cc = sc.saturating_sub(dl);
                for dr in 0..2usize {
                    if dr >= vis { continue; }
                    if grid[dr][cc].0 != ' ' { continue; }
                    let color = if dl == 0 { 231u8 } else if dl < 3 { 250 } else { 244 };
                    grid[dr][cc] = ('-', color, dl == 0);
                }
            }
        }
    }

    // ── render_missiles ───────────────────────────────────────────────────────
    fn render_missiles(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize, td: &ThemeData) {
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
    }

    // ── render_interceptor_trails ─────────────────────────────────────────────
    fn render_interceptor_trails(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize) {
        for &(tx, ty, life) in &self.intercept_trails {
            let tr = ty as usize;
            let tc = tx as usize;
            if tr >= vis || tc >= cols { continue; }
            if grid[tr][tc].0 != ' ' { continue; }
            let color = if life > 0.18 { 159u8 } else if life > 0.10 { 123 } else { 87 };
            grid[tr][tc] = ('·', color, false);
        }
    }

    // ── render_interceptors ───────────────────────────────────────────────────
    fn render_interceptors(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize, td: &ThemeData) {
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
    }

    // ── render_bombers ────────────────────────────────────────────────────────
    fn render_bombers(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize) {
        for b in &self.bombers {
            let br = b.y as usize;
            let bc = b.x as usize;
            if br < vis && bc < cols {
                // Bomber body: direction arrow
                let ch = if b.vx > 0.0 { '»' } else { '«' };
                grid[br][bc] = (ch, 231, true);
                // Wing chars
                if bc + 1 < cols { grid[br][bc + 1] = ('─', 250, false); }
                if bc > 0        { grid[br][bc - 1] = ('─', 250, false); }
            }
        }
    }

    // ── render_city ───────────────────────────────────────────────────────────
    fn render_city(&self, grid: &mut Vec<Vec<(char, u8, bool)>>, cols: usize, vis: usize, ground: usize, td: &ThemeData) {
        // Snapshot explosions for building illumination
        let expl_snap: Vec<(f32, f32, f32, f32)> = self.explosions.iter()
            .map(|e| (e.cx, e.cy, e.max_radius, e.life))
            .collect();
        let n_shades = td.city_shades.len().max(1);

        // Snapshot craters to avoid borrow conflicts
        let craters_snap: Vec<(f32, f32)> = self.craters.iter().map(|cr| (cr.cx, cr.radius)).collect();

        // Launch pad markers
        let launch_cols: Vec<usize> = self.interceptors.iter()
            .filter(|i| !i.dead)
            .map(|i| i.launch_col)
            .collect();

        for col in 0..cols.min(self.terrain.len()) {
            // Explosion glow for this column
            let expl_glow = expl_snap.iter().map(|&(ecx, _ecy, er, el)| {
                if er < 1.0 { return 0.0f32; }
                let dc = (col as f32 - ecx) * 0.5;
                let glow_r = er * 2.2;
                if dc.abs() < glow_r { el * (1.0 - dc.abs() / glow_r) } else { 0.0 }
            }).fold(0.0f32, f32::max);

            let cells = &self.terrain[col];
            let col_height = cells.len();

            for row_g in 0..col_height {
                let screen_row = match ground.checked_sub(row_g + 1) {
                    Some(r) if r < vis => r,
                    _ => continue,
                };
                if grid[screen_row][col].0 != ' ' { continue; }

                let cell = cells[row_g];

                // Apply explosion glow to base color
                let color = if expl_glow > 0.55 { 231u8 }
                            else if expl_glow > 0.30 { td.city_shades[1 % n_shades] }
                            else { cell.color };

                let (ch, col_out, bold) = match cell.kind {
                    CellKind::Empty   => continue,
                    CellKind::Solid   => ('█', color, false),
                    CellKind::Top     => ('▀', color, false),
                    CellKind::Window  => if cell.lit {
                        ('▓', td.window_lit,  false)
                    } else {
                        ('░', td.window_dark, false)
                    },
                    CellKind::Antenna => {
                        // Tip is the last antenna cell (highest row_g)
                        let is_tip = row_g + 1 == col_height
                            || cells[row_g + 1].kind != CellKind::Antenna;
                        let ch = if is_tip { '╻' } else { '│' };
                        (ch, td.antenna_color, is_tip)
                    },
                    CellKind::Cracked => ('▒', td.city_shades[n_shades - 1], false),
                    CellKind::Blown   => ('·', 234, false),
                    CellKind::Rubble  => ('▄', td.city_shades[2 % n_shades], false),
                };
                grid[screen_row][col] = (ch, col_out, bold);
            }

            // Ground row fill
            if ground < vis && grid[ground][col].0 == ' ' {
                grid[ground][col] = ('▄', td.ground_color, false);
            }

            // Launch pad marker at building top
            if col_height > 0 && launch_cols.contains(&col) {
                let top_screen = ground.saturating_sub(col_height);
                if top_screen < vis && grid[top_screen][col].0 == ' ' {
                    grid[top_screen][col] = ('╦', td.antenna_color, true);
                }
            }

            // Scorch marks at base of hit columns
            if self.scorch_enabled && col < self.scorch.len() && self.scorch[col] > 0.05 {
                if ground < vis {
                    let sc_intensity = self.scorch[col];
                    let ch    = if sc_intensity > 0.7 { '▓' } else if sc_intensity > 0.4 { '▒' } else { '░' };
                    let color = if sc_intensity > 0.7 { 234u8 } else if sc_intensity > 0.4 { 236 } else { 238 };
                    if grid[ground][col].1 == td.ground_color {
                        grid[ground][col] = (ch, color, false);
                    }
                }
            }
        }

        // Crater — dim depression mark at columns within active craters
        if self.crater_enabled {
            for (cr_cx, cr_radius) in &craters_snap {
                let reach = *cr_radius as isize;
                let cxi   = *cr_cx as isize;
                for dc in -reach..=reach {
                    let cc = (cxi + dc) as usize;
                    if cc >= cols { continue; }
                    let depth = 1.0 - ((dc as f32).abs() / cr_radius.max(0.1)).min(1.0);
                    if depth < 0.15 { continue; }
                    let gr = ground;
                    if gr < vis && (grid[gr][cc].1 == td.ground_color || grid[gr][cc].0 == ' ') {
                        let ch    = if depth > 0.7 { '▂' } else if depth > 0.4 { '▁' } else { '·' };
                        let color = if depth > 0.7 { 233u8 } else if depth > 0.4 { 235 } else { 237 };
                        grid[gr][cc] = (ch, color, false);
                    }
                }
            }
        }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(MissilesViz::new(""))]
}
