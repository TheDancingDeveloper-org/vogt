import { render } from "solid-js/web";
import { HashRouter, Route } from "@solidjs/router";
import App from "./App";
import "./styles.css";

// HashRouter avoids needing an SPA fallback configured on the embedding
// Rust server: every navigation stays under index.html.
render(
  () => (
    <HashRouter>
      <Route path="/" component={App} />
      <Route path="/t/:id" component={App} />
      <Route path="/e/*path" component={App} />
    </HashRouter>
  ),
  document.getElementById("root")!,
);
