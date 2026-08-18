import { createSignal } from "solid-js";
import type { AssistantPendingAction } from "./api";

const [pendingAction, setPendingAction] = createSignal<AssistantPendingAction | null>(null);

export { pendingAction, setPendingAction };

export function clearPendingAction(): void {
  setPendingAction(null);
}
