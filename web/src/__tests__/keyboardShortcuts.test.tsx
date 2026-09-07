import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";

import KeyboardShortcuts from "../KeyboardShortcuts";
import { KEYBOARD_SHORTCUTS, matchAppShortcut } from "../keyboardShortcuts";

function matchedFrom(target: HTMLElement, init: KeyboardEventInit) {
  let matched: ReturnType<typeof matchAppShortcut>;
  const listener = (event: KeyboardEvent) => {
    matched = matchAppShortcut(event);
  };
  target.addEventListener("keydown", listener, { once: true });
  fireEvent.keyDown(target, init);
  return matched!;
}

describe("the declared shortcut registry", () => {
  it("drives help content and states each binding's context", () => {
    const rendered = render(() => (
      <KeyboardShortcuts open={true} onClose={() => undefined} />
    ));

    const help = KEYBOARD_SHORTCUTS.find((shortcut) => shortcut.id === "show-shortcut-help")!;
    expect(rendered.getByText(help.description)).toBeVisible();
    expect(rendered.getByText(help.contextLabel)).toBeVisible();
    expect(rendered.getAllByText("Terminal only", { exact: true }).length).toBeGreaterThan(0);
    expect(rendered.getAllByText("Editor only", { exact: true }).length).toBeGreaterThan(0);
  });

  it("matches question mark outside editable surfaces", () => {
    const button = document.createElement("button");
    document.body.append(button);

    expect(matchedFrom(button, { key: "?", shiftKey: true })?.id).toBe(
      "show-shortcut-help",
    );
    button.remove();
  });

  it.each([
    ["input", document.createElement("input")],
    ["textarea", document.createElement("textarea")],
    ["contenteditable", (() => {
      const element = document.createElement("div");
      element.setAttribute("contenteditable", "true");
      return element;
    })()],
    ["editor", Object.assign(document.createElement("div"), { className: "monaco-editor" })],
    ["terminal", Object.assign(document.createElement("div"), { className: "xterm" })],
  ])("does not steal question mark from %s", (_name, editable) => {
    document.body.append(editable);
    expect(matchedFrom(editable, { key: "?", shiftKey: true })).toBeUndefined();
    editable.remove();
  });

  it("still matches the declared global palette binding from an input", () => {
    const input = document.createElement("input");
    document.body.append(input);
    expect(matchedFrom(input, { key: "k", ctrlKey: true })?.id).toBe(
      "open-command-palette",
    );
    input.remove();
  });
});
