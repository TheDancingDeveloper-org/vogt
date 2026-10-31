// WI-1842: specs that only hold against the mocked `installFixtures` data and
// routes. The `live` project (a real, credential-free compose stack) skips
// them with `grepInvert: /@mock-only/`; the mocked projects still run them.
// Each call names why the spec can't run live.
export const LIVE_SKIP = {
  // A: the live stack seeds no Board/Backlog/Inbox items, sessions, files,
  // history or projects, and page.route fault injection has nothing to stub.
  data: "needs installFixtures data or page.route fault injection the live stack doesn't seed",
  // B: live seeds a token on an already-claimed instance.
  auth: "sign-in/first-run flow; the live instance is already claimed and signed in",
  // C: screenshot and geometry baselines are recorded on fixture data only.
  baseline:
    "screenshot/geometry baseline recorded against fixture data; no -live baselines",
  // D: the stack has no LLM/STT/TTS route.
  assistant: "assistant/voice/chat flow needs mocked model, STT and TTS routes",
  // E: harness assumptions that only hold on the Vite dev server.
  harness:
    "relies on a dev-server module graph, test window hook or emulated permission",
} as const;

export function mockOnly(reason: string) {
  return {
    tag: "@mock-only",
    annotation: { type: "mock-only", description: reason },
  };
}
