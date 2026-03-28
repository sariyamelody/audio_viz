/**
 * main.js — Application entry point.
 *
 * Loads the WASM module, sets up the audio capture and canvas renderer,
 * and runs the animation loop.
 */

import { AudioCapture } from './audio.js';
import { Renderer }     from './renderer.js';
import init, { WebViz } from './pkg/audio_viz_web.js';

const canvas        = document.getElementById('canvas');
const catSelect     = document.getElementById('cat-select');
const vizSelect     = document.getElementById('viz-select');
const settingsBtn   = document.getElementById('settings-btn');
const settingsPanel = document.getElementById('settings-panel');
const startBtn      = document.getElementById('start-btn');
const systemBtn     = document.getElementById('system-btn');
const overlayEl     = document.getElementById('overlay');
const overlayStart  = document.getElementById('overlay-start-btn');
const overlaySystem = document.getElementById('overlay-system-btn');
const statusEl      = document.getElementById('status');

const audio    = new AudioCapture();
const renderer = new Renderer(canvas);

let wasm    = null;
let viz     = null;
let running = false;
let rafId   = null;
let lastTs  = null;
let frameCount = 0;
let fpsSmooth  = 0;

// ── Initialise WASM ───────────────────────────────────────────────────────────

// categories: Array of [categoryName, [vizName, ...]] pairs, populated on init.
let categories = [];

async function initWasm() {
  wasm = await init();

  categories = JSON.parse(WebViz.all_categories());

  // Populate category dropdown
  for (const [cat] of categories) {
    const opt = document.createElement('option');
    opt.value = cat;
    opt.text  = cat;
    catSelect.appendChild(opt);
  }

  // Default to whichever category contains 'spectrum', else first category
  const defaultCat = categories.find(([, names]) => names.includes('spectrum'))?.[0]
    ?? categories[0]?.[0];
  if (defaultCat) catSelect.value = defaultCat;

  populateVizSelect(catSelect.value, 'spectrum');
  makeViz(vizSelect.value);

  // Show system audio button only on supported browsers (Chrome/Edge desktop)
  if (AudioCapture.systemAudioSupported()) {
    systemBtn.style.display     = '';
    overlaySystem.style.display = '';
  }
}

function populateVizSelect(catName, preferredViz = null) {
  vizSelect.innerHTML = '';
  const entry = categories.find(([cat]) => cat === catName);
  if (!entry) return;
  const [, names] = entry;
  for (const name of names) {
    const opt = document.createElement('option');
    opt.value = name;
    opt.text  = name;
    if (name === preferredViz) opt.selected = true;
    vizSelect.appendChild(opt);
  }
}

function makeViz(name) {
  viz?.free?.();
  viz = new WebViz(name, renderer.cols, renderer.rows);
  loadSettings(name);
}

// ── Settings UI ───────────────────────────────────────────────────────────────

const STORAGE_PREFIX = 'audio_viz_config_';

function buildSettingsUI(schemaJson) {
  settingsPanel.innerHTML = '';
  let schema;
  try { schema = JSON.parse(schemaJson); } catch { return; }
  const config = schema.config ?? [];
  if (config.length === 0) return;

  for (const entry of config) {
    const wrap = document.createElement('div');
    wrap.className = 'setting';

    const lbl = document.createElement('label');
    lbl.textContent = entry.display_name ?? entry.name;

    wrap.appendChild(lbl);

    if (entry.type === 'float' || entry.type === 'int') {
      const min  = entry.min  ?? 0;
      const max  = entry.max  ?? 1;
      const step = entry.type === 'int' ? 1 : (max - min) / 200;
      const val  = entry.value ?? min;

      const slider = document.createElement('input');
      slider.type  = 'range';
      slider.min   = min;
      slider.max   = max;
      slider.step  = step;
      slider.value = val;
      slider.dataset.name = entry.name;
      slider.dataset.type = entry.type;

      const readout = document.createElement('span');
      readout.className   = 'setting-val';
      readout.textContent = entry.type === 'int' ? val : Number(val).toFixed(2);

      slider.addEventListener('input', () => {
        readout.textContent = entry.type === 'int'
          ? slider.value
          : Number(slider.value).toFixed(2);
        applySettings();
      });

      wrap.appendChild(slider);
      wrap.appendChild(readout);

    } else if (entry.type === 'enum') {
      const sel = document.createElement('select');
      sel.dataset.name = entry.name;
      sel.dataset.type = 'enum';
      for (const v of (entry.variants ?? [])) {
        const opt = document.createElement('option');
        opt.value    = v;
        opt.text     = v;
        opt.selected = v === entry.value;
        sel.appendChild(opt);
      }
      sel.addEventListener('change', applySettings);
      wrap.appendChild(sel);

    } else if (entry.type === 'bool') {
      const cb = document.createElement('input');
      cb.type           = 'checkbox';
      cb.checked        = !!entry.value;
      cb.dataset.name   = entry.name;
      cb.dataset.type   = 'bool';
      cb.addEventListener('change', applySettings);
      wrap.appendChild(cb);
    }

    settingsPanel.appendChild(wrap);
  }

  const resetBtn = document.createElement('button');
  resetBtn.id          = 'reset-btn';
  resetBtn.textContent = 'Reset';
  resetBtn.addEventListener('click', () => {
    if (!viz) return;
    const name = viz.name();
    try { localStorage.removeItem(STORAGE_PREFIX + name); } catch { /* ignore */ }
    buildSettingsUI(viz.get_config());
    applySettings();
  });
  settingsPanel.appendChild(resetBtn);
}

