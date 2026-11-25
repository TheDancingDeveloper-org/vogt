// The session's unique id in the terminal toolbar, with copy (WI-4683).
// Shows the Vogt `ses_…` id, falling back to the engine UUID for a session
// the core has no link for. Copies the full id on click.
import { createResource, createSignal, onCleanup } from "solid-js";
import { writeClipboardText } from "./clipboard";

/** Keep head and tail of a long id, eliding the middle. */
export function elideMiddle(id: string, max = 20): string {
  if (id.length <= max) return id;
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  return `${id.slice(0, head)}…${id.slice(id.length - (keep - head))}`;
}

async function copyWithFallback(text: string): Promise<void> {
  try {
    await writeClipboardText(text);
    return;
  } catch {
    // Insecure origin or no clipboard API: fall through to execCommand.
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try {
    if (!document.execCommand("copy")) throw new Error("copy refused");
  } finally {
    document.body.removeChild(ta);
  }
}

export default function SessionIdChip(props: {
  engineId: string;
  lookup: (engineId: string) => Promise<string | null>;
}) {
  const [linked] = createResource(
    () => props.engineId,
    (id) => props.lookup(id).catch(() => null),
  );
  const fullId = () => linked() ?? props.engineId;
  const [state, setState] = createSignal<"idle" | "copied" | "failed">("idle");
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));
  const copy = async () => {
    try {
      await copyWithFallback(fullId());
      setState("copied");
    } catch {
      setState("failed");
    }
    clearTimeout(timer);
    timer = setTimeout(() => setState("idle"), 1500);
  };
  return (
    <button
      type="button"
      class="session-id-chip"
      data-state={state()}
      onClick={copy}
      title={`${fullId()} (click to copy)`}
      aria-label={`Copy session id ${fullId()}`}
    >
      <span class="session-id-icon" aria-hidden="true">⧉</span>
      <span class="session-id-text">
        {state() === "copied" ? "Copied" : state() === "failed" ? "Copy failed" : elideMiddle(fullId())}
      </span>
    </button>
  );
}
