# audio_viz

A terminal and WebAssembly audio visualizer written in Rust. Visualizers render to ASCII with 256-color ANSI codes — the terminal displays them directly; the web frontend parses ANSI to RGB for canvas rendering.

## Architecture

```
Core Library (src/)          Web Frontend (web/)
├── visualizer.rs            ├── src/lib.rs      (WASM bindings via wasm-bindgen)
├── visualizers/             ├── main.js         (UI, audio, render loop)
│   ├── frequency/           ├── audio.js        (mic + system audio capture)
│   ├── scopes/              ├── renderer.js     (canvas ANSI→RGB renderer)
│   ├── effects/             ├── processor.worklet.js
│   └── abstract/            └── index.html
└── main.rs                  (terminal CLI + cpal audio)
```

The same Rust library compiles to both a native terminal binary and a WASM module. Terminal-specific crates (`crossterm`, `cpal`, `clap`) are gated behind the `terminal` feature and excluded from WASM builds.

## Build Commands

**Terminal binary:**
```bash
cargo build --release
./target/release/audio_viz [VISUALIZER] [OPTIONS]
./target/release/audio_viz --list           # list visualizers
./target/release/audio_viz --list-devices   # list audio devices
```

**WASM (from web/ directory):**
```bash
cd web
wasm-pack build --target web --out-dir pkg --release
```

## Adding a Visualizer

Visualizers are **auto-discovered at compile time** — no manual registration needed.

1. Create `src/visualizers/<category>/<name>.rs`
2. Implement the `Visualizer` trait (`tick`, `render`, `get_default_config`, `set_config`, `name`)
3. `build.rs` scans the category subdirectories and generates `registry.rs` automatically

Existing categories: `frequency`, `scopes`, `effects`, `abstract`

## Visualizer Trait

```rust
pub trait Visualizer: Send {
    fn name(&self)        -> &str;
    fn description(&self) -> &str;
    fn tick(&mut self, audio: &AudioFrame, dt: f32, size: TermSize);
    fn render(&self, size: TermSize, fps: f32) -> Vec<String>;
    fn on_resize(&mut self, _size: TermSize) {}  // optional
    fn get_default_config(&self) -> String;      // JSON string
    fn set_config(&mut self, json: &str) -> Result<String, String>;
}
```

- `render()` returns ANSI-escaped strings (one per row); `&self` not `&mut self`
- Config uses JSON schema with `float`, `enum`, and `boolean` setting types
- `get_default_config()` / `set_config()` use JSON strings, not `serde_json::Value`
- `AudioFrame` contains `.left`, `.right`, `.mono` (PCM) and `.fft` (spectrum magnitude)

## Key Constants (src/visualizer.rs)

- `SAMPLE_RATE`: 44,100 Hz
- `FFT_SIZE`: 4,096
- `CHANNELS`: 2
- `FPS_TARGET`: 45

## Shared Utilities (src/visualizer_utils.rs)

Common DSP helpers, colour palettes, and rendering primitives extracted from all visualizers. Import selectively:

```rust
use crate::visualizer_utils::{rms, band_energy, freq_to_bin, smooth_asymmetric,
    palette_lookup, brightness_char, ansi_fg, with_gained_fft,
    PALETTE_FIRE, PALETTE_ICE, PALETTE_OCEAN, PALETTE_NEON,
    PALETTE_GOLD, PALETTE_SUNSET, PALETTE_ARCTIC, PALETTE_TROPICAL};
```

Index: `rms@15` · `freq_to_bin@25` · `band_energy@32` · `mag_to_frac@41` · `smooth_asymmetric@51` · `with_gained_fft@60` · palettes@74 · `palette_lookup@85` · `brightness_char@95` · `ansi_fg@109` · `ansi_bold_fg@115` · `ansi_dim_fg@121`

## File Index Convention

Every visualizer file begins with a single-line index comment immediately before the first `use` statement:

```rust
// ── Index: MyViz@42 · new@55 · impl@90 · config@94 · set_config@120 · tick@145 · render@160 · register@210
```

The index lists key section names and their line numbers **in the file as it exists on disk** (accounting for the index line itself). Use these numbers to jump directly to a section when reading or editing a visualizer. When making edits that shift line numbers, update the index comment to match.

## Config Persistence

- Linux: `~/.config/audio_viz/` (or `$XDG_CONFIG_HOME/audio_viz/`)
- macOS: `~/Library/Application Support/audio_viz/`
- Settings accessible at runtime: **F1** in terminal, **Settings button** in web UI

## CI

GitHub Actions (`.github/workflows/build.yml`) builds on Linux, macOS (x86_64 + ARM), Windows, and WASM. Main branch and tags produce release artifacts; WASM is deployed to GitHub Pages.
