// The live call's browser half in a real Chromium (WI-960): the capture
// worklet really runs and frames the microphone as the engine expects, and
// the playback queue really plays decoded clips and reports them. jsdom has
// neither an AudioWorklet nor a decoder, so this is where they are proven.
// Chromium's fake capture device stands in for a microphone (a steady beep).
import { expect, test } from "@playwright/test";

test.use({
  launchOptions: {
    args: [
      "--use-fake-device-for-media-stream",
      "--use-fake-ui-for-media-stream",
      "--autoplay-policy=no-user-gesture-required",
    ],
  },
  permissions: ["microphone"],
});

test.beforeEach(({}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "audio plumbing is device-independent");
});

test("the capture worklet streams 20 ms frames of 16 kHz PCM16", async ({ page }) => {
  await page.goto("/");
  const result = await page.evaluate(async () => {
    const { startCapture } = await import("/src/callCapture.ts");
    const frames: ArrayBuffer[] = [];
    const capture = await startCapture({ onFrame: (frame) => frames.push(frame) });
    await new Promise((resolve) => setTimeout(resolve, 1500));
    capture.stop();
    const samples = frames.flatMap((f) => Array.from(new Int16Array(f)));
    return {
      count: frames.length,
      sizes: [...new Set(frames.map((f) => f.byteLength))],
      peak: samples.reduce((m, s) => Math.max(m, Math.abs(s)), 0),
    };
  });
  // ~50 frames a second, each exactly 320 samples.
  expect(result.sizes).toEqual([640]);
  expect(result.count).toBeGreaterThan(40);
  expect(result.count).toBeLessThan(110);
  // The fake device's beep made it through, not silence.
  expect(result.peak).toBeGreaterThan(500);
});

test("the playback queue plays clips in order and reports them", async ({ page }) => {
  await page.goto("/");
  const events = await page.evaluate(async () => {
    const { CallPlayer } = await import("/src/callPlayer.ts");
    const { sharedAudioContext, primeAudio } = await import("/src/audioPlayback.ts");
    primeAudio();
    // A 200 ms 440 Hz WAV at 24 kHz, as a TTS backend would send.
    const wav = (ms: number): ArrayBuffer => {
      const rate = 24000;
      const n = Math.round((rate * ms) / 1000);
      const buf = new ArrayBuffer(44 + n * 2);
      const v = new DataView(buf);
      const w = (o: number, s: string) => [...s].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
      w(0, "RIFF");
      v.setUint32(4, 36 + n * 2, true);
      w(8, "WAVEfmt ");
      v.setUint32(16, 16, true);
      v.setUint16(20, 1, true);
      v.setUint16(22, 1, true);
      v.setUint32(24, rate, true);
      v.setUint32(28, rate * 2, true);
      v.setUint16(32, 2, true);
      v.setUint16(34, 16, true);
      w(36, "data");
      v.setUint32(40, n * 2, true);
      for (let i = 0; i < n; i += 1) {
        v.setInt16(44 + i * 2, Math.round(Math.sin((2 * Math.PI * 440 * i) / rate) * 8000), true);
      }
      return buf;
    };
    const log: string[] = [];
    await new Promise<void>((resolve) => {
      const player = new CallPlayer(sharedAudioContext(), {
        onFirstAudio: (id) => log.push(`first:${id}`),
        onStarted: (id, index) => log.push(`started:${id}:${index}`),
        onIdle: (id) => {
          log.push(`idle:${id}`);
          resolve();
        },
      });
      player.enqueue("r1", 0, wav(200));
      player.enqueue("r1", 1, wav(200));
    });
    return log;
  });
  expect(events).toEqual(["first:r1", "started:r1:0", "started:r1:1", "idle:r1"]);
});
