// The PWA's test runner (NFR-Q1, and the gap `REQUIREMENTS.md` §6 names first).
//
// Vitest rather than anything else for one reason: the bundle is already a
// Vite build, so the transform pipeline the tests run through is the same one
// `pnpm build` uses. A second toolchain would be a second set of resolution
// rules to keep true, and the first thing it would get wrong is Solid's JSX.
//
// Kept in its own file rather than as a `test` block in `vite.config.ts`
// because of `resolve.conditions` below: it is wrong for the shipped bundle
// and required for the tests, and one file that means two things depending on
// who loaded it is how a build starts differing from what was tested.

import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  resolve: {
    // Solid ships two builds. `browser` is the one with a real DOM renderer,
    // and `development` is the one that keeps the reactive graph's dev
    // warnings — without both, a component under test renders through the
    // server bundle and every `onMount` silently never runs.
    conditions: ["development", "browser"],
  },
  test: {
    environment: "jsdom",
    // `src/**` only: `tests/test_pwa.py` reads `web/src/*.ts[x]` as text and
    // knows nothing about this directory, which is why the tests live one
    // level down in `__tests__/` — see the note at the top of `harness.tsx`.
    include: ["src/__tests__/**/*.test.tsx", "src/__tests__/**/*.test.ts"],
    setupFiles: ["src/__tests__/setup.ts"],
    // A surface that leaks a timer or a listener between tests is a surface
    // whose next test is reading the previous one's state.
    restoreMocks: true,
    clearMocks: true,
  },
});
