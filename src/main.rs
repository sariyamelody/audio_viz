/// main.rs — Core application: CLI, audio capture, FFT pipeline, render loop.
///
/// Responsibilities
/// ────────────────
/// 1. Parse CLI arguments (clap).
/// 2. Enumerate audio devices and select an input source.
/// 3. Spawn an audio capture thread (cpal) that fills a lock-free ring buffer.
/// 4. Run the render loop on the main thread:
///      a. Drain the ring buffer into a window of FFT_SIZE samples.
///      b. Apply a Hann window and compute the rfft magnitude spectrum (rustfft).
///      c. Call viz.tick() with the AudioFrame.
///      d. Call viz.render() and write the result to stdout via crossterm.
///      e. Handle terminal resize events.
///      f. Sleep to target FPS_TARGET frames per second.

mod visualizer;
mod visualizers;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    style::Print,
    terminal::{self, ClearType},
};
use rustfft::{FftPlanner, num_complex::Complex};

use visualizer::{
    AudioFrame, TermSize, Visualizer,
    CHANNELS, FFT_SIZE, FPS_TARGET, SAMPLE_RATE,
};

// ── ALSA / JACK stderr silencer ──────────────────────────────────────────────
//
// ALSA's C library and the JACK client library write diagnostic messages
// directly to file-descriptor 2, bypassing Rust's stderr — including from
// background threads cpal spawns, so a scoped redirect on the main thread
// is not sufficient.
//
// Strategy: redirect fd 2 -> /dev/null permanently before the first cpal call.
// Our own messages are written through the real stderr fd we saved beforehand,
// via the diag!() macro below.
//
// On non-Linux platforms this is a no-op and diag!() falls back to eprintln!.

#[cfg(target_os = "linux")]
mod stderr_silence {
    use std::fs::OpenOptions;
    use std::os::unix::io::IntoRawFd;
    use std::sync::atomic::{AtomicI32, Ordering};

    // Saved copy of the original fd 2.  -1 means suppress() not yet called.
    static SAVED_STDERR: AtomicI32 = AtomicI32::new(-1);

    /// Redirect fd 2 -> /dev/null permanently.
    /// Call before the first cpal call and before spawning any threads.
    /// Safe to call multiple times; subsequent calls are no-ops.
    pub fn suppress() {
        if SAVED_STDERR.load(Ordering::Relaxed) >= 0 { return; }
        let saved = unsafe { libc::dup(2) };
        if saved < 0 { return; }
        SAVED_STDERR.store(saved, Ordering::Relaxed);
        if let Ok(dev) = OpenOptions::new().write(true).open("/dev/null") {
            unsafe { libc::dup2(dev.into_raw_fd(), 2); }
        }
    }

    /// Write a message to the real terminal stderr, bypassing the redirect.
    pub fn write_err(msg: &str) {
        let fd = SAVED_STDERR.load(Ordering::Relaxed);
        if fd < 0 { return; }
        let b = msg.as_bytes();
        unsafe { libc::write(fd, b.as_ptr() as *const libc::c_void, b.len()); }
    }
}

#[cfg(not(target_os = "linux"))]
mod stderr_silence {
    pub fn suppress() {}
    pub fn write_err(msg: &str) { eprintln!("{msg}"); }
}

/// Print a diagnostic message to the real terminal stderr.
/// Works even after stderr_silence::suppress() has redirected fd 2.
macro_rules! diag {
    ($($arg:tt)*) => {
        stderr_silence::write_err(&format!("{}
", format_args!($($arg)*)))
    };
}

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name        = "audio_viz",
    about       = "Multi-mode terminal audio visualizer",
    long_about  = None,
)]
struct Cli {
    /// Visualizer to run (use --list to see all options).
    #[arg(default_value = "spectrum")]
    visualizer: String,

