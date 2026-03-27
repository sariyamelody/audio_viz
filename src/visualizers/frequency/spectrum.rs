/// spectrum.rs — Classic log-spaced vertical frequency bar visualizer.

// ── Index: SpectrumViz@70 · new@78 · render_helpers@89 · impl@222 · config@226 · set_config@250 · tick@271 · render@276 · register@374
use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar, hline, title_line,
    AudioFrame, SpectrumBars, TermSize, Visualizer,
};
use crate::visualizer_utils::with_gained_fft;

const CONFIG_VERSION: u64 = 2;

// ── HiFi / LED shared band definitions ───────────────────────────────────────

const HIFI_BANDS: &[(f32, &str)] = &[
    (25.0,    "25"),
    (40.0,    "40"),
    (63.0,    "63"),
    (100.0,  "100"),
    (160.0,  "160"),
    (250.0,  "250"),
    (500.0,  "500"),
    (1000.0,  "1k"),
    (2000.0,  "2k"),
    (4000.0,  "4k"),
    (8000.0,  "8k"),
    (16000.0,"16k"),
];

// ── Theme ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Theme {
    Classic,  // rainbow gradient left→right
    HiFi,     // vintage VFD teal — 12 fixed bands, half-block segments
    Led,      // red LED bar graph — 12 fixed bands, sparse bar / solid peak
    Phosphor, // mysterious green phosphor CRT
    Mono,     // utilitarian monochrome
}

impl Theme {
    fn from_str(s: &str) -> Self {
        match s {
            "hifi"     => Theme::HiFi,
            "led"      => Theme::Led,
            "phosphor" => Theme::Phosphor,
            "mono"     => Theme::Mono,
            _          => Theme::Classic,
        }
    }
}

// ── Shared 12-band layout config ──────────────────────────────────────────────

/// Everything that differs between the HiFi and LED themes.
struct BandLayout {
    title:       &'static str,
    title_color: u8,
    rule_color:  u8,
    label_color: u8,
    /// Colour for a lit bar segment at the given normalised height [0,1].
    bar_color:   fn(f32) -> u8,
    bar_char:    &'static str,
    peak_char:   &'static str,
    peak_color:  u8,
}

// ── Visualizer struct ─────────────────────────────────────────────────────────

pub struct SpectrumViz {
    bars:   SpectrumBars,
    source: String,
    gain:   f32,
    theme:  Theme,
}

impl SpectrumViz {
    pub fn new(source: &str) -> Self {
        Self {
            bars:   SpectrumBars::new(80),
            source: source.to_string(),
            gain:   1.0,
            theme:  Theme::Classic,
        }
    }

    // ── Band-layout rendering (HiFi + LED) ───────────────────────────────────

