import { render } from "solid-js/web";
import { HashRouter, Route } from "@solidjs/router";
import App from "./App";
import { registerServiceWorker } from "./push";
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
    window.dispatchEvent(new CustomEvent("mydevenv2:viewport-resize"));
  };

  apply();
  window.addEventListener("resize", apply);
  window.addEventListener("orientationchange", apply);
  window.visualViewport?.addEventListener("resize", apply);
  window.visualViewport?.addEventListener("scroll", apply);
}

// Register the SW eagerly so push subscriptions can be created from the
// Settings modal without waiting for first-paint.
void registerServiceWorker();
installVisualViewportSizing();

// HashRouter avoids needing an SPA fallback configured on the embedding
// Rust server: every navigation stays under index.html.
render(
  () => (
    <HashRouter>
      <Route path="/" component={App} />
      <Route path="/t/:id" component={App} />
      <Route path="/e/*path" component={App} />
      <Route path="/g" component={App} />
      <Route path="/g/*path" component={App} />
      <Route path="/gui" component={App} />
    </HashRouter>
  ),
  document.getElementById("root")!,
);