    /// Audio input device name or index.
    /// On Linux: PulseAudio/PipeWire source name (e.g. alsa_output.*.monitor).
    /// On macOS: CoreAudio device name or index (requires BlackHole for system audio).
    /// Omit to auto-detect the best loopback source.
    #[arg(short, long)]
    device: Option<String>,

    /// List all available visualizers and exit.
    #[arg(short, long)]
    list: bool,

    /// List all available audio input devices and exit.
    #[arg(long)]
    list_devices: bool,

    /// Target frames per second (default: 45).
    #[arg(long, default_value_t = FPS_TARGET)]
    fps: f32,
}

// ── Ring buffer ───────────────────────────────────────────────────────────────

/// Shared audio ring buffer: audio thread writes, render thread reads.
///
/// We use a simple Arc<Mutex<Vec<f32>>> rather than a lock-free structure.
/// At 45 fps the render thread holds the lock for <1 ms, which is far less
/// than the ~93 ms audio chunk period, so contention is negligible.
type RingBuf = Arc<Mutex<Vec<f32>>>;

fn make_ring() -> RingBuf {
    // Pre-allocate enough for ~4 FFT windows of stereo audio
    Arc::new(Mutex::new(Vec::with_capacity(FFT_SIZE * CHANNELS * 4)))
}

// ── Audio host selection ──────────────────────────────────────────────────────
//
// cpal 0.15 on Linux only compiles one host backend: ALSA.  Native PipeWire
// and PulseAudio host backends do not exist in any released version of cpal.
//
// To capture system audio on a PipeWire/PulseAudio system we use ALSA's "pulse"
// PCM plugin as the device name.  When opened, the pulse plugin connects to
// the running PipeWire-PulseAudio daemon and, because we set PULSE_SOURCE to
// the monitor source name, records what is being played to the speakers rather
// than the microphone.
//
// select_host() simply returns the default host on all platforms; the real
// work is done in find_best_device() / prepare_pulse_env() below.

fn select_host() -> cpal::Host {
    cpal::default_host()
}

// ── PulseAudio environment setup (Linux) ──────────────────────────────────────
//
// Sets PULSE_SOURCE to the first .monitor source reported by pactl so that
// when cpal opens the "pulse" ALSA device it captures system audio output
// rather than the microphone.
//
// Returns Ok(monitor_name) on success, or an Err with a human-readable
// message explaining what is missing and how to fix it.
//
// Must be called before any threads are spawned (set_var safety).
#[cfg(target_os = "linux")]
fn prepare_pulse_env(host: &cpal::Host) -> anyhow::Result<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    use std::process::Command;

    // ── Step 1: check that the "pulse" ALSA PCM plugin is present ─────────────
    // libasound2-plugins provides /usr/lib/.../libasound_module_pcm_pulse.so
    // and adds the "pulse" device name to ALSA's device list.
    let pulse_available = host
        .input_devices()
        .map(|mut devs| devs.any(|d| d.name().map(|n| n == "pulse").unwrap_or(false)))
        .unwrap_or(false);

    if !pulse_available {
        anyhow::bail!(
            "The ALSA PulseAudio plugin is not installed.
             
             audio_viz requires this plugin to capture system audio through
             PipeWire or PulseAudio.  Install it with:
             
               Debian/Ubuntu:  sudo apt install libasound2-plugins
               Fedora:         sudo dnf install alsa-plugins-pulse
               Arch:           sudo pacman -S alsa-plugins
             
             After installing, run audio_viz again.
             
             Alternatively, select a specific device with --device:
               audio_viz --list-devices
               audio_viz --device <name>"
        );
    }

    // ── Step 2: find the .monitor source via pactl ────────────────────────────
    // pactl is provided by pulseaudio-utils (Debian) / pipewire-pulse (Arch/Fedora).
    let out = Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
        .map_err(|_| anyhow::anyhow!(
            "Could not run `pactl`.  Ensure PipeWire or PulseAudio is running
             and pulseaudio-utils (or equivalent) is installed."
        ))?;

    let stdout = String::from_utf8_lossy(&out.stdout);

    let monitor = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .find(|name| name.contains(".monitor"))
        .ok_or_else(|| anyhow::anyhow!(
            "`pactl list short sources` returned no .monitor source.
             Ensure PipeWire or PulseAudio is running."
        ))?
        .to_string();

    // ── Step 3: export PULSE_SOURCE so libpulse captures the right source ─────
    // Safety: no threads have been spawned yet at this call site.
    unsafe { std::env::set_var("PULSE_SOURCE", &monitor) };

    Ok(monitor)
}

