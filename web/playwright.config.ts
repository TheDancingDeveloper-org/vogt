import { defineConfig, devices } from "@playwright/test";

// Parallel worktrees must not silently reuse another product's dev server.
// CI keeps the conventional port; local agents can allocate an isolated one.
const port = Number.parseInt(process.env.PLAYWRIGHT_PORT ?? "4173", 10);

// The `live` project (#295) runs the same specs against a *running* stack —
// real API, no `installFixtures` — instead of the mocked Vite dev server, so
// mock/truth drift fails a test. It is opt-in: the project exists only when
// `PLAYWRIGHT_LIVE_BASE_URL` names the front door to point at. That is the
// guard the issue requires — the default and PR invocations set nothing, so
// `playwright test` never runs `live`, and the mocked `desktop`/`phone`
// projects are untouched. The e2e workflow sets the variable and selects it
// explicitly with `--project=live`.
//
// A front-door token is needed to get past the sign-in gate against a real
// core; `PLAYWRIGHT_LIVE_TOKEN` carries it (see gui.spec.ts, which seeds it
// into localStorage in live mode the way the mocked run seeds its fake token).
const liveBaseURL = process.env.PLAYWRIGHT_LIVE_BASE_URL;

export default defineConfig({
  testDir: "./tests/browser",
  timeout: 30_000,
  fullyParallel: false,
  reporter: process.env.CI ? "line" : "list",
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    browserName: "chromium",
    headless: true,
    ...(process.env.CHROMIUM_PATH
      ? { launchOptions: { executablePath: process.env.CHROMIUM_PATH } }
      : {}),
    viewport: { width: 1280, height: 900 },
    trace: "retain-on-failure",
  },
  projects: [
    { name: "desktop", use: { ...devices["Desktop Chrome"] } },
    { name: "phone", use: { ...devices["iPhone 13"] } },
    // Registered only when a live stack is named, so a bare `playwright test`
    // (PR CI, local runs) can neither list nor run it against nothing.
    ...(liveBaseURL
      ? [
          {
            name: "live",
            use: { ...devices["Desktop Chrome"], baseURL: liveBaseURL },
          },
        ]
      : []),
  ],
  // The mocked projects need the Vite dev server; the live project must not
  // start it — it points at the real stack. So the dev server is configured
  // only when we are not in live mode.
  ...(liveBaseURL
    ? {}
    : {
        webServer: {
          command: `pnpm exec vite --host 127.0.0.1 --port ${port}`,
          url: `http://127.0.0.1:${port}`,
          reuseExistingServer: !process.env.CI,
          timeout: 30_000,
        },
      }),
});
