import { Component, createResource, For, Show, createSignal } from "solid-js";
import { api } from "./api";

interface Props {
  streamUrl: string | null;
}

const GuiTab: Component<Props> = (props) => {
  const [procs, { refetch }] = createResource(() => api.guiProcesses());
  const [cmd, setCmd] = createSignal("chromium --no-sandbox --start-maximized");

  const launch = async () => {
    const tokens = cmd().trim().split(/\s+/);
    if (tokens.length === 0) return;
    try {
      await api.guiLaunch(tokens, true);
      void refetch();
    } catch (e) {
      alert((e as Error).message);
    }
  };

  return (
    <div class="gui-shell">
      <div class="gui-toolbar">
        <input
          type="text"
          value={cmd()}
          onInput={(e) => setCmd(e.currentTarget.value)}
          placeholder="GUI command (e.g. chromium http://localhost:4200)"
          style={{ flex: 1 }}
        />
        <button onClick={launch}>Launch</button>
        <button onClick={() => refetch()} title="Refresh process list">
          ⟳
        </button>
      </div>

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
                <span class="git-path">{p.command.join(" ")}</span>
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
