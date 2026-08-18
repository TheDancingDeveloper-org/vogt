import { Show, type Component, type JSX } from "solid-js";

/**
 * The one DOM grammar for a primary product surface's working header.
 *
 * Slots stay in reading order even when flex wrapping changes their visual
 * line. Honesty is deliberately content-free here: Board can report view
 * age, Inbox can report source coverage, and Sessions can report connection
 * state without a shared component pretending those are the same fact.
 */
export interface SurfaceHeaderProps {
  title: JSX.Element;
  honesty?: JSX.Element;
  controls?: JSX.Element;
  action?: JSX.Element;
  detail?: JSX.Element;
  class?: string;
  label?: string;
}

export const SurfaceHeader: Component<SurfaceHeaderProps> = (props) => (
  <header
    class={`surface-header${props.class ? ` ${props.class}` : ""}`}
    aria-label={props.label}
    data-surface-header
  >
    <div class="surface-header-title" data-surface-header-slot="title">
      {props.title}
    </div>
    <Show when={props.honesty !== undefined}>
      <div class="surface-header-honesty" data-surface-header-slot="honesty">
        {props.honesty}
      </div>
    </Show>
    <span
      class="surface-header-spacer"
      data-surface-header-slot="spacer"
      aria-hidden="true"
    />
    <Show when={props.controls !== undefined}>
      <div class="surface-header-controls" data-surface-header-slot="controls">
        {props.controls}
      </div>
    </Show>
    <Show when={props.action !== undefined}>
      <div class="surface-header-action" data-surface-header-slot="action">
        {props.action}
      </div>
    </Show>
    <Show when={props.detail !== undefined}>
      <div class="surface-header-detail" data-surface-header-slot="detail">
        {props.detail}
      </div>
    </Show>
  </header>
);

export default SurfaceHeader;
