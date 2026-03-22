/**
 * renderer.js — Canvas renderer for audio_viz WebAssembly output.
 *
 * Receives the sparse cell array from WebViz.render() and paints each
 * character to a <canvas> using fillText.  The canvas is sized to show
 * exactly `cols × rows` monospace characters.
 *
 * Font metrics are measured once on init (or on resize) so character
 * positioning is always pixel-accurate regardless of the font chosen.
 */

const FONT_FAMILY = "'JetBrains Mono', 'Fira Code', 'Cascadia Code', 'Consolas', 'Menlo', monospace";
const FONT_SIZE_PX = 13;

export class Renderer {
  /**
   * @param {HTMLCanvasElement} canvas
   */
  constructor(canvas) {
    this._canvas  = canvas;
    this._ctx     = canvas.getContext('2d');
    this._charW   = 0;
    this._charH   = 0;
    this._cols    = 0;
    this._rows    = 0;
    this._measure();
  }

  _measure() {
    const ctx = this._ctx;
    ctx.font = `${FONT_SIZE_PX}px ${FONT_FAMILY}`;
    // Use 'M' as a representative wide character for the monospace cell width.
    this._charW = ctx.measureText('M').width;
    // Line height: font size + a small leading so characters don't overlap.
    this._charH = Math.ceil(FONT_SIZE_PX * 1.2);
  }

  /** Recalculate grid dimensions to fill the canvas. */
  resize() {
    const dpr = window.devicePixelRatio || 1;
    const w   = this._canvas.clientWidth;
    const h   = this._canvas.clientHeight;

    this._canvas.width  = Math.round(w * dpr);
    this._canvas.height = Math.round(h * dpr);

    this._ctx.scale(dpr, dpr);
    this._measure();

    this._cols = Math.max(1, Math.floor(w / this._charW));
    this._rows = Math.max(1, Math.floor(h / this._charH));
  }

  get cols() { return this._cols; }
  get rows() { return this._rows; }

  /**
   * Paint one frame.
   * @param {Array<{ch,col,row,r,g,b,bold,dim}>} cells  — parsed from WASM JSON
   */
  drawFrame(cells) {
    const ctx   = this._ctx;
    const charW = this._charW;
    const charH = this._charH;

    // Clear to black
    ctx.fillStyle = '#000000';
    ctx.fillRect(0, 0, this._canvas.clientWidth, this._canvas.clientHeight);

    // Draw each cell
    for (const cell of cells) {
      if (cell.col >= this._cols || cell.row >= this._rows) continue;

      const x = cell.col * charW;
      const y = cell.row * charH;

      const weight = cell.bold ? 'bold' : 'normal';
      ctx.font      = `${weight} ${FONT_SIZE_PX}px ${FONT_FAMILY}`;
      ctx.fillStyle = `rgb(${cell.r},${cell.g},${cell.b})`;
      // Baseline offset: position text so the top of the glyph aligns with y
      ctx.fillText(cell.ch, x, y + FONT_SIZE_PX);
    }
  }
}
