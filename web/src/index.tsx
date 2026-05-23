import { render } from "solid-js/web";
import { HashRouter, Route } from "@solidjs/router";
import App from "./App";
import { registerServiceWorker } from "./push";
import "./styles.css";

// Register the SW eagerly so push subscriptions can be created from the
// Settings modal without waiting for first-paint.
void registerServiceWorker();

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