    fn render_band_frame(&self, layout: &BandLayout, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = (rows.saturating_sub(4)).max(4);

        let n   = HIFI_BANDS.len(); // 12
        let gap = 1usize;

        let bar_w    = ((cols.saturating_sub((n - 1) * gap)) / n).clamp(3, 9);
        let total_w  = n * bar_w + (n - 1) * gap;
        let left_pad = cols.saturating_sub(total_w) / 2;

        // Sample smoothed/peak at each band's log-spaced position.
        let n_bars = self.bars.smoothed.len().max(1);
        let log_lo = 30f32.log10();
        let log_hi = 18_000f32.log10();

        let band_vals: Vec<(f32, f32)> = HIFI_BANDS.iter().map(|(freq, _)| {
            let frac = (freq.log10() - log_lo) / (log_hi - log_lo);
            let idx  = ((frac * (n_bars - 1) as f32) as usize).min(n_bars - 1);
            (self.bars.smoothed[idx], self.bars.peaks[idx])
        }).collect();

        let mut lines = Vec::with_capacity(rows);
        lines.push(title_line(cols, layout.title, layout.title_color));
        lines.push(hline(cols, layout.rule_color));

        for row in (0..vis).rev() {
            let threshold = row as f32 / vis as f32;
            let mut line  = String::with_capacity(cols * 14);

            for _ in 0..left_pad { line.push(' '); }

            for (bi, &(bh, ph)) in band_vals.iter().enumerate() {
                if bi > 0 { line.push(' '); }

                let pkr  = (ph * vis as f32) as usize;
                let cell = if bh >= threshold {
                    let color = (layout.bar_color)(threshold);
                    format!("\x1b[38;5;{color}m{}\x1b[0m", layout.bar_char)
                } else if pkr > 0 && row == pkr - 1 && ph > 0.03 {
                    format!("\x1b[1m\x1b[38;5;{}m{}\x1b[0m", layout.peak_color, layout.peak_char)
                } else {
                    String::from(" ")
                };

                for _ in 0..bar_w { line.push_str(&cell); }
            }
            lines.push(line);
        }

        lines.push(hline(cols, layout.rule_color));

        // Frequency labels centred under each bar.
        let mut label_line = String::with_capacity(cols * 10);
        for _ in 0..left_pad { label_line.push(' '); }
        for (bi, &(_, lbl)) in HIFI_BANDS.iter().enumerate() {
            if bi > 0 { label_line.push(' '); }
            let lbl_len = lbl.len();
            let pad_l   = (bar_w.saturating_sub(lbl_len)) / 2;
            let pad_r   = bar_w.saturating_sub(lbl_len + pad_l);
            for _ in 0..pad_l { label_line.push(' '); }
            label_line.push_str(&format!("\x1b[38;5;{}m{lbl}\x1b[0m", layout.label_color));
            for _ in 0..pad_r { label_line.push(' '); }
        }
        lines.push(label_line);
        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));

        pad_frame(lines, rows, cols)
    }

    // ── Band colour functions ─────────────────────────────────────────────────

    /// VFD teal: uniform colour throughout the bar.
    fn hifi_bar_color(_threshold: f32) -> u8 { 30 }

    /// Red LED: uniform pure red throughout.
    fn led_bar_color(_threshold: f32) -> u8 { 160 }

    // ── Per-cell renderers for the full-width themes ──────────────────────────

    fn render_classic(row: usize, vis: usize, bh: f32, ph: f32, threshold: f32, frac: f32) -> Option<String> {
        let code = specgrad(frac);
        let pkr  = (ph * vis as f32) as usize;
        if bh >= threshold {
            let pfx = if threshold > 0.75 {
                "\x1b[1m"
            } else if threshold < 0.25 {
                "\x1b[2m"
            } else {
                ""
            };
            Some(format!("{pfx}\x1b[38;5;{code}m|\x1b[0m"))
        } else if pkr > 0 && row == pkr - 1 && ph > 0.03 {
            Some(format!("\x1b[1m\x1b[38;5;{code}m*\x1b[0m"))
        } else {
            None
        }
    }

    fn render_phosphor(row: usize, vis: usize, bh: f32, ph: f32, threshold: f32) -> Option<String> {
        let pkr   = (ph * vis as f32) as usize;
        let color: u8 = if threshold >= 0.75 { 82 } else if threshold >= 0.35 { 40 } else { 22 };

        if bh >= threshold {
            let pfx = if threshold >= 0.75 { "\x1b[1m" } else { "" };
            Some(format!("{pfx}\x1b[38;5;{color}m|\x1b[0m"))
        } else if pkr > 0 && row == pkr - 1 && ph > 0.03 {
            Some(format!("\x1b[1m\x1b[38;5;82m•\x1b[0m"))
        } else if threshold < 0.04 {
            Some(format!("\x1b[38;5;22m·\x1b[0m"))
        } else {
            None
        }
    }

    fn render_mono(row: usize, vis: usize, bh: f32, ph: f32, threshold: f32) -> Option<String> {
        let pkr   = (ph * vis as f32) as usize;
        let color: u8 = if threshold >= 0.80 { 255 } else if threshold >= 0.50 { 245 } else { 238 };

        if bh >= threshold {
            let ch = if threshold >= 0.80 { "▓" } else if threshold >= 0.50 { "▒" } else { "░" };
            Some(format!("\x1b[38;5;{color}m{ch}\x1b[0m"))
        } else if pkr > 0 && row == pkr - 1 && ph > 0.03 {
            Some(format!("\x1b[38;5;250m-\x1b[0m"))
        } else {
            None
        }
    }
}

// ── Visualizer impl ───────────────────────────────────────────────────────────

impl Visualizer for SpectrumViz {
    fn name(&self)        -> &str { "spectrum" }
    fn description(&self) -> &str { "Classic log-spaced frequency bars" }

