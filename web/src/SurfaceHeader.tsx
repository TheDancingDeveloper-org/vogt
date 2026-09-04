import { Show, createSignal, createUniqueId, type Component, type JSX } from "solid-js";
import GoToButton from "./GoToButton";
import { createNarrow } from "./narrow";
import { useShellActions } from "./shellActions";

/**
 * The one DOM grammar for a primary product surface's working header.
 *
 * Slots stay in reading order even when flex wrapping changes their visual
 * line. Honesty is deliberately content-free here: Board can report view
 * age, Inbox can report source coverage, and Sessions can report connection
 * state without a shared component pretending those are the same fact.
 *
 * On a phone the header can say all of that without saying it all at once. A
 * surface whose controls are chrome — a refresh cadence, a Refresh now — can
 * ask for them to sit behind one disclosure, so the first screen belongs to
 * the work rather than to the controls over it. That is `collapseControls`,
 * and it is opt-in for a reason: the Inbox's source pills and the Sessions
 * tool links live in the same slot and are not chrome, they are the surface.
 * Nothing is ever removed, the slot order never changes, and on a desk
 * everything is open as before.
 */
export interface SurfaceHeaderProps {
  title: JSX.Element;
  honesty?: JSX.Element;
  /**
   * Extra class on the honesty slot's own wrapper — `surface-header-honesty--
   * fresh|partial|stale|outage|never` (rail-spec.md A1). The slot stays
   * content-free about what tone means; this is the one thing about it the
   * shared component knows.
   */
  honestyClass?: string;
  controls?: JSX.Element;
  action?: JSX.Element;
  detail?: JSX.Element;
  /**
   * Whether a narrow client may fold `controls` and `detail` away behind a
   * disclosure. Only true where those slots hold chrome rather than the
   * surface's own navigation or filtering.
   */
  collapseControls?: boolean;
  /**
   * Whether a narrow client may fold `honesty` away behind the same
   * disclosure. Opt-in for the same reason `collapseControls` is: a status
   * line worth reading is not chrome. The Sessions surface asks for it only
   * once a terminal is open, where the connection line is a sentence a reader
   * already had on the way in and the whole screen belongs to the terminal.
   */
  collapseHonesty?: boolean;
  class?: string;
  label?: string;
}

export const SurfaceHeader: Component<SurfaceHeaderProps> = (props) => {
  const narrow = createNarrow();
  const shell = useShellActions();
  const [open, setOpen] = createSignal(false);
  const controlsId = createUniqueId();

  /** Something to disclose, a surface willing to, and a narrow client. The
   *  controls/detail fold and the honesty fold are independent — Sessions asks
   *  for the second without the first — but they share one disclosure. */
  const controlsCollapsible = () =>
    narrow() &&
    props.collapseControls === true &&
    (props.controls !== undefined || props.detail !== undefined);
  const honestyCollapsible = () =>
    narrow() && props.collapseHonesty === true && props.honesty !== undefined;
  const collapsible = () => controlsCollapsible() || honestyCollapsible();
  const controlsShown = () => !controlsCollapsible() || open();
  const honestyShown = () => !honestyCollapsible() || open();

  return (
    <header
      class={`surface-header${props.class ? ` ${props.class}` : ""}`}
      aria-label={props.label}
      data-surface-header
    >
      <div class="surface-header-title" data-surface-header-slot="title">
        {props.title}
      </div>
      {/* A phone's "Go to…" rides in the title row beside the surface's own
          action, so it costs no row of its own above every screen. The slot
          exists only on a narrow client inside the shell; a desk has the
          rail's button and this renders nothing. */}
      <Show when={narrow() && shell !== undefined}>
        <div class="surface-header-goto" data-surface-header-slot="goto">
          <GoToButton />
        </div>
      </Show>
      <Show when={props.honesty !== undefined}>
        <div
          class={`surface-header-honesty${props.honestyClass ? ` ${props.honestyClass}` : ""}`}
          data-surface-header-slot="honesty"
          hidden={!honestyShown()}
        >
          {props.honesty}
        </div>
      </Show>
      <span
        class="surface-header-spacer"
        data-surface-header-slot="spacer"
        aria-hidden="true"
      />
      <Show when={collapsible()}>
        <button
          type="button"
          class="surface-header-more"
          aria-expanded={open()}
          aria-controls={controlsId}
          onClick={() => setOpen((was) => !was)}
        >
          {open() ? "Fewer controls" : "View controls"}
        </button>
      </Show>
      <Show when={props.controls !== undefined}>
        <div
          id={controlsId}
          class="surface-header-controls"
          data-surface-header-slot="controls"
          hidden={!controlsShown()}
        >
          {props.controls}
        </div>
      </Show>
      <Show when={props.action !== undefined}>
        <div class="surface-header-action" data-surface-header-slot="action">
          {props.action}
        </div>
      </Show>
      <Show when={props.detail !== undefined}>
        <div
          class="surface-header-detail"
          data-surface-header-slot="detail"
          hidden={!controlsShown()}
        >
          {props.detail}
        </div>
      </Show>
    </header>
  );
};

export default SurfaceHeader;
