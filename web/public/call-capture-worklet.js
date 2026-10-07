// Live call microphone capture (WI-960). Runs on the audio thread: takes the
// microphone at the context's own rate, averages it down to 16 kHz (the
// average doubles as the low-pass that keeps the decimation from aliasing),
// and posts 20 ms frames of little-endian PCM16 — exactly what
// `/api/assistant/call` reads. A static, same-origin file because the PWA's
// Content-Security-Policy is `script-src 'self'`: a worklet built from a blob
// URL would be refused.
const TARGET_RATE = 16000;
const FRAME = 320; // 20 ms at 16 kHz

class VogtCallCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.step = sampleRate / TARGET_RATE;
    this.acc = 0;
    this.count = 0;
    this.position = 0;
    this.frame = new Int16Array(FRAME);
    this.filled = 0;
  }

  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (!channel) return true;
    for (let i = 0; i < channel.length; i++) {
      this.acc += channel[i];
      this.count += 1;
      this.position += 1;
      if (this.position >= this.step) {
        this.position -= this.step;
        const sample = Math.max(-1, Math.min(1, this.acc / this.count));
        this.acc = 0;
        this.count = 0;
        this.frame[this.filled++] = sample < 0 ? sample * 0x8000 : sample * 0x7fff;
        if (this.filled === FRAME) {
          const out = this.frame.buffer;
          this.port.postMessage(out, [out]);
          this.frame = new Int16Array(FRAME);
          this.filled = 0;
        }
      }
    }
    return true;
  }
}

registerProcessor("vogt-call-capture", VogtCallCapture);
