/**
 * audio.js — Microphone capture via the Web Audio API.
 *
 * Provides an AudioCapture class that:
 *   - Requests microphone permission
 *   - Runs an AnalyserNode for FFT magnitude data
 *   - Keeps a rolling buffer of raw PCM samples for left/right channels
 *   - Exposes getFrame() → { fft, left, right } each animation frame
 */

const FFT_SIZE    = 4096;
const SAMPLE_RATE = 44100;

export class AudioCapture {
  constructor() {
    this._ctx       = null;
    this._analyser  = null;
    this._fftBuf    = null;
    this._left      = new Float32Array(FFT_SIZE);
    this._right     = new Float32Array(FFT_SIZE);
    this._processor = null;
    this._stream    = null;
    this._started   = false;
  }

  /**
   * Request microphone access and start the audio graph.
   * Returns a Promise that resolves when audio is flowing.
   */
  async start() {
    if (this._started) return;

    this._stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        echoCancellation:   false,
        noiseSuppression:   false,
        autoGainControl:    false,
        sampleRate:         SAMPLE_RATE,
      },
      video: false,
    });

    this._ctx = new AudioContext({ sampleRate: SAMPLE_RATE });

    const source = this._ctx.createMediaStreamSource(this._stream);

    // ── Analyser for FFT magnitude ──────────────────────────────────────────
    this._analyser = this._ctx.createAnalyser();
    this._analyser.fftSize            = FFT_SIZE;
    this._analyser.smoothingTimeConstant = 0.0; // no smoothing — visualizers do their own
    this._fftBuf   = new Float32Array(this._analyser.frequencyBinCount);

    // ── ScriptProcessor for raw PCM ────────────────────────────────────────
    // ScriptProcessorNode is deprecated but remains the only cross-browser
    // way to extract per-channel PCM without an AudioWorklet build step.
    // bufferSize=4096 matches FFT_SIZE so we get a full window each callback.
    const bufSize       = FFT_SIZE;
    const channelCount  = Math.min(source.channelCount, 2);
    this._processor     = this._ctx.createScriptProcessor(bufSize, channelCount, channelCount);

    const leftBuf  = this._left;
    const rightBuf = this._right;

    this._processor.onaudioprocess = (ev) => {
      const L = ev.inputBuffer.getChannelData(0);
      leftBuf.set(L);
      // If mono input, mirror left → right
      const R = ev.inputBuffer.numberOfChannels > 1
        ? ev.inputBuffer.getChannelData(1)
        : L;
      rightBuf.set(R);
    };

    source.connect(this._analyser);
    source.connect(this._processor);
    // Processor must be connected to destination to fire callbacks
    this._processor.connect(this._ctx.destination);

    this._started = true;
  }

  /** Stop the audio graph and release the microphone. */
  stop() {
    if (!this._started) return;
    this._processor?.disconnect();
    this._analyser?.disconnect();
    this._stream?.getTracks().forEach(t => t.stop());
    this._ctx?.close();
    this._started = false;
  }

  /** Returns true once audio is flowing. */
  get isRunning() { return this._started; }

  /**
   * Snapshot the current audio state.
   * Returns { fft: Float32Array, left: Float32Array, right: Float32Array }
   * where fft contains linear magnitude values (not dBFS).
   */
  getFrame() {
    if (!this._analyser) {
      return {
        fft:   new Float32Array(FFT_SIZE / 2 + 1),
        left:  this._left,
        right: this._right,
      };
    }

    // AnalyserNode gives us dBFS; convert to linear magnitude to match
    // what the Rust visualizers expect from rustfft output.
    this._analyser.getFloatFrequencyData(this._fftBuf);
    const fft = new Float32Array(this._fftBuf.length);
    for (let i = 0; i < this._fftBuf.length; i++) {
      // dBFS → linear: 10^(dBFS/20), clamped to [0, 1]
      fft[i] = Math.min(1.0, Math.pow(10, this._fftBuf[i] / 20));
    }

    return {
      fft,
      left:  this._left,
      right: this._right,
    };
  }
}
