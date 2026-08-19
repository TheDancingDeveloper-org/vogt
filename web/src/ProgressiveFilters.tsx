// The progressive filter grammar Board and Backlog both speak (FR-U11,
// FR-U14, RESTRUCTURE Stage 6–7).
//
// One interaction, described once: what is filtering the view right now is a
// row of removable chips; anything not yet filtered is added through a
// `+ Filter` disclosure; a combination worth keeping is a named lens.
//
// What the two surfaces do *not* share is their filter model — the Board's
// swimlanes and the Backlog's ranked view have nothing to say to each other —
// so this file owns the chrome and the disclosure's focus behaviour, and each
// surface passes its own fields as children and its own chips as data.
//
// Class names are prefixed per surface rather than generic: `styles.css`
// styles the shared `vogt-filter-*` names, and the prefixed name is what each
// surface's own layout rules and tests already reach for.

import { For, Show, createSignal, type JSX } from "solid-js";

/** One active filter, as the reader sees it: a sentence and a way to drop it. */
export interface FilterChip {
  /** What `onRemove` is told; the surface decides what clearing it means. */
  key: string;
  label: string;
}

interface FiltersProps {
  /** The surface's name, as it appears in the group labels. */
  surface: string;
  /** The class prefix this surface's stylesheet already uses. */
  prefix: string;
  chips: FilterChip[];
  onRemove: (key: string) => void;
  onClear: () => void;
  clearDisabled?: boolean;
  /** Controls rendered beside Done, for surface-specific shortcuts. */
  actions?: JSX.Element;
  /** The surface's own filter fields. */
  children: JSX.Element;
}

/**
 * The chip summary and the `+ Filter` disclosure over it.
 *
 * The disclosure owns focus: opening it does not move focus away from the
 * button that opened it, and closing it — by Done or by Escape — puts focus
 * back there rather than at the top of the document.
 */
export function ProgressiveFilters(props: FiltersProps): JSX.Element {
  const [open, setOpen] = createSignal(false);
  let addButton: HTMLButtonElement | undefined;

  const close = () => {
    setOpen(false);
    queueMicrotask(() => addButton?.focus());
  };

  const panelId = `${props.prefix}-filter-panel`;

  return (
    <div
      class={`vogt-filters ${props.prefix}-toolbar`}
      role="group"
      aria-label={`${props.surface} filters`}
    >
      <div class={`vogt-filter-summary ${props.prefix}-filter-summary`}>
        <span class={`vogt-filter-summary-label ${props.prefix}-filter-summary-label`}>
          Filters
        </span>
        <Show
          when={props.chips.length > 0}
          fallback={<span class={`${props.prefix}-muted`}>No filters applied</span>}
        >
          <div class={`vogt-filter-chips ${props.prefix}-filter-chips`}>
            <For each={props.chips}>
              {(chip) => (
                <span class={`vogt-filter-chip ${props.prefix}-filter-chip`}>
                  <span>{chip.label}</span>
                  <button
                    type="button"
                    aria-label={`Remove filter ${chip.label}`}
                    onClick={() => props.onRemove(chip.key)}
                  >
                    ×
                  </button>
                </span>
              )}
            </For>
          </div>
        </Show>
        <button
          type="button"
          class={`vogt-add-filter ${props.prefix}-add-filter`}
          ref={addButton}
          aria-controls={panelId}
          aria-expanded={open()}
          onClick={() => setOpen((was) => !was)}
        >
          + Filter
        </button>
        <button type="button" onClick={props.onClear} disabled={props.clearDisabled}>
          Clear all
        </button>
      </div>
      <div
        id={panelId}
        class={`vogt-filter-panel ${props.prefix}-filter-panel`}
        role="group"
        aria-label={`Add ${props.surface} filters`}
        hidden={!open()}
        onKeyDown={(event) => {
          if (event.key !== "Escape") return;
          // The surface above may close something of its own on Escape; the
          // open disclosure is the innermost thing the key applies to.
          event.stopPropagation();
          close();
        }}
      >
        <div class={`vogt-filter-panel-grid ${props.prefix}-filter-panel-grid`}>
          {props.children}
          <div class={`vogt-filter-actions ${props.prefix}-toolbar-actions`}>
            {props.actions}
            <button type="button" onClick={close}>
              Done
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

/** A saved combination, as the row draws it. */
export interface Lens {
  name: string;
  /** What the combination is, for the recall control's tooltip. */
  title: string;
}

interface LensProps {
  prefix: string;
  lenses: Lens[];
  onSave: (name: string) => void;
  onRecall: (name: string) => void;
  onForget: (name: string) => void;
  /** Where the lenses live, said out loud — they are per-client (§3). */
  note: JSX.Element;
}

/** Naming the current combination, and recalling a named one (FR-U14). */
export function SavedLenses(props: LensProps): JSX.Element {
  const [name, setName] = createSignal("");

  const save = () => {
    const wanted = name().trim();
    if (!wanted) return;
    props.onSave(wanted);
    setName("");
  };

  return (
    <div class={`vogt-lenses ${props.prefix}-savedrow`}>
      <span class={`vogt-lens-label ${props.prefix}-savedlabel`}>Saved lenses</span>
      <input
        type="text"
        class={`vogt-lens-name ${props.prefix}-savedname`}
        placeholder="Name this lens"
        aria-label="Lens name"
        value={name()}
        onInput={(event) => setName(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          save();
        }}
      />
      <button type="button" onClick={save} disabled={!name().trim()}>
        Save lens
      </button>
      <For each={props.lenses}>
        {(lens) => (
          <span class={`vogt-lens ${props.prefix}-saved`}>
            <button
              type="button"
              class={`vogt-lens-recall ${props.prefix}-saved-recall`}
              title={lens.title}
              onClick={() => props.onRecall(lens.name)}
            >
              {lens.name}
            </button>
            <button
              type="button"
              class={`vogt-lens-drop ${props.prefix}-saved-drop`}
              aria-label={`Forget the saved lens ${lens.name}`}
              onClick={() => props.onForget(lens.name)}
            >
              ×
            </button>
          </span>
        )}
      </For>
      <span class={`${props.prefix}-muted`}>{props.note}</span>
    </div>
  );
}
