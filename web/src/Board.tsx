// Placeholder for the Board surface (M11). Replaced by the surface itself.
import type { Component } from "solid-js";

const Board: Component<{ onError?: (message: string) => void }> = () => (
  <div class="vogt-surface vogt-surface--placeholder">
    <p>Not built yet.</p>
  </div>
);

export default Board;
