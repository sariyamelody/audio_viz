/// lib.rs — Library entry point.
///
/// The `visualizer` module and the `visualizers` sub-modules are pure Rust
/// with no platform dependencies and compile to both native and wasm32.
///
/// Modules that use crossterm/cpal/clap are only compiled when the
/// `terminal` feature is active (the default for native builds).
pub mod beat;
pub mod visualizer;
pub mod visualizer_utils;
pub mod visualizers;
