// One wake signal for every mounted FileTree. The shell keeps a rail tree
// mounted while the editor can mount a second one; focus and visibility often
// arrive together after resume, so each instance must not install its own
// immediately-running listener (#415).

export const FILE_TREE_WAKE_DEBOUNCE_MS = 250;

type Registration = {
  active: () => boolean;
  refresh: () => void;
};

const registrations = new Set<Registration>();
let timer: number | undefined;
let installed = false;

function schedule(): void {
  if (document.visibilityState !== "visible") return;
  if (timer !== undefined) window.clearTimeout(timer);
  timer = window.setTimeout(() => {
    timer = undefined;
    for (const registration of registrations) {
      if (registration.active()) registration.refresh();
    }
  }, FILE_TREE_WAKE_DEBOUNCE_MS);
}

function onVisibility(): void {
  if (document.visibilityState === "visible") schedule();
}

/** Register one FileTree instance for coalesced wake reconciliation. */
export function registerFileTreeRevalidator(
  active: () => boolean,
  refresh: () => void,
): () => void {
  const registration = { active, refresh };
  registrations.add(registration);
  if (!installed) {
    installed = true;
    window.addEventListener("focus", schedule);
    document.addEventListener("visibilitychange", onVisibility);
  }
  return () => {
    registrations.delete(registration);
    if (registrations.size === 0 && installed) {
      installed = false;
      window.removeEventListener("focus", schedule);
      document.removeEventListener("visibilitychange", onVisibility);
      if (timer !== undefined) {
        window.clearTimeout(timer);
        timer = undefined;
      }
    }
  };
}