#[cfg(not(target_os = "linux"))]
fn prepare_pulse_env(_host: &cpal::Host) -> anyhow::Result<String> {
    // Non-Linux: this function is never called; return a dummy value.
    Ok(String::new())
}

// ── Audio device selection ────────────────────────────────────────────────────

/// Check whether the "pulse" ALSA device is present in the device list.
/// Used to give a helpful --list-devices hint when it is absent.
#[cfg(target_os = "linux")]
fn pulse_device_present(host: &cpal::Host) -> bool {
    use cpal::traits::{DeviceTrait, HostTrait};
    host.input_devices()
        .map(|mut devs| devs.any(|d| d.name().map(|n| n == "pulse").unwrap_or(false)))
        .unwrap_or(false)
}

/// Find the capture device to use when --device is not specified.
///
/// On Linux the "pulse" ALSA plugin is the only valid auto-detected choice;
/// prepare_pulse_env() must already have been called successfully.
///
/// On macOS: tries BlackHole/Loopback, then the default input.
fn find_best_device(host: &cpal::Host) -> Option<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};

    // Linux: use the "pulse" ALSA device exclusively for auto-detection.
    // prepare_pulse_env() has already verified it exists and set PULSE_SOURCE.
    #[cfg(target_os = "linux")]
    if let Ok(mut devs) = host.input_devices() {
        if let Some(d) = devs.find(|d| d.name().map(|n| n == "pulse").unwrap_or(false)) {
            return Some(d);
        }
    }

    // macOS: loopback drivers (BlackHole, Loopback by Rogue Amoeba)
    #[cfg(not(target_os = "linux"))]
    {
        if let Ok(mut devs) = host.input_devices() {
            if let Some(d) = devs.find(|d| {
                d.name().map(|n| {
                    let lc = n.to_lowercase();
                    lc.contains("blackhole") || lc.contains("loopback")
                }).unwrap_or(false)
            }) {
                return Some(d);
            }
        }
        // macOS fallback: default input with a warning
        diag!("audio: no loopback device found.");
        diag!("       Install BlackHole for system audio: https://existential.audio/blackhole/");
    }

    host.default_input_device()
}

/// Find a device by name substring or numeric index string.
fn find_device_by_name(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let name_lc = name.to_lowercase();

    // Try as a name substring first
    if let Ok(mut devs) = host.input_devices() {
        if let Some(d) = devs.find(|d| {
            d.name().map(|n| n.to_lowercase().contains(&name_lc)).unwrap_or(false)
        }) {
            return Some(d);
        }
    }

    // Try as a numeric index
    if let Ok(idx) = name.parse::<usize>() {
        if let Ok(devs) = host.input_devices() {
            return devs.into_iter().nth(idx);
        }
    }

    None
}

// ── Hann window ───────────────────────────────────────────────────────────────

/// Pre-compute a Hann window of length `n`.
fn hann_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos()))
        .collect()
}

// ── FFT pipeline ──────────────────────────────────────────────────────────────

/// Compute rfft magnitude spectrum from `mono` samples.
///
/// Returns a Vec of length `FFT_SIZE / 2 + 1`.
fn compute_fft(
    mono:   &[f32],
    window: &[f32],
    planner: &mut FftPlanner<f32>,
) -> Vec<f32> {
    let n = FFT_SIZE;

    // Build complex input with Hann window applied
    let mut input: Vec<Complex<f32>> = (0..n)
        .map(|i| {
            let s = if i < mono.len() { mono[i] } else { 0.0 };
            Complex::new(s * window[i], 0.0)
        })
        .collect();

    let fft = planner.plan_fft_forward(n);
    fft.process(&mut input);

    // Take magnitude of the first half (rfft)
    let scale = 1.0 / n as f32;
    input[..n / 2 + 1]
        .iter()
        .map(|c| c.norm() * scale)
        .collect()
}

