import {
  type Component,
  type JSX,
  createUniqueId,
  onCleanup,
  onMount,
} from "solid-js";
import { Portal } from "solid-js/web";

const FOCUSABLE = [
  "button:not([disabled])",
  "[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

const dialogStack: HTMLElement[] = [];

function focusableElements(dialog: HTMLElement): HTMLElement[] {
  return [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((element) => {
    return !element.hidden && element.getAttribute("aria-hidden") !== "true";
  });
}

function topDialog(): HTMLElement | undefined {
  return dialogStack.at(-1);
}

export interface DialogProps {
  /** Accessible name rendered as the dialog's heading. */
  title?: string;
  /** Accessible name for layouts that render their own visible heading. */
  label?: string;
  /** Id of a caller-rendered visible heading. */
  labelledBy?: string;
  description?: string;
  describedBy?: string;
  children: JSX.Element;
  onClose: () => void;
  dialogClass?: string;
  backdropClass?: string;
  titleClass?: string;
  /** Backdrop dismissal is opt-in; dirty and destructive decisions stay put. */
  dismissOnBackdrop?: boolean;
  /** Escape is safe cancellation by default, but callers can explicitly disable it. */
  closeOnEscape?: boolean;
}

/**
 * Shared accessible modal foundation.
 *
 * Instances form a stack: only the top dialog contains focus and handles
 * Escape, so a nested editor can open over Settings without the parent
 * stealing focus. Closing restores the element that invoked that instance.
 */
const Dialog: Component<DialogProps> = (props) => {
  const generatedId = createUniqueId();
  const titleId = `dialog-title-${generatedId}`;
  const descriptionId = `dialog-description-${generatedId}`;
  let dialogRef: HTMLDivElement | undefined;
  let invoker: HTMLElement | null = null;
  // Existing top-level overlays stay in their caller's tree. A dialog opened
  // while another is active must leave that tree before the parent becomes
  // inert/aria-hidden, otherwise the active child disappears with it.
  const portalOverParent = topDialog() !== undefined;

  const focusInitial = () => {
    if (!dialogRef || topDialog() !== dialogRef) return;
    const preferred = dialogRef.querySelector<HTMLElement>(
      "[data-dialog-initial-focus], [autofocus]",
    );
    (preferred ?? focusableElements(dialogRef)[0] ?? dialogRef).focus();
  };

  onMount(() => {
    if (!props.title && !props.label && !props.labelledBy) {
      throw new Error("Dialog requires title, label, or labelledBy");
    }
    if (!dialogRef) return;
    invoker = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    const parentDialog = topDialog();
    if (parentDialog) {
      parentDialog.inert = true;
      parentDialog.setAttribute("aria-hidden", "true");
    }
    dialogStack.push(dialogRef);

    const onKeyDown = (event: KeyboardEvent) => {
      if (!dialogRef || topDialog() !== dialogRef) return;
      if (event.key === "Escape" && props.closeOnEscape !== false) {
        event.preventDefault();
        event.stopPropagation();
        props.onClose();
        return;
      }
      if (event.key !== "Tab") return;

      const focusable = focusableElements(dialogRef);
      if (focusable.length === 0) {
        event.preventDefault();
        dialogRef.focus();
        return;
      }
      const first = focusable[0]!;
      const last = focusable.at(-1)!;
      const active = document.activeElement;
      if (event.shiftKey && (active === first || !dialogRef.contains(active))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (active === last || !dialogRef.contains(active))) {
        event.preventDefault();
        first.focus();
      }
    };
    const onFocusIn = (event: FocusEvent) => {
      if (
        dialogRef &&
        topDialog() === dialogRef &&
        event.target instanceof Node &&
        !dialogRef.contains(event.target)
      ) {
        focusInitial();
      }
    };

    document.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("focusin", onFocusIn);
    queueMicrotask(focusInitial);

    onCleanup(() => {
      document.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("focusin", onFocusIn);
      const index = dialogRef ? dialogStack.lastIndexOf(dialogRef) : -1;
      if (index >= 0) dialogStack.splice(index, 1);
      const revealedParent = topDialog();
      if (revealedParent) {
        revealedParent.inert = false;
        revealedParent.removeAttribute("aria-hidden");
      }
      queueMicrotask(() => {
        if (invoker?.isConnected) {
          invoker.focus();
        } else {
          const parent = topDialog();
          (parent ? focusableElements(parent)[0] ?? parent : null)?.focus();
        }
      });
    });
  });

  const overlay = (
    <div
      class={props.backdropClass ?? "modal-backdrop"}
      onPointerDown={(event) => {
        if (
          props.dismissOnBackdrop === true &&
          event.target === event.currentTarget &&
          dialogRef &&
          topDialog() === dialogRef
        ) {
          props.onClose();
        }
      }}
    >
      <div
        ref={dialogRef}
        class={props.dialogClass ?? "modal"}
        role="dialog"
        aria-modal="true"
        aria-label={props.label}
        aria-labelledby={props.labelledBy ?? (props.title ? titleId : undefined)}
        aria-describedby={
          props.describedBy ?? (props.description ? descriptionId : undefined)
        }
        tabindex="-1"
        onPointerDown={(event) => event.stopPropagation()}
      >
        {props.title ? (
          <h2 id={titleId} class={props.titleClass}>
            {props.title}
          </h2>
        ) : null}
        {props.description ? <p id={descriptionId}>{props.description}</p> : null}
        {props.children}
      </div>
    </div>
  );

  return portalOverParent ? <Portal>{overlay}</Portal> : overlay;
};

export default Dialog;
