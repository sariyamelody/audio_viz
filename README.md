# audio_viz

A multi-mode terminal audio visualizer written in Rust. Captures system audio
and renders real-time ASCII visualizations in your terminal using 256-colour
ANSI escape codes.

```
./audio_viz lissajous
./audio_viz spectrum
./audio_viz fire
```

---

## Requirements

### Linux
- PipeWire or PulseAudio (for system audio capture)
- `libasound2-plugins` — provides the ALSA `pulse` device bridge
- `pulseaudio-utils` — provides `pactl` for monitor source detection

```bash
sudo apt install libasound2-plugins pulseaudio-utils
```

### macOS
- [BlackHole](https://existential.audio/blackhole/) — virtual loopback driver for system audio capture
- Set BlackHole as your audio output (or create a Multi-Output Device in Audio MIDI Setup to hear audio simultaneously)

---

## Building

```bash
cargo build --release
./target/release/audio_viz
```

Or for development:

```bash
cargo build
./target/debug/audio_viz
```

---

## Usage

```
audio_viz [VISUALIZER] [OPTIONS]

Arguments:
  [VISUALIZER]   Visualizer to run [default: spectrum]

Options:
  -d, --device <DEVICE>   Audio input device name or index
  -l, --list              List all available visualizers
      --list-devices      List all available audio input devices
      --fps <FPS>         Target frames per second [default: 45]
  -h, --help              Show help
```

**Examples:**

```bash
# Run with auto-detected system audio source
./audio_viz
./audio_viz lissajous
./audio_viz fire

# Specify a device explicitly (use --list-devices to find names)
./audio_viz spectrum --device pulse
./audio_viz spectrum --device 2

# List everything
./audio_viz --list
./audio_viz --list-devices
```

**Exit:** press `q` or `Ctrl-C`.

---

## Visualizers

| Name | Description |
|---|---|
| `spectrum` | Log-spaced frequency bars with peak markers |
| `scope` | Dual-channel time-domain oscilloscope |
| `matrix` | Audio-reactive falling character rain |
| `radial` | Polar spectrum radiating from the centre |
| `lissajous` | Full-terminal XY oscilloscope — beat rotation, planets, vocal stars, ripples |
| `fire` | Audio-reactive ASCII fire |
| `vu` | Stereo VU meter (also a minimal reference implementation) |

---

## Architecture

```
audio_viz/
├── Cargo.toml
├── build.rs                     — auto-discovers visualizer files at compile time
└── src/
    ├── main.rs                  — CLI, audio capture, FFT pipeline, render loop
    ├── visualizer.rs            — Visualizer trait, AudioFrame, shared DSP helpers
    └── visualizers/
        ├── mod.rs               — includes build.rs-generated registry
        ├── spectrum.rs
        ├── scope.rs
        ├── matrix.rs
        ├── radial.rs
        ├── lissajous.rs
        ├── fire.rs
        └── vu.rs
```

**Audio pipeline:** `cpal` captures raw PCM from the system audio source into a
ring buffer. Each render frame, the main thread drains the buffer, applies a
Hann window, and computes an rfft magnitude spectrum via `rustfft`. The
resulting `AudioFrame` (left, right, mono, fft) is passed to the active
visualizer's `tick()` and `render()` methods.

**Registry:** `build.rs` scans `src/visualizers/*.rs` at compile time and
generates `OUT_DIR/registry.rs`, which declares each file as a `pub mod` with
an absolute `#[path]` attribute and emits an `all_visualizers()` factory that
calls each file's `register()` function. This is `include!`-ed by
`src/visualizers/mod.rs`. Adding a new visualizer requires no changes to any
existing file.

**Stderr silencing (Linux):** ALSA and JACK write diagnostics directly to
file-descriptor 2, bypassing Rust's stderr, including from cpal's internal
audio callback thread. `stderr_silence::suppress()` saves the real fd 2 and
permanently redirects it to `/dev/null` before the first cpal call. The
`diag!()` macro writes to the saved fd so application messages still reach the
terminal.

---

## Adding a Visualizer

1. Create `src/visualizers/myvis.rs`
2. Implement the `Visualizer` trait
3. Export `pub fn register() -> Vec<Box<dyn Visualizer>>`
4. Run `cargo build` — it appears automatically in `--list`

Minimal example:

```rust
use crate::visualizer::{pad_frame, status_bar, AudioFrame, TermSize, Visualizer};

pub struct MyViz { source: String }

impl MyViz {
    pub fn new(source: &str) -> Self {
        Self { source: source.to_string() }
    }
}

impl Visualizer for MyViz {
    fn name(&self)        -> &str { "myvis" }
    fn description(&self) -> &str { "My visualizer" }

    fn tick(&mut self, _audio: &AudioFrame, _dt: f32, _size: TermSize) {
        // update state from audio here
    }

    fn render(&self, size: TermSize, fps: f32) -> Vec<String> {
        let rows = size.rows as usize;
        let cols = size.cols as usize;
        let mut lines = vec!["hello, world!".to_string()];
        lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
        pad_frame(lines, rows, cols)
    }
}

pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(MyViz::new(""))]
}
```

Then add a match arm in `main.rs` to inject the device name:

```rust
"myvis" => Box::new(visualizers::myvis::MyViz::new(&device_name)),
```

See `src/visualizers/vu.rs` for a fully annotated reference implementation,
including documentation of all available `AudioFrame` fields and shared helpers.

---

## Developer Notes

**`diag!()` vs `eprintln!()`**
On Linux, `eprintln!` writes to fd 2 which is permanently redirected to
`/dev/null` once the audio subsystem starts. Use `diag!()` for any message
that needs to reach the terminal after that point. `eprintln!` is fine for
errors that occur before audio initialisation (e.g. argument parsing).

**`Cargo.lock` is committed**
This is a binary crate. Committing the lock file ensures reproducible builds
across machines and in CI. This is the correct behaviour for binaries.

**`lissajous.rs` is complex**
It has seven rendering layers (orbit rings, spokes, phase dots, nucleus, vocal
stars, planets, beat ripples, spectrum shell) and several interacting subsystems
(beat detector, rotation physics, vocal onset detector, planet orbital
mechanics). Everything is documented inline. The geometry caches
(`ring_cache`, `shell_cache`) are populated in `tick()` — not `render()` —
because `render()` takes `&self` and cannot mutate state.

**Runtime plugin loading**
The current registry is compile-time only. The `Visualizer` trait is already
the right shape for runtime plugins: extract `visualizer.rs` into a
`audio_viz_core` crate, have plugins export
`extern "C" fn viz_register() -> *mut Vec<Box<dyn Visualizer>>`, and load them
with `libloading`. Nothing about the trait needs to change.