// ── Terminal helpers ──────────────────────────────────────────────────────────

fn term_size() -> TermSize {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    TermSize { rows, cols }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let cli = Cli::parse();

    // ── Build visualizer registry ─────────────────────────────────────────────
    // all_visualizers() is generated by build.rs and calls register() on each
    // discovered file in src/visualizers/.
    let all_vizs = visualizers::all_visualizers();

    // ── --list ────────────────────────────────────────────────────────────────
    if cli.list {
        println!("Available visualizers:");
        for v in &all_vizs {
            println!("  {:12}  {}", v.name(), v.description());
        }
        return Ok(());
    }

    // ── Audio host ────────────────────────────────────────────────────────────
    // Silence fd 2 permanently before the first cpal call.
    // All subsequent ALSA/JACK noise goes to /dev/null.
    // Our own messages use diag!() which writes to the saved real fd.
    stderr_silence::suppress();
    let host = select_host();

    // ── --list-devices ────────────────────────────────────────────────────────
    if cli.list_devices {
        println!("Available input devices (host: {}):", host.id().name());
        for (i, d) in host.input_devices()?.enumerate() {
            println!("  [{}] {}", i, d.name().unwrap_or_else(|_| "?".into()));
        }
        // On Linux, warn if the pulse plugin is absent so the user knows why
        // the visualizer won't start without --device.
        #[cfg(target_os = "linux")]
        if !pulse_device_present(&host) {
            diag!("");
            diag!("WARNING: The ALSA PulseAudio plugin (\"pulse\" device) was not found.");
            diag!("         System audio capture will not work without it.");
            diag!("         Install with: sudo apt install libasound2-plugins");
        }
        return Ok(());
    }

    // ── Select input device ──────────────────────────────────────────────────
    let device = match &cli.device {
        Some(name) => {
            // Explicit --device: use it directly, no pulse check required.
            find_device_by_name(&host, name)
                .ok_or_else(|| anyhow::anyhow!("Device not found: {name}\nRun --list-devices to see available devices."))?
        }
        None => {
            // No --device: on Linux we require the pulse plugin so we can
            // capture system audio.  Bail with a clear message if it is missing
            // rather than silently falling back to the microphone.
            #[cfg(target_os = "linux")]
            let monitor = prepare_pulse_env(&host)?; // exits with error if pulse absent
            #[cfg(target_os = "linux")]
            diag!("audio: monitor source → {monitor}");

            find_best_device(&host)
                .ok_or_else(|| anyhow::anyhow!(
                    "No suitable input device found.\n\
                     On macOS install BlackHole: https://existential.audio/blackhole/\n\
                     Use --list-devices to see what is available."
                ))?
        }
    };

    let device_name = device.name().unwrap_or_else(|_| "unknown".into());

    // ── Select input format ───────────────────────────────────────────────────
    // We request stereo f32 at SAMPLE_RATE; fall back to the device default
    // if our preferred config is not supported.
    let config = {
        let preferred = cpal::StreamConfig {
            channels:    CHANNELS as u16,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };
        // Check if the device supports f32
        let supported = device
            .supported_input_configs()
            .map(|mut it| it.any(|c| {
                c.sample_format() == cpal::SampleFormat::F32
                    && (c.channels() as usize == CHANNELS
                        || c.channels() >= 1)
            }))
            .unwrap_or(false);

        if supported { preferred } else {
            device.default_input_config()?.into()
        }
    };

    let actual_channels = config.channels as usize;

    // ── Spawn audio capture thread ────────────────────────────────────────────
    let ring  = make_ring();
    let ring2 = Arc::clone(&ring);

    // The cpal stream callback writes interleaved f32 samples into the ring.
    // We convert to mono by averaging all channels.
    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _| {
            let mut buf = ring2.lock().unwrap();
            for frame in data.chunks(actual_channels) {
                if actual_channels >= 2 {
                    buf.push(frame[0]); // left
                    buf.push(frame[1]); // right
                } else {
                    buf.push(frame[0]); // mono → both channels
                    buf.push(frame[0]);
                }
            }
            // Cap the ring to prevent unbounded growth if the render thread lags
            const MAX_RING: usize = FFT_SIZE * CHANNELS * 8;
            if buf.len() > MAX_RING {
                let drain = buf.len() - MAX_RING;
                buf.drain(0..drain);
            }
        },
        |err| eprintln!("[audio error] {err}"),
        None,
    )?;
    stream.play()?;

    // ── Select and initialise visualizer ──────────────────────────────────────
    let viz_name = cli.visualizer.to_lowercase();
    let mut viz: Box<dyn Visualizer> = {
        // Find by name in registry first; if not found, print list and exit.
        // We construct from the registry with a source name injected.
        // Each visualizer's register() returns a placeholder with source="";
        // we replace it by matching on name and constructing directly.
        let found = all_vizs.iter().any(|v| v.name() == viz_name);
        if !found {
            eprintln!("Unknown visualizer '{viz_name}'.");
            eprintln!("Available: {}", all_vizs.iter().map(|v| v.name()).collect::<Vec<_>>().join(", "));
            std::process::exit(1);
        }

        // Construct the chosen visualizer with the device name as source string.
        // We match by name so each visualizer can pass the source to its constructor.
        // New visualizers added via build.rs are automatically available via registry.
        match viz_name.as_str() {
            "spectrum"  => Box::new(visualizers::spectrum ::SpectrumViz ::new(&device_name)),
            "scope"     => Box::new(visualizers::scope    ::ScopeViz    ::new(&device_name)),
            "matrix"    => Box::new(visualizers::matrix   ::MatrixViz   ::new(&device_name)),
            "radial"    => Box::new(visualizers::radial   ::RadialViz   ::new(&device_name)),
            "lissajous" => Box::new(visualizers::lissajous::LissajousViz::new(&device_name)),
            "fire"      => Box::new(visualizers::fire     ::FireViz     ::new(&device_name)),
            "vu"        => Box::new(visualizers::vu       ::VuViz       ::new(&device_name)),
            // Visualizers added via build.rs that are not listed above will use
            // the placeholder from register() (source = "").
            // To inject the device name, add a match arm above.
            _ => {
                // Fall back to the registry instance
                all_vizs.into_iter().find(|v| v.name() == viz_name).unwrap()
            }
        }
    };

    // ── Terminal setup ────────────────────────────────────────────────────────
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        cursor::Hide,
        terminal::Clear(ClearType::All),
    )?;

    // Restore terminal on exit (via a simple guard struct)
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                terminal::LeaveAlternateScreen,
                cursor::Show,
            );
        }
    }
    let _guard = Guard;

    // ── FFT setup ─────────────────────────────────────────────────────────────
    let window  = hann_window(FFT_SIZE);
    let mut planner = FftPlanner::<f32>::new();

    // Sliding mono window of FFT_SIZE samples, updated each frame
    let mut mono_window: Vec<f32> = vec![0.0; FFT_SIZE];
    // Corresponding left/right windows for per-channel visualizers (scope, lissajous)
    let mut left_window:  Vec<f32> = vec![0.0; FFT_SIZE];
    let mut right_window: Vec<f32> = vec![0.0; FFT_SIZE];

    // ── Render loop ───────────────────────────────────────────────────────────
    let frame_duration = Duration::from_secs_f32(1.0 / cli.fps);
    let mut fps_display = cli.fps;
    const FPS_ALPHA: f32 = 0.08;

    let mut size = term_size();
    viz.on_resize(size);

    let mut t_prev = Instant::now();

    loop {
        let t0 = Instant::now();

        // ── Poll for quit / resize events (non-blocking) ──────────────────────
        while event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(KeyEvent { code: KeyCode::Char('q'), .. })
                | Event::Key(KeyEvent { code: KeyCode::Char('c'),
                                        modifiers: event::KeyModifiers::CONTROL, .. }) => {
                    return Ok(());
                }
                Event::Resize(cols, rows) => {
                    size = TermSize { rows, cols };
                    viz.on_resize(size);
                    execute!(stdout, terminal::Clear(ClearType::All))?;
                }
                _ => {}
            }
        }

        // Also poll size directly in case resize events were missed
        let current_size = term_size();
        if current_size != size {
            size = current_size;
            viz.on_resize(size);
            execute!(stdout, terminal::Clear(ClearType::All))?;
        }

        // ── Drain ring buffer → sliding sample windows ────────────────────────
        {
            let mut buf = ring.lock().unwrap();
            if !buf.is_empty() {
                // buf contains interleaved stereo pairs (L, R, L, R, ...)
                // Each stereo pair is 2 f32 values.
                let n_pairs = buf.len() / 2;
                let take    = n_pairs.min(FFT_SIZE);

                // Slide existing data left to make room for new samples
                let keep = FFT_SIZE - take;
                left_window .copy_within(take.., 0);
                right_window.copy_within(take.., 0);
                mono_window .copy_within(take.., 0);

                // Copy new samples from the front of the ring
                let start_pair = n_pairs.saturating_sub(take);
                for i in 0..take {
                    let pair_idx = (start_pair + i) * 2;
                    if pair_idx + 1 < buf.len() {
                        let l = buf[pair_idx];
                        let r = buf[pair_idx + 1];
                        left_window [keep + i] = l;
                        right_window[keep + i] = r;
                        mono_window [keep + i] = (l + r) * 0.5;
                    }
                }
                buf.clear();
            }
        }

        // ── Compute FFT ───────────────────────────────────────────────────────
        let fft_out = compute_fft(&mono_window, &window, &mut planner);

        // ── Build AudioFrame ──────────────────────────────────────────────────
        let dt = {
            let now  = Instant::now();
            let secs = (now - t_prev).as_secs_f32().clamp(1e-4, 0.15);
            t_prev   = now;
            secs
        };

        let frame = AudioFrame {
            left:        left_window.clone(),
            right:       right_window.clone(),
            mono:        mono_window.clone(),
            fft:         fft_out,
            sample_rate: SAMPLE_RATE,
        };

        // ── Tick + render ─────────────────────────────────────────────────────
        viz.tick(&frame, dt, size);
        let rendered = viz.render(size, fps_display);

        // ── Write frame to terminal ───────────────────────────────────────────
        // Move to top-left and overwrite line by line.
        // Using crossterm's queued API to minimise syscalls.
        execute!(stdout, cursor::MoveTo(0, 0))?;
        let rows = size.rows as usize;
        for (i, line) in rendered.iter().take(rows).enumerate() {
            execute!(
                stdout,
                Print(line),
                // Erase to end of line in case the new line is shorter than the old
                terminal::Clear(ClearType::UntilNewLine),
            )?;
            if i + 1 < rows {
                execute!(stdout, Print("\r\n"))?;
            }
        }
        stdout.flush()?;

        // ── FPS tracking ──────────────────────────────────────────────────────
        let elapsed  = t0.elapsed();
        let inst_fps = 1.0 / elapsed.as_secs_f32().max(1e-6);
        fps_display  = FPS_ALPHA * inst_fps + (1.0 - FPS_ALPHA) * fps_display;

        // ── Frame cap ─────────────────────────────────────────────────────────
        if let Some(sleep) = frame_duration.checked_sub(elapsed) {
            std::thread::sleep(sleep);
        }
    }
}
