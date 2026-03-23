/**
 * main.js — Application entry point.
 *
 * Loads the WASM module, sets up the audio capture and canvas renderer,
 * and runs the animation loop.
 */

import { AudioCapture } from './audio.js';
import { Renderer }     from './renderer.js';
// wasm-pack outputs these two files into pkg/
import init, { WebViz } from './pkg/audio_viz_web.js';

const canvas       = document.getElementById('canvas');
const vizSelect    = document.getElementById('viz-select');
const startBtn     = document.getElementById('start-btn');
const overlayEl    = document.getElementById('overlay');
const overlayStart = document.getElementById('overlay-start-btn');
const statusEl     = document.getElementById('status');

const audio    = new AudioCapture();
const renderer = new Renderer(canvas);

let wasm    = null;   // initialised WASM module
let viz     = null;   // WebViz instance
let running = false;
let rafId   = null;
let lastTs  = null;
let frameCount = 0;
let fpsSmooth  = 0;

// ── Initialise WASM ───────────────────────────────────────────────────────────

async function initWasm() {
  wasm = await init();

  // Populate the visualizer selector, defaulting to scope
  const names = JSON.parse(WebViz.all_names());
  for (const name of names) {
    const opt  = document.createElement('option');
    opt.value  = name;
    opt.text   = name;
    if (name === 'scope') opt.selected = true;
    vizSelect.appendChild(opt);
  }

  // Create the initial viz
  makeViz(vizSelect.value);
}

function makeViz(name) {
  viz?.free?.();  // release WASM memory for previous instance
  viz = new WebViz(name, renderer.cols, renderer.rows);
}

// ── Resize handling ───────────────────────────────────────────────────────────

function handleResize() {
  renderer.resize();
  viz?.resize(renderer.cols, renderer.rows);
}

window.addEventListener('resize', handleResize);
handleResize();

// ── Animation loop ────────────────────────────────────────────────────────────

function loop(ts) {
  if (!running) return;

  rafId = requestAnimationFrame(loop);

  const dt = lastTs === null ? 1 / 60 : Math.min((ts - lastTs) / 1000, 0.15);
  lastTs = ts;

  // FPS tracking
  frameCount++;
  fpsSmooth = fpsSmooth * 0.92 + (1 / dt) * 0.08;
  if (frameCount % 30 === 0) {
    statusEl.textContent = `${fpsSmooth.toFixed(0)} fps`;
  }

  // Get audio data and tick the visualizer
  const { fft, left, right } = audio.getFrame();
  viz.tick(fft, left, right, dt, audio.sampleRate);

  // Render and paint
  const cellsJson = viz.render(fpsSmooth);
  const cells     = JSON.parse(cellsJson);
  renderer.drawFrame(cells);
}

// ── Start / stop ──────────────────────────────────────────────────────────────

async function start() {
  if (running) return;

  startBtn.textContent    = '…';
  startBtn.disabled       = true;
  overlayStart.disabled   = true;

  try {
    await audio.start();
  } catch (err) {
    statusEl.textContent   = 'Microphone access denied.';
    startBtn.textContent   = 'Start';
    startBtn.disabled      = false;
    overlayStart.disabled  = false;
    return;
  }

  overlayEl.classList.add('hidden');
  running = true;
  lastTs  = null;
  startBtn.textContent  = 'Stop';
  startBtn.disabled     = false;
  rafId = requestAnimationFrame(loop);
}

function stop() {
  running = false;
  if (rafId !== null) { cancelAnimationFrame(rafId); rafId = null; }
  audio.stop();
  startBtn.textContent = 'Start';
  statusEl.textContent = 'Stopped.';
}

startBtn.addEventListener('click', () => {
  running ? stop() : start();
});
overlayStart.addEventListener('click', start);

// ── Visualizer switching ──────────────────────────────────────────────────────

vizSelect.addEventListener('change', () => {
  makeViz(vizSelect.value);
});

// ── Boot ──────────────────────────────────────────────────────────────────────

initWasm().catch(err => {
  statusEl.textContent = `Failed to load WASM: ${err}`;
  console.error(err);
});
