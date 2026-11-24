// The Android hardware/gesture back button inside the Capacitor shell.
//
// Without a listener, Capacitor's default for back is to leave the app when
// the WebView cannot go back — and it never asks the page whether a modal is
// open. On a phone that made the command palette and the new-session picker
// impossible to leave except by choosing something: the palette fills the
// screen so there is no backdrop to tap, and there is no Escape key. Back is
// the phone's Escape, so it is wired to the same place Escape goes: the
// top-most dialog, then any lighter menu that listens for Escape, then the
// router's history, and only when nothing else is left, the app itself.

export interface NativeBackDeps {
  dialogOpen: () => boolean;
  /** Dispatch a synthetic Escape at the document; returns whether it was consumed. */
  dispatchEscape: () => boolean;
  canGoBack: boolean;
  goBack: () => void;
  exitApp: () => void;
}

/** Pure decision for one back press; the Capacitor wiring is below. */
export function handleNativeBack(deps: NativeBackDeps): "dialog" | "escape" | "history" | "exit" {
  const hadDialog = deps.dialogOpen();
  const consumed = deps.dispatchEscape();
  if (hadDialog) return "dialog";
  if (consumed) return "escape";
  if (deps.canGoBack) {
    deps.goBack();
    return "history";
  }
  deps.exitApp();
  return "exit";
}

function dispatchEscapeAtDocument(): boolean {
  const target = document.activeElement instanceof HTMLElement
    ? document.activeElement
    : document.body;
  const event = new KeyboardEvent("keydown", {
    key: "Escape",
    code: "Escape",
    bubbles: true,
    cancelable: true,
  });
  target.dispatchEvent(event);
  return event.defaultPrevented;
}

/**
 * Register the Capacitor `backButton` listener. A no-op outside the native
 * shell: the plugin import resolves to its web stub, which never fires.
 */
export function installNativeBackButton(dialogOpen: () => boolean): void {
  if (typeof window === "undefined") return;
  void (async () => {
    try {
      const { App } = await import("@capacitor/app");
      await App.addListener("backButton", ({ canGoBack }) => {
        handleNativeBack({
          dialogOpen,
          dispatchEscape: dispatchEscapeAtDocument,
          canGoBack,
          goBack: () => window.history.back(),
          exitApp: () => void App.exitApp(),
        });
      });
    } catch {
      // Plain web/PWA: the browser owns its own back button.
    }
  })();
}
