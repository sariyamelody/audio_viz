/// spectrum.rs — Classic log-spaced vertical frequency bar visualizer.

use crate::visualizer::{
    merge_config,
    pad_frame, specgrad, status_bar, hline, title_line,
    AudioFrame, SpectrumBars, TermSize, Visualizer,
};

const CONFIG_VERSION: u64 = 1;

pub struct SpectrumViz {
    bars:   SpectrumBars,
    source: String,
    // ── Config fields ──────────────────────────────────────────────────────
    /// Linear amplitude multiplier applied to the FFT before bar mapping.
    gain:   f32,
}

impl SpectrumViz {
    pub fn new(source: &str) -> Self {
        Self {
            bars:   SpectrumBars::new(80),
            source: source.to_string(),
            gain:   1.0,
        }
    }
}

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
                    "gain" => self.gain = entry["value"].as_f64().unwrap_or(1.0) as f32,
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
        if (self.gain - 1.0).abs() > f32::EPSILON {
            let scaled: Vec<f32> = audio.fft.iter().map(|v| v * self.gain).collect();
            self.bars.update(&scaled, dt);
        } else {
            self.bars.update(&audio.fft, dt);
        }
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let vis  = (rows.saturating_sub(4)).max(4);

        let mut lines = Vec::with_capacity(rows);
        lines.push(title_line(cols, " SPECTRUM ANALYZER ", 255));
        lines.push(hline(cols, 238));

        for row in (0..vis).rev() {
            let threshold = row as f32 / vis as f32;
            let mut line  = String::with_capacity(cols * 12);

            for bi in 0..cols {
                let bh   = self.bars.smoothed[bi.min(self.bars.smoothed.len() - 1)];
                let ph   = self.bars.peaks   [bi.min(self.bars.peaks.len()    - 1)];
                let frac = bi as f32 / (cols - 1).max(1) as f32;
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
                    line.push_str(&format!("{pfx}\x1b[38;5;{code}m|\x1b[0m"));
                } else if pkr > 0 && row == pkr - 1 && ph > 0.03 {
                    line.push_str(&format!("\x1b[1m\x1b[38;5;{code}m*\x1b[0m"));
                } else {
                    line.push(' ');
                }
            }
            lines.push(line);
        }

        lines.push(hline(cols, 238));

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
        lines.push(format!("\x1b[38;5;245m{}\x1b[0m", label_str));
        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));

        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(SpectrumViz::new(""))]
}
