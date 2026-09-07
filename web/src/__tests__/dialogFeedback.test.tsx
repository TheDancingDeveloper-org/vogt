import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import Dialog from "../Dialog";
import FeedbackCenter, { createFeedbackQueue } from "../FeedbackCenter";

afterEach(() => {
  vi.useRealTimers();
});

describe("Dialog", () => {
  it("names the modal, traps Tab, dismisses with Escape, and restores focus", async () => {
    let openDialog = () => {};
    const rendered = render(() => {
      const [open, setOpen] = createSignal(false);
      openDialog = () => setOpen(true);
      return (
        <>
          <button data-testid="invoker" onClick={() => setOpen(true)}>Open</button>
          {open() ? (
            <Dialog title="Delete artifact" onClose={() => setOpen(false)}>
              <button data-dialog-initial-focus>Cancel</button>
              <button>Delete</button>
            </Dialog>
          ) : null}
        </>
      );
    });
    const invoker = rendered.getByTestId("invoker");
    invoker.focus();
    openDialog();

    const dialog = await screen.findByRole("dialog", { name: "Delete artifact" });
    const cancel = screen.getByRole("button", { name: "Cancel" });
    const remove = screen.getByRole("button", { name: "Delete" });
    await waitFor(() => expect(cancel).toHaveFocus());
    remove.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(cancel).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    await waitFor(() => expect(invoker).toHaveFocus());
  });

  it("does not backdrop-dismiss a destructive decision", async () => {
    const close = vi.fn();
    render(() => (
      <Dialog title="Discard changes" onClose={close}>
        <button>Cancel</button>
      </Dialog>
    ));
    fireEvent.pointerDown(screen.getByRole("dialog").parentElement!);
    expect(close).not.toHaveBeenCalled();
  });

  it("keeps nested dialogs stacked and returns focus through both invokers", async () => {
    const rendered = render(() => {
      const [parentOpen, setParentOpen] = createSignal(false);
      const [childOpen, setChildOpen] = createSignal(false);
      return (
        <>
          <button onClick={() => setParentOpen(true)}>Settings</button>
          {parentOpen() ? (
            <Dialog title="Settings" onClose={() => setParentOpen(false)}>
              <button onClick={() => setChildOpen(true)}>Manage presets</button>
              {childOpen() ? (
                <Dialog title="Workspace presets" onClose={() => setChildOpen(false)}>
                  <button data-dialog-initial-focus>Done</button>
                </Dialog>
              ) : null}
            </Dialog>
          ) : null}
        </>
      );
    });

    const settingsInvoker = rendered.getByRole("button", { name: "Settings" });
    settingsInvoker.focus();
    fireEvent.click(settingsInvoker);
    const manage = await screen.findByRole("button", { name: "Manage presets" });
    await waitFor(() => expect(manage).toHaveFocus());
    fireEvent.click(manage);
    const parent = document.querySelector<HTMLElement>(
      '[role="dialog"][aria-hidden="true"]',
    )!;
    const child = screen.getByRole("dialog", { name: "Workspace presets" });
    expect(parent).toHaveAttribute("aria-hidden", "true");
    expect(parent).toHaveProperty("inert", true);
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(child).not.toBeInTheDocument());
    await waitFor(() => expect(manage).toHaveFocus());
    expect(parent).not.toHaveAttribute("aria-hidden");
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(settingsInvoker).toHaveFocus());
  });
});

describe("FeedbackCenter", () => {
  it("orders messages, exposes severity, coalesces duplicates, and persists errors", () => {
    vi.useFakeTimers();
    let queue!: ReturnType<typeof createFeedbackQueue>;
    const rendered = render(() => {
      queue = createFeedbackQueue();
      return <FeedbackCenter queue={queue} />;
    });

    queue.push("Saved", { kind: "success" });
    queue.push("Could not save", { kind: "error", details: "HTTP 503" });
    queue.push("Could not save", { kind: "error", details: "HTTP 503" });

    expect(rendered.getByRole("status")).toHaveTextContent("SuccessSaved");
    expect(rendered.getByRole("alert")).toHaveTextContent("ErrorCould not save ×2");
    expect(rendered.getByText("Open details")).toBeInTheDocument();
    vi.advanceTimersByTime(4000);
    expect(rendered.queryByRole("status")).not.toBeInTheDocument();
    expect(rendered.getByRole("alert")).toBeInTheDocument();
  });

  it("runs actions and allows manual dismissal", () => {
    const action = vi.fn();
    let queue!: ReturnType<typeof createFeedbackQueue>;
    const rendered = render(() => {
      queue = createFeedbackQueue();
      return <FeedbackCenter queue={queue} />;
    });
    queue.push("Connection lost", {
      kind: "error",
      actionLabel: "Retry",
      action,
    });
    fireEvent.click(rendered.getByRole("button", { name: "Retry" }));
    expect(action).toHaveBeenCalledOnce();
    fireEvent.click(rendered.getByRole("button", { name: "Dismiss Connection lost" }));
    expect(rendered.queryByRole("alert")).not.toBeInTheDocument();
  });
});
