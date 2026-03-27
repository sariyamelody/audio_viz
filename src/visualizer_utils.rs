/// visualizer_utils.rs — Shared utility functions and constants for visualizers.
///
/// This module contains common code that was previously duplicated across
/// multiple visualizer implementations: audio DSP helpers, colour palettes,
/// rendering primitives, and smoothing utilities.

use crate::visualizer::{FFT_SIZE, SAMPLE_RATE};

// ── Audio DSP helpers ────────────────────────────────────────────────────────

/// Root-mean-square of a sample slice.
///
/// Uses `fold()` for better autovectorisation compared to `map().sum()`.
#[inline]
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples.iter().fold(0.0f32, |acc, &v| acc + v * v);
    (sum / samples.len() as f32).sqrt()
}

/// Convert a frequency in Hz to an FFT bin index, clamped to valid range.
#[inline]
pub fn freq_to_bin(freq_hz: f32, n_bins: usize) -> usize {
    let freq_res = SAMPLE_RATE as f32 / FFT_SIZE as f32;
    ((freq_hz / freq_res) as usize).clamp(1, n_bins - 1)
}

/// Compute RMS energy of an FFT slice between two frequencies.
#[inline]
pub fn band_energy(fft: &[f32], lo_hz: f32, hi_hz: f32) -> f32 {
    let n = fft.len();
    let lo = freq_to_bin(lo_hz, n);
    let hi = freq_to_bin(hi_hz, n).max(lo + 1);
    rms(&fft[lo..hi.min(n)])
}

/// Convert a linear magnitude to a normalised [0, 1] fraction via dB scale.
#[inline]
pub fn mag_to_frac(v: f32, db_floor: f32, db_ceil: f32) -> f32 {
    let db = 20.0 * v.max(1e-9).log10();
    ((db - db_floor) / (db_ceil - db_floor)).clamp(0.0, 1.0)
}

/// Exponential moving average with asymmetric rise/fall coefficients.
///
/// When `target > current` the `rise` coefficient is used (faster attack);
/// otherwise `fall` is used (slower release).  Returns the new smoothed value.
#[inline]
pub fn smooth_asymmetric(current: f32, target: f32, rise: f32, fall: f32) -> f32 {
    let a = if target > current { rise } else { fall };
    a * current + (1.0 - a) * target
}

/// Apply gain to FFT data, avoiding allocation when gain ≈ 1.0.
///
/// Calls `f` with the (possibly scaled) FFT slice.
#[inline]
pub fn with_gained_fft<F>(fft: &[f32], gain: f32, mut f: F)
where
    F: FnMut(&[f32]),
{
    if (gain - 1.0).abs() > f32::EPSILON {
        let scaled: Vec<f32> = fft.iter().map(|v| v * gain).collect();
        f(&scaled);
    } else {
        f(fft);
    }
}

// ── Colour palettes ─────────────────────────────────────────────────────────

pub const PALETTE_FIRE:     &[u8] = &[52, 88, 124, 160, 196, 202, 208, 214, 220, 226, 227, 228, 229, 230, 231];
pub const PALETTE_ICE:      &[u8] = &[17, 18, 19, 20, 21, 27, 33, 39, 45, 51, 87, 123, 159, 195, 231];
pub const PALETTE_OCEAN:    &[u8] = &[17, 18, 19, 20, 21, 27, 33, 39, 45, 51, 50, 49, 159, 195, 231];
pub const PALETTE_NEON:     &[u8] = &[201, 200, 165, 129, 93, 57, 21, 27, 33, 39, 45, 51, 87, 123, 159, 231];
pub const PALETTE_GOLD:     &[u8] = &[52, 94, 130, 136, 178, 214, 220, 226, 227, 228, 229, 230, 231, 255];
pub const PALETTE_SUNSET:   &[u8] = &[57, 93, 129, 165, 201, 200, 198, 197, 196, 202, 208, 214, 220, 226, 229];
pub const PALETTE_ARCTIC:   &[u8] = &[17, 18, 19, 21, 27, 33, 39, 45, 51, 87, 123, 159, 195, 231, 255];
pub const PALETTE_TROPICAL: &[u8] = &[22, 28, 34, 40, 46, 82, 118, 154, 190, 226, 220, 214, 208, 51, 87];

/// Look up a 256-colour ANSI code from a palette by fractional position [0, 1].
#[inline]
pub fn palette_lookup(frac: f32, palette: &[u8]) -> u8 {
    let len = palette.len();
    let i = (frac.clamp(0.0, 1.0) * (len - 1) as f32) as usize;
    palette[i.min(len - 1)]
}

// ── Rendering helpers ───────────────────────────────────────────────────────

/// Map a brightness value [0, 1] to a Unicode block character.
#[inline]
pub fn brightness_char(b: f32) -> char {
    if b > 0.75 {
        '█'
    } else if b > 0.50 {
        '▓'
    } else if b > 0.25 {
        '▒'
    } else {
        '░'
    }
}

/// Format a character with 256-colour ANSI foreground.
#[inline]
pub fn ansi_fg(ch: char, color: u8) -> String {
    format!("\x1b[38;5;{color}m{ch}\x1b[0m")
}

/// Format a character with bold + 256-colour ANSI foreground.
#[inline]
pub fn ansi_bold_fg(ch: char, color: u8) -> String {
    format!("\x1b[1m\x1b[38;5;{color}m{ch}\x1b[0m")
}

/// Format a string with dim + 256-colour ANSI foreground.
#[inline]
pub fn ansi_dim_fg(s: &str, color: u8) -> String {
    format!("\x1b[2m\x1b[38;5;{color}m{s}\x1b[0m")
}