function applySettings() {
  if (!viz) return;
  const controls = settingsPanel.querySelectorAll('[data-name]');
  const entries  = [];
  for (const ctrl of controls) {
    const type = ctrl.dataset.type;
    let value;
    if (type === 'bool')       value = ctrl.checked;
    else if (type === 'float') value = parseFloat(ctrl.value);
    else if (type === 'int')   value = parseInt(ctrl.value, 10);
    else                       value = ctrl.value;
    entries.push({ name: ctrl.dataset.name, value });
  }
  const partial = JSON.stringify({ config: entries });
  const merged  = viz.set_config(partial);
  const name    = viz.name();
  try { localStorage.setItem(STORAGE_PREFIX + name, merged); } catch { /* quota */ }
}

function loadSettings(name) {
  if (!viz) return;
  const saved = localStorage.getItem(STORAGE_PREFIX + name);
  let configJson;
  if (saved) {
    try { configJson = viz.set_config(saved); } catch { /* ignore stale data */ }
  }
  buildSettingsUI(configJson || viz.get_config());
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

  frameCount++;
  fpsSmooth = fpsSmooth * 0.92 + (1 / dt) * 0.08;
  if (frameCount % 30 === 0) {
    statusEl.textContent = `${fpsSmooth.toFixed(0)} fps`;
  }

  const { fft, left, right } = audio.getFrame();
  viz.tick(fft, left, right, dt, audio.sampleRate);

  const cellsJson = viz.render(fpsSmooth);
  const cells     = JSON.parse(cellsJson);
  renderer.drawFrame(cells);
}

// ── Start / stop ──────────────────────────────────────────────────────────────

async function start(mode) {
  if (running) return;

  startBtn.textContent     = '…';
  startBtn.disabled        = true;
  systemBtn.disabled       = true;
  overlayStart.disabled    = true;
  overlaySystem.disabled   = true;

  try {
    if (mode === 'system') {
      await audio.startSystem();
    } else {
      await audio.startMic();
    }
  } catch (err) {
    statusEl.textContent   = err.message || 'Audio access denied.';
    startBtn.textContent   = 'Microphone';
    startBtn.disabled      = false;
    systemBtn.disabled     = false;
    overlayStart.disabled  = false;
    overlaySystem.disabled = false;
    return;
  }

  overlayEl.classList.add('hidden');
  running = true;
  lastTs  = null;
  startBtn.textContent  = 'Stop';
  startBtn.disabled     = false;
  systemBtn.style.display = 'none'; // hide the alternate button while running
  rafId = requestAnimationFrame(loop);
}

function stop() {
  running = false;
  if (rafId !== null) { cancelAnimationFrame(rafId); rafId = null; }
  audio.stop();
  startBtn.textContent = 'Microphone';
  if (AudioCapture.systemAudioSupported()) {
    systemBtn.style.display = '';
    systemBtn.disabled = false;
  }
  statusEl.textContent = 'Stopped.';
}

startBtn.addEventListener('click',    () => running ? stop() : start('mic'));
systemBtn.addEventListener('click',   () => start('system'));
overlayStart.addEventListener('click',  () => start('mic'));
overlaySystem.addEventListener('click', () => start('system'));

// ── Settings toggle ───────────────────────────────────────────────────────────

settingsBtn.addEventListener('click', () => {
  const open = settingsPanel.classList.toggle('open');
  settingsBtn.classList.toggle('active', open);
  settingsBtn.textContent = open ? 'Settings ▲' : 'Settings ▼';
  handleResize();
});

// ── Visualizer switching ──────────────────────────────────────────────────────

catSelect.addEventListener('change', () => {
  populateVizSelect(catSelect.value);
  makeViz(vizSelect.value);
});

vizSelect.addEventListener('change', () => makeViz(vizSelect.value));

// ── Boot ──────────────────────────────────────────────────────────────────────

initWasm().catch(err => {
  statusEl.textContent = `Failed to load WASM: ${err}`;
  console.error(err);
});
