import { type Component, Show } from "solid-js";
import type { SessionTool } from "./routeModel";

interface Props {
  currentTool?: SessionTool | null;
  guiEnabled?: boolean;
  assistantEnabled?: boolean;
  chatEnabled?: boolean;
}

export const SessionTools: Component<Props> = (props) => (
    <nav class="sessions-tools" aria-label="Session tools">
      <span>Tools</span>
      <a href="#/sessions" aria-current={!props.currentTool ? "page" : undefined}>Overview</a>
      <a href="#/g" aria-current={props.currentTool === "git" ? "page" : undefined}>Git</a>
      <a href="#/oversight" aria-current={props.currentTool === "oversight" ? "page" : undefined}>Oversight</a>
      <a href="#/history" aria-current={props.currentTool === "history" ? "page" : undefined}>History</a>
      <a href="#/tasks" aria-current={props.currentTool === "tasks" ? "page" : undefined}>Tasks</a>
      <Show when={props.guiEnabled}>
        <a href="#/gui" aria-current={props.currentTool === "gui" ? "page" : undefined}>GUI stream</a>
      </Show>
      <Show when={props.chatEnabled}>
        <a href="#/chat" aria-current={props.currentTool === "chat" ? "page" : undefined}>Chat</a>
      </Show>
      <Show when={props.assistantEnabled}>
        <a href="#/assistant" aria-current={props.currentTool === "assistant" ? "page" : undefined}>Assistant</a>
      </Show>
    </nav>
);
