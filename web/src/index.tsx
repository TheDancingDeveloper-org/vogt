import { ErrorBoundary, render } from "solid-js/web";
import { HashRouter, Route } from "@solidjs/router";
import { APP_ROUTES } from "./routes";
import { migrateStorageKeys } from "./storageMigration";
import "./styles.css";
import { initializeRuntimeTransport } from "./runtimeTransport";
import { dialogIsOpen } from "./Dialog";
import { keyboardInsetFor } from "./viewportInsets";
import { installNativeBackButton } from "./nativeBack";

await initializeRuntimeTransport();

// Migrate historic browser-storage keys to the `vogt.*` prefix before any
// module reads a preference or the credential (#271; see storageMigration.ts).
// One-shot and idempotent.
migrateStorageKeys();

// `App` owns storage-backed singletons (tabs, layouts, recent places). Keep it
// out of the static import graph so the demo transport can seed those stores
// before their modules evaluate. The normal build takes the same path and the
// same chunks; only a valid runtime manifest selects the simulator.
const [
  { default: App },
  { registerServiceWorker },
  { applyAppTheme, initAppThemeSystemWatch },
] = await Promise.all([
  import("./App"),
  import("./push"),
  import("./appThemes"),
]);

function installVisualViewportSizing() {
  const apply = () => {
    const viewport = window.visualViewport;
    const width = viewport?.width ?? window.innerWidth;
    const height = viewport?.height ?? window.innerHeight;
    // Scale-aware: a pinch-zoomed viewport is a crop, not a keyboard
    // (viewportInsets.ts).
    const keyboardInset = keyboardInsetFor(window.innerHeight, viewport);

    document.documentElement.style.setProperty("--app-width", `${width}px`);
    document.documentElement.style.setProperty("--app-height", `${height}px`);
    document.documentElement.style.setProperty(
      "--keyboard-inset",
      `${keyboardInset}px`,
    );
    window.dispatchEvent(new CustomEvent("vogt:viewport-resize"));
  };

  apply();
  window.addEventListener("resize", apply);
  window.addEventListener("orientationchange", apply);
  window.visualViewport?.addEventListener("resize", apply);
  window.visualViewport?.addEventListener("scroll", apply);
}

function installNativeInsetsFallback() {
  const apply = () => {
    const root = document.documentElement;
    const dataset = root.dataset as DOMStringMap & {
      nativeInsetTop?: string;
      nativeInsetRight?: string;
      nativeInsetBottom?: string;
      nativeInsetLeft?: string;
    };
    const keys = {
      top: "nativeInsetTop",
      right: "nativeInsetRight",
      bottom: "nativeInsetBottom",
      left: "nativeInsetLeft",
    } as const;
    for (const side of ["top", "right", "bottom", "left"] as const) {
      const key = keys[side];
      const raw = dataset[key];
      root.style.setProperty(`--native-safe-${side}`, raw ? `${raw}px` : "0px");
    }
  };

  apply();
  // Prefer the renamed event, but keep listening for the historic name: the
  // native-insets event is dispatched by the human-gated Android shell, whose
  // rename is deferred to #265. Both are harmless no-ops when unused.
  window.addEventListener("vogt:native-insets", apply);
  window.addEventListener("mydevenv2:native-insets", apply);
}

// Register the SW eagerly so push subscriptions can be created from the
// Settings modal without waiting for first-paint.
void registerServiceWorker();
// The inline script in index.html has already set `data-theme` before paint;
// re-apply here to sync the `theme-color` meta and adopt any legacy selection,
// then follow the OS while the reader is on "System" (#299).
applyAppTheme();
initAppThemeSystemWatch();
installVisualViewportSizing();
installNativeInsetsFallback();
installNativeBackButton(dialogIsOpen);

// HashRouter avoids needing an SPA fallback configured on the embedding
// Rust server: every navigation stays under index.html.
render(
  () => (
    <ErrorBoundary
      fallback={(error) => (
        <main class="app-error" role="alert">
          <h1>Vogt could not render this view</h1>
          <p>{error instanceof Error ? error.message : String(error)}</p>
          <button type="button" onClick={() => window.location.reload()}>
            Reload
          </button>
        </main>
      )}
    >
      <HashRouter>
        {/* The paths themselves are in `routes.ts`, so a test can mount the
            shell behind the same table this bundle routes with. */}
        <Route path={[...APP_ROUTES]} component={App} />
      </HashRouter>
    </ErrorBoundary>
  ),
  document.getElementById("root")!,
);