    fn get_default_config(&self) -> String {
        serde_json::json!({
            "visualizer_name": "spectrum",
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
                    "name": "theme",
                    "display_name": "Theme",
                    "type": "enum",
                    "value": "classic",
                    "variants": ["classic", "hifi", "led", "phosphor", "mono"]
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
                    "gain"  => self.gain  = entry["value"].as_f64().unwrap_or(1.0) as f32,
                    "theme" => self.theme = Theme::from_str(entry["value"].as_str().unwrap_or("classic")),
                    _ => {}
                }
            }
        }
        Ok(merged)
    }

    fn on_resize(&mut self, size: TermSize) {
        self.bars.resize(size.cols as usize);
    }

    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize) {
        self.bars.resize(size.cols as usize);
        with_gained_fft(&audio.fft, self.gain, |fft| self.bars.update(fft, dt));
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        match self.theme {
            Theme::HiFi => return self.render_band_frame(&BandLayout {
                title:       " SPECTRUM ANALYZER ",
                title_color: 44,
                rule_color:  23,
                label_color: 37,
                bar_color:   Self::hifi_bar_color,
                bar_char:    "▄",
                peak_char:   "▀",
                peak_color:  255,
            }, size, fps),

            Theme::Led => return self.render_band_frame(&BandLayout {
                title:       " SPECTRUM ANALYZER ",
                title_color: 196,
                rule_color:  88,
                label_color: 160,
                bar_color:   Self::led_bar_color,
                bar_char:    "░",
                peak_char:   "▄",
                peak_color:  196,
            }, size, fps),

            _ => {}
        }

        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = (rows.saturating_sub(4)).max(4);

        let (title_color, rule_color): (u8, u8) = match self.theme {
            Theme::Classic  => (255, 238),
            Theme::Phosphor => (40,  22),
            Theme::Mono     => (250, 236),
            _               => unreachable!(),
        };

        let title_label = match self.theme {
            Theme::Phosphor => " ◈ SPECTRUM ◈ ",
            Theme::Mono     => " SPECTRUM ",
            _               => " SPECTRUM ANALYZER ",
        };

        let mut lines = Vec::with_capacity(rows);
        lines.push(title_line(cols, title_label, title_color));
        lines.push(hline(cols, rule_color));

        for row in (0..vis).rev() {
            let threshold = row as f32 / vis as f32;
            let mut line  = String::with_capacity(cols * 12);

            for bi in 0..cols {
                let bh   = self.bars.smoothed[bi.min(self.bars.smoothed.len() - 1)];
                let ph   = self.bars.peaks   [bi.min(self.bars.peaks.len()    - 1)];
                let frac = bi as f32 / (cols - 1).max(1) as f32;

                let cell = match self.theme {
                    Theme::Classic  => Self::render_classic(row, vis, bh, ph, threshold, frac),
                    Theme::Phosphor => Self::render_phosphor(row, vis, bh, ph, threshold),
                    Theme::Mono     => Self::render_mono(row, vis, bh, ph, threshold),
                    _               => unreachable!(),
                };
                line.push_str(cell.as_deref().unwrap_or(" "));
            }
            lines.push(line);
        }

        lines.push(hline(cols, rule_color));

        let label_color: u8 = match self.theme {
            Theme::Phosphor => 40,
            Theme::Mono     => 244,
            _               => 245,
        };
        let mut label_row: Vec<u8> = vec![b' '; cols];
        let log_lo = 30f32.log10();
        let log_hi = 18_000f32.log10();
        for (freq, lbl) in &[
            (30u32, "30"), (60, "60"), (125, "125"), (250, "250"),
            (500, "500"), (1000, "1k"), (2000, "2k"), (4000, "4k"),
            (8000, "8k"), (16000, "16k"),
        ] {
            let f    = (*freq as f32).log10();
            let frac = (f - log_lo) / (log_hi - log_lo);
            let col  = ((frac * (cols - 1) as f32) as usize).min(cols - 1);
            for (i, ch) in lbl.bytes().enumerate() {
                if col + i < cols { label_row[col + i] = ch; }
            }
        }
        let label_str = String::from_utf8(label_row).unwrap_or_default();
        lines.push(format!("\x1b[38;5;{label_color}m{label_str}\x1b[0m"));
        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));

        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(SpectrumViz::new(""))]
}
