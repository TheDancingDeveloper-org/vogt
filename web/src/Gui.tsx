import { Component, createMemo, createResource, For, Show, createSignal } from "solid-js";
import { api } from "./api";

interface Props {
  streamUrl: string | null;
  onError?: (message: string) => void;
}

interface SavedGuiLauncher {
  id: string;
  label: string;
  command: string;
}

const GUI_LAUNCHERS_KEY = "mydevenv2.guiLaunchers";

function readSavedLaunchers(): SavedGuiLauncher[] {
  try {
    const raw = localStorage.getItem(GUI_LAUNCHERS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (entry): entry is SavedGuiLauncher =>
        !!entry &&
        typeof entry.id === "string" &&
        typeof entry.label === "string" &&
        typeof entry.command === "string",
    );
  } catch {
    return [];
  }
}

function writeSavedLaunchers(launchers: SavedGuiLauncher[]) {
  localStorage.setItem(GUI_LAUNCHERS_KEY, JSON.stringify(launchers));
}

function normalizeCommand(command: string): string {
  return command.trim().replace(/\s+/g, " ");
}

const GuiTab: Component<Props> = (props) => {
  const [procs, { refetch }] = createResource(() => api.guiProcesses());
  const [cmd, setCmd] = createSignal("chromium --no-sandbox --start-maximized");
  const [label, setLabel] = createSignal("Chromium");
  const [savedLaunchers, setSavedLaunchers] = createSignal(readSavedLaunchers());
  const [message, setMessage] = createSignal<string | null>(null);

  const launcherLabels = createMemo(() => {
    const map = new Map<string, string>();
    for (const launcher of savedLaunchers()) {
      map.set(normalizeCommand(launcher.command), launcher.label);
    }
    return map;
  });

  const saveLauncher = () => {
    const normalized = normalizeCommand(cmd());
    if (!normalized) return;
    const nextLabel = label().trim() || normalized.split(/\s+/)[0] || "Launcher";
    const next: SavedGuiLauncher = {
      id: crypto.randomUUID(),
      label: nextLabel,
      command: normalized,
    };
    const filtered = savedLaunchers().filter((launcher) => launcher.command !== normalized);
    const updated = [next, ...filtered];
    setSavedLaunchers(updated);
    writeSavedLaunchers(updated);
    setMessage(`Saved launcher "${nextLabel}"`);
  };

  const deleteLauncher = (id: string) => {
    const updated = savedLaunchers().filter((launcher) => launcher.id !== id);
    setSavedLaunchers(updated);
    writeSavedLaunchers(updated);
  };

  const launch = async () => {
    const tokens = normalizeCommand(cmd()).split(/\s+/).filter(Boolean);
    if (tokens.length === 0) return;
    try {
      await api.guiLaunch(tokens, true);
      setMessage(`Launched ${label().trim() || tokens[0]}`);
      void refetch();
    } catch (e) {
      const message = `GUI launch failed: ${(e as Error).message}`;
      setMessage(message);
      props.onError?.(message);
    }
  };

  return (
    <div class="gui-shell">
      <div class="gui-toolbar">
        <input
          type="text"
          value={label()}
          onInput={(e) => setLabel(e.currentTarget.value)}
          placeholder="Launcher label"
          class="gui-label-input"
        />
        <input
          type="text"
          value={cmd()}
          onInput={(e) => setCmd(e.currentTarget.value)}
          placeholder="GUI command (e.g. chromium http://localhost:4200)"
          style={{ flex: 1 }}
        />
        <span class={`gui-status-chip ${props.streamUrl ? "online" : "offline"}`}>
          {props.streamUrl ? "Stream configured" : "No stream"}
        </span>
        <button onClick={saveLauncher} title="Save launcher">
          Save
        </button>
        <button onClick={launch}>Launch</button>
        <button onClick={() => refetch()} title="Refresh process list">
          ⟳
        </button>
      </div>

      <Show when={message()}>
        <div class="gui-message meta">{message()}</div>
      </Show>

      <Show when={savedLaunchers().length > 0}>
        <div class="gui-launchers">
          <For each={savedLaunchers()}>
            {(launcher) => (
              <div class="gui-launcher">
                <button
                  class="gui-launcher-main"
                  onClick={() => {
                    setLabel(launcher.label);
                    setCmd(launcher.command);
                  }}
                  title={launcher.command}
                >
                  <span>{launcher.label}</span>
                  <span class="meta">{launcher.command}</span>
                </button>
                <button
                  onClick={() => {
                    setLabel(launcher.label);
                    setCmd(launcher.command);
                    void launch();
                  }}
                  title="Launch saved launcher"
                >
                  Run
                </button>
                <button onClick={() => deleteLauncher(launcher.id)} title="Delete saved launcher">
                  Remove
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show
        when={props.streamUrl}
        fallback={
          <div class="empty">
            <div>No GUI stream configured.</div>
            <div class="meta" style={{ "max-width": "40em", "text-align": "left" }}>
              Set <code>gui_stream_url</code> in the server config or the
              <code>GUI_STREAM_URL</code> env to a Selkies-GStreamer or
              KasmVNC URL. Once set, the embedded view appears here and any
              GUI you launch (above) renders into it.
            </div>
            <Show when={(procs() ?? []).length > 0}>
              <div class="meta" style={{ "margin-top": "8px" }}>
                {procs()!.length} running GUI process(es)
              </div>
            </Show>
          </div>
        }
      >
        <iframe
          class="gui-frame"
          src={props.streamUrl ?? ""}
          allow="autoplay; fullscreen; clipboard-read; clipboard-write"
          title="GUI stream"
        />
      </Show>

      <Show when={(procs() ?? []).length > 0}>
        <div class="gui-procs">
          <For each={procs() ?? []}>
            {(p) => (
              <div class="gui-proc">
                <div class="gui-proc-main">
                  <span class="gui-proc-label">
                    {launcherLabels().get(normalizeCommand(p.command.join(" "))) ??
                      p.command[0] ??
                      "Process"}
                  </span>
                  <span class="git-path">{p.command.join(" ")}</span>
                </div>
                <span class="meta">pid {p.pid}</span>
                <button
                  onClick={async () => {
                    await api.guiKill(p.pid);
                    void refetch();
                  }}
                >
                  Kill
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
};

export default GuiTab;
