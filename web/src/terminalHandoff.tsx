import {
  type Component,
  type ComponentProps,
  type Owner,
  createEffect,
  createRoot,
  createSignal,
  getOwner,
  onCleanup,
  onMount,
} from "solid-js";
import Terminal, { type TerminalActions } from "./Terminal";

/**
 * Mobile pager hand-off (WI-2706).
 *
 * A swipe mounts the destination session's terminal in the pager slide while
 * the finger is down. When the swipe settles, the new tab mounts a pane for
 * that same session. Without a hand-off that is a second xterm and a second
 * attach for a terminal that is already on screen.
 *
 * Here a terminal lives in its own reactive root, keyed by session id, and
 * its DOM node is moved between slots instead of being remounted. A slide's
 * slot that settles `reserve`s its terminal; the next slot to mount for the
 * session adopts it — props, socket, screen and all. An unclaimed reservation
 * is disposed after `HANDOFF_TTL_MS`.
 */

export type HandoffTerminalProps = ComponentProps<typeof Terminal>;

export const HANDOFF_TTL_MS = 5_000;

interface Entry {
  sessionId: string;
  node: HTMLElement;
  setProps: (props: HandoffTerminalProps) => void;
  dispose: () => void;
  owner: symbol | null;
  reserved: boolean;
  focusOnAdopt: boolean;
  ttl: ReturnType<typeof setTimeout> | undefined;
  send: ((data: string | ArrayBuffer) => void) | null;
  actions: TerminalActions | null;
}

const entries = new Map<string, Entry>();

function createEntry(initial: HandoffTerminalProps, owner: Owner | null): Entry {
  return createRoot((dispose) => {
    const [props, setProps] = createSignal(initial);
    const entry = {
      sessionId: initial.sessionId,
      setProps,
      dispose,
      owner: null,
      reserved: false,
      focusOnAdopt: false,
      ttl: undefined,
      send: null,
      actions: null,
    } as unknown as Entry;
    entry.node = (
      <div class="terminal-handoff-host">
        <Terminal
          sessionId={initial.sessionId}
          parked={props().parked}
          preview={props().preview}
          workspaceActive={props().workspaceActive}
          interceptInput={(data) => props().interceptInput?.(data) ?? false}
          registerSend={(fn) => {
            entry.send = fn;
            props().registerSend?.(fn);
          }}
          registerActions={(actions) => {
            entry.actions = actions;
            props().registerActions?.(actions);
          }}
          onNotify={(message, kind) => props().onNotify?.(message, kind)}
          onRequestFind={() => props().onRequestFind?.()}
          onSearchResults={(info) => props().onSearchResults?.(info)}
          onTitle={(title) => props().onTitle?.(title)}
          onBell={() => props().onBell?.()}
        />
      </div>
    ) as HTMLElement;
    return entry;
  }, owner ?? undefined);
}

function disposeEntry(entry: Entry) {
  if (entry.ttl !== undefined) clearTimeout(entry.ttl);
  if (entries.get(entry.sessionId) === entry) entries.delete(entry.sessionId);
  entry.node.remove();
  entry.dispose();
}

/**
 * Keep the session's terminal alive past its slot's teardown, for the next
 * slot that mounts the same session. `focus` re-focuses its input on adoption
 * so an open soft keyboard stays open across the swipe.
 */
export function reserveTerminalHandoff(
  sessionId: string,
  options: { focus?: boolean } = {},
): boolean {
  const entry = entries.get(sessionId);
  if (!entry) return false;
  entry.reserved = true;
  entry.focusOnAdopt = Boolean(options.focus);
  if (entry.ttl !== undefined) clearTimeout(entry.ttl);
  entry.ttl = setTimeout(() => {
    if (entry.reserved) disposeEntry(entry);
  }, HANDOFF_TTL_MS);
  return true;
}

/** A reserved terminal is waiting for this session's next slot. */
export function hasTerminalHandoff(sessionId: string): boolean {
  return entries.get(sessionId)?.reserved === true;
}

/** The xterm input of the session's hand-off terminal, if it has one. */
export function terminalHandoffInput(
  sessionId: string,
): HTMLTextAreaElement | null {
  return entries.get(sessionId)?.node.querySelector("textarea") ?? null;
}

/** For tests: drop every hand-off terminal. */
export function resetTerminalHandoffs(): void {
  for (const entry of [...entries.values()]) disposeEntry(entry);
}

/**
 * A terminal slot whose terminal can outlive it. Mounting adopts the
 * session's reserved terminal if there is one, and otherwise starts a new one.
 */
export const HandoffTerminal: Component<HandoffTerminalProps> = (props) => {
  const token = Symbol(props.sessionId);
  const sessionId = props.sessionId;
  let slot: HTMLDivElement | undefined;
  const existing = entries.get(sessionId);
  let entry: Entry;
  let adopted = false;
  if (existing && (existing.reserved || existing.owner === null)) {
    entry = existing;
    adopted = existing.reserved;
    if (entry.ttl !== undefined) clearTimeout(entry.ttl);
    entry.ttl = undefined;
    entry.reserved = false;
    entry.setProps({ ...props });
  } else {
    if (existing) disposeEntry(existing);
    entry = createEntry({ ...props }, getOwner());
    entries.set(sessionId, entry);
  }
  entry.owner = token;

  createEffect(() => entry.setProps({ ...props }));

  onMount(() => {
    slot?.appendChild(entry.node);
    if (!adopted) return;
    // The terminal registered its sender and actions with the slot it was
    // mounted in; hand them to this one.
    if (entry.send) props.registerSend?.(entry.send);
    if (entry.actions) props.registerActions?.(entry.actions);
    if (entry.focusOnAdopt) {
      entry.focusOnAdopt = false;
      entry.node.querySelector("textarea")?.focus({ preventScroll: true });
    }
  });

  onCleanup(() => {
    if (entry.owner !== token) return;
    entry.owner = null;
    if (entry.reserved) {
      // Moving on to the next slot: detach, keep it alive.
      entry.node.remove();
      return;
    }
    disposeEntry(entry);
  });

  return <div ref={slot} class="terminal-handoff-slot" />;
};
