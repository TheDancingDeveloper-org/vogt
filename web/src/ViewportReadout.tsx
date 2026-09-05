import { createSignal, onCleanup, onMount, type Component } from "solid-js";
import { keyboardInsetFor } from "./viewportInsets";

interface Reading {
  inner: string;
  visual: string;
  keyboard: string;
  safe: string;
  native: string;
}

function read(): Reading {
  const vv = window.visualViewport;
  const css = getComputedStyle(document.documentElement);
  const data = document.documentElement.dataset;
  return {
    inner: `${window.innerWidth}×${window.innerHeight}`,
    visual: vv
      ? `${Math.round(vv.width)}×${Math.round(vv.height)} · scale ${vv.scale.toFixed(3)} · offset ${Math.round(vv.offsetLeft)},${Math.round(vv.offsetTop)}`
      : "unavailable",
    keyboard: `${keyboardInsetFor(window.innerHeight, vv)}px (var ${css.getPropertyValue("--keyboard-inset").trim() || "unset"})`,
    safe: `top ${css.getPropertyValue("--safe-top").trim() || "0"} · bottom ${css.getPropertyValue("--safe-bottom").trim() || "0"}`,
    native: `${data.nativeInsetTop ?? "–"} / ${data.nativeInsetRight ?? "–"} / ${data.nativeInsetBottom ?? "–"} / ${data.nativeInsetLeft ?? "–"}`,
  };
}

/**
 * Live viewport numbers for the phone (WI-79): what the shell measures is
 * what positions the bottom bar and the terminal dock, and a screenshot of
 * this panel is how a layout complaint from a device becomes a diagnosis.
 */
const ViewportReadout: Component = () => {
  const [reading, setReading] = createSignal<Reading>(read());
  onMount(() => {
    const refresh = () => setReading(read());
    const events = ["resize", "vogt:viewport-resize", "vogt:native-insets", "mydevenv2:native-insets"];
    for (const name of events) window.addEventListener(name, refresh);
    window.visualViewport?.addEventListener("resize", refresh);
    window.visualViewport?.addEventListener("scroll", refresh);
    onCleanup(() => {
      for (const name of events) window.removeEventListener(name, refresh);
      window.visualViewport?.removeEventListener("resize", refresh);
      window.visualViewport?.removeEventListener("scroll", refresh);
    });
  });
  const row = (label: string, value: string) => (
    <div style={{ display: "flex", gap: "8px", "font-size": "12px" }}>
      <span style={{ color: "var(--fg-muted)", "min-width": "104px" }}>{label}</span>
      <code style={{ "font-size": "12px" }}>{value}</code>
    </div>
  );
  return (
    <details class="viewport-readout">
      <summary style={{ "font-size": "12px", cursor: "pointer" }}>Viewport (phone layout diagnostics)</summary>
      <div style={{ display: "flex", "flex-direction": "column", gap: "4px", "margin-top": "6px" }}>
        {row("window", reading().inner)}
        {row("visual viewport", reading().visual)}
        {row("keyboard inset", reading().keyboard)}
        {row("safe insets", reading().safe)}
        {row("native insets", reading().native)}
      </div>
    </details>
  );
};

export default ViewportReadout;
