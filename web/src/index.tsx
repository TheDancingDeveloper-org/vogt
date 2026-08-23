import { ErrorBoundary, render } from "solid-js/web";
import { HashRouter, Route } from "@solidjs/router";
import App from "./App";
import { APP_ROUTES } from "./routes";
import { registerServiceWorker } from "./push";
import { migrateStorageKeys } from "./storageMigration";
import "./styles.css";

function installVisualViewportSizing() {
  const apply = () => {
    const viewport = window.visualViewport;
    const width = viewport?.width ?? window.innerWidth;
    const height = viewport?.height ?? window.innerHeight;
    const keyboardInset = Math.max(
      0,
      window.innerHeight - height - (viewport?.offsetTop ?? 0),
    );

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
  window.addEventListener("vogt:native-insets", apply);
  // The Android shell (MainActivity.java) still dispatches the historic
  // `mydevenv2:native-insets` name; keep listening for it so the shipped app
  // keeps working until its native half is renamed under the human-gated #265.
  window.addEventListener("mydevenv2:native-insets", apply);
}

// Register the SW eagerly so push subscriptions can be created from the
// Settings modal without waiting for first-paint.
// Rename historic `mydevenv2.*` storage keys to `vogt.*` before anything reads
// a preference (#271). One-shot and cheap after the first load.
migrateStorageKeys();

void registerServiceWorker();
installVisualViewportSizing();
installNativeInsetsFallback();

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
