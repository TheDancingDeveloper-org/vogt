/** Route truth shared by rendering and every navigation surface. */
import { productDocumentTitle } from "./identity";
export type PrimaryPlace =
  | "sessions"
  | "inbox"
  | "board"
  | "backlog"
  | "projects"
  | "audit";

export type SessionTool =
  | "terminal"
  | "editor"
  | "git"
  | "history"
  | "tasks"
  | "gui"
  | "assistant";

export type RouteOutcome =
  | { kind: "place"; place: PrimaryPlace }
  | { kind: "tool"; place: "sessions"; tool: SessionTool }
  | { kind: "work-item"; place: "board" }
  | { kind: "settings"; place: PrimaryPlace; tool?: SessionTool }
  | {
      kind: "loading" | "unavailable" | "not-found";
      place: "sessions";
      title: string;
      message: string;
    };

export interface RouteCapabilities {
  configReady: boolean;
  sessionsState: "loading" | "ready" | "unavailable";
  sessionExists: (id: string) => boolean;
  assistantEnabled: boolean;
  guiAvailable: boolean;
}

const PLACE_PATHS: Record<string, PrimaryPlace> = {
  "/sessions": "sessions",
  "/inbox": "inbox",
  "/board": "board",
  "/backlog": "backlog",
  "/projects": "projects",
  "/audit": "audit",
};

export function describeRoute(
  pathname: string,
  capabilities: RouteCapabilities,
  settingsReturnPath = "/sessions",
): RouteOutcome | null {
  const place = PLACE_PATHS[pathname];
  if (place) return { kind: "place", place };
  if (pathname === "/settings") {
    const returnPathname = settingsReturnRoute(settingsReturnPath).split("?", 1)[0]
      ?? "/sessions";
    const returned = describeRoute(returnPathname, capabilities);
    return {
      kind: "settings",
      place: returned?.place ?? "sessions",
      tool: returned?.kind === "tool" ? returned.tool : undefined,
    };
  }
  if (pathname.startsWith("/w/")) return { kind: "work-item", place: "board" };
  if (pathname.startsWith("/e/")) {
    return { kind: "tool", place: "sessions", tool: "editor" };
  }
  if (pathname === "/g" || pathname.startsWith("/g/")) {
    return { kind: "tool", place: "sessions", tool: "git" };
  }
  if (pathname === "/history") {
    return { kind: "tool", place: "sessions", tool: "history" };
  }
  if (pathname === "/tasks") {
    return { kind: "tool", place: "sessions", tool: "tasks" };
  }
  if (pathname.startsWith("/t/")) {
    const id = decodeURIComponent(pathname.slice(3));
    if (capabilities.sessionsState === "loading") {
      return {
        kind: "loading",
        place: "sessions",
        title: "Loading session",
        message: "Checking the live session list before attaching this terminal.",
      };
    }
    if (capabilities.sessionsState === "unavailable") {
      return {
        kind: "unavailable",
        place: "sessions",
        title: "Sessions are unavailable",
        message: "The engine could not confirm whether this terminal still exists.",
      };
    }
    if (!capabilities.sessionExists(id)) {
      return {
        kind: "not-found",
        place: "sessions",
        title: "Session not found",
        message: `The terminal session “${id}” is no longer available.`,
      };
    }
    return { kind: "tool", place: "sessions", tool: "terminal" };
  }
  if (pathname === "/assistant" || pathname.startsWith("/assistant/")) {
    if (!capabilities.configReady) {
      return {
        kind: "loading",
        place: "sessions",
        title: "Loading Assistant",
        message: "Checking whether this deployment provides an Assistant.",
      };
    }
    return capabilities.assistantEnabled
      ? { kind: "tool", place: "sessions", tool: "assistant" }
      : {
          kind: "unavailable",
          place: "sessions",
          title: "Assistant is unavailable",
          message: "This deployment has no Assistant provider configured.",
        };
  }
  if (pathname === "/gui") {
    if (!capabilities.configReady) {
      return {
        kind: "loading",
        place: "sessions",
        title: "Loading GUI stream",
        message: "Checking whether this deployment provides a verified stream.",
      };
    }
    return capabilities.guiAvailable
      ? { kind: "tool", place: "sessions", tool: "gui" }
      : {
          kind: "unavailable",
          place: "sessions",
          title: "GUI stream is unavailable",
          message:
            "GUI streaming is hidden because the server has not made a configured stream available.",
        };
  }
  return null;
}

export function isCurrentPlace(outcome: RouteOutcome | null, place: PrimaryPlace): boolean {
  return outcome?.place === place;
}

export function isCurrentTool(outcome: RouteOutcome | null, tool: SessionTool): boolean {
  return (outcome?.kind === "tool" || outcome?.kind === "settings")
    && outcome.tool === tool;
}

const PLACE_TITLES: Record<PrimaryPlace, string> = {
  sessions: "Sessions",
  inbox: "Inbox",
  board: "Board",
  backlog: "Backlog",
  projects: "Projects",
  audit: "Audit",
};

const TOOL_TITLES: Record<SessionTool, string> = {
  terminal: "Terminal",
  editor: "Editor",
  git: "Git",
  history: "History",
  tasks: "Tasks",
  gui: "GUI stream",
  assistant: "Assistant",
};

/**
 * Name the current document from the same interpreted route that renders it.
 * This is deliberately a switch over RouteOutcome, not another pathname table:
 * a route cannot acquire a title without first becoming part of route truth.
 */
export function documentTitleForRoute(
  outcome: RouteOutcome | null,
  pathname: string,
): string {
  if (!outcome) return productDocumentTitle("Not found");
  switch (outcome.kind) {
    case "place":
      return productDocumentTitle(PLACE_TITLES[outcome.place]);
    case "tool":
      return productDocumentTitle(TOOL_TITLES[outcome.tool]);
    case "work-item": {
      const encoded = pathname.startsWith("/w/") ? pathname.slice(3) : "";
      let ref = encoded;
      try {
        ref = decodeURIComponent(encoded);
      } catch {
        // The router will present its own not-found result for an invalid URL;
        // a title must never make that failure fatal.
      }
      return productDocumentTitle(ref || "Work item");
    }
    case "settings":
      return productDocumentTitle("Settings");
    case "loading":
    case "unavailable":
    case "not-found":
      return productDocumentTitle(outcome.title);
  }
}

export function isRestorableRoute(path: string): boolean {
  const pathname = path.split("?", 1)[0] ?? path;
  return Boolean(
    PLACE_PATHS[pathname]
      || pathname.startsWith("/w/")
      || pathname.startsWith("/t/")
      || pathname.startsWith("/e/")
      || pathname === "/g"
      || pathname.startsWith("/g/")
      || pathname === "/history"
      || pathname === "/tasks"
      || pathname === "/gui"
      || pathname === "/assistant"
      || pathname.startsWith("/assistant/"),
  );
}

export function settingsReturnRoute(
  candidate: string,
  previous = "/sessions",
): string {
  return isRestorableRoute(candidate) ? candidate : previous;
}
