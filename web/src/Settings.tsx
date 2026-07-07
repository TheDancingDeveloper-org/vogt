import { Component, Show, For, createEffect, createSignal, onMount } from "solid-js";
import { clearStoredAuth, getBase, getToken, setBase, setToken } from "./api";
import { getLayoutMode, setLayoutMode, type LayoutMode } from "./layout";
import TemplateEditor from "./TemplateEditor";
import { THEMES, getThemeName, setThemeName } from "./terminalThemes";
import {
  listWorkspaceLayouts,
  workspaceLayoutSummary,
  type SavedWorkspaceLayout,
} from "./workspaceLayouts";
import {
  currentPushEnabled,
  isPushAvailable,
  pushPermission,
  pushSelfTest,
  subscribePushNotifications,
  unsubscribePushNotifications,
  type PushPermissionState,
} from "./push";

interface Props {
  open: boolean;
  onClose: () => void;
  onSaveWorkspaceLayout?: () => Promise<boolean | void>;
  onRestoreWorkspaceLayout?: (layoutId: string) => Promise<boolean | void>;
  onDeleteWorkspaceLayout?: (layoutId: string) => Promise<boolean | void>;
}

const Settings: Component<Props> = (props) => {
  const [token, setT] = createSignal(getToken());
  const [base, setB] = createSignal(getBase());
  const [layoutMode, setL] = createSignal<LayoutMode>(getLayoutMode());
  const [pushOn, setPushOn] = createSignal(false);
  const [pushPerm, setPushPerm] = createSignal<PushPermissionState>("default");
  const [pushBusy, setPushBusy] = createSignal(false);
  const [pushMsg, setPushMsg] = createSignal<string | null>(null);
  const [templateEditorOpen, setTemplateEditorOpen] = createSignal(false);
  const [terminalTheme, setTerminalTheme] = createSignal(getThemeName());
  const [workspaceLayouts, setWorkspaceLayouts] = createSignal<SavedWorkspaceLayout[]>(
    listWorkspaceLayouts(),
  );

  const refreshLayouts = () => {
    setWorkspaceLayouts(listWorkspaceLayouts());
  };

  const refreshPushState = async () => {
    setPushPerm(await pushPermission());
    setPushOn(await currentPushEnabled());
  };

  onMount(() => {
    if (isPushAvailable()) {
      void refreshPushState();
    }
  });

  createEffect(() => {
    if (!props.open) return;
    setT(getToken());
    setB(getBase());
    setL(getLayoutMode());
    setTerminalTheme(getThemeName());
    refreshLayouts();
    if (isPushAvailable()) {
      void refreshPushState();
    }
  });

  const formatDate = (value: string) => {
    try {
      return new Date(value).toLocaleString();
    } catch {
      return value;
    }
  };

  const save = () => {
    const newTok = token().trim();
    const newBase = base().trim();
    const newLayout = layoutMode();
    const tokChanged = newTok !== getToken();
    const baseChanged = newBase !== getBase();
    const layoutChanged = newLayout !== getLayoutMode();

    setToken(newTok);
    setBase(newBase);
    setLayoutMode(newLayout);

    props.onClose();

    // Reload if connection identity or layout changed
    if (tokChanged || baseChanged || layoutChanged) {
      location.reload();
    }
  };

  const clearAuth = () => {
    clearStoredAuth();
    props.onClose();
    location.reload();
  };

  const togglePush = async () => {
    setPushBusy(true);
    setPushMsg(null);
    try {
      if (pushOn()) {
        await unsubscribePushNotifications();
        setPushMsg("Notifications turned off.");
      } else {
        const label = navigator.userAgent.slice(0, 60);
        const r = await subscribePushNotifications(label);
        setPushMsg(`Subscribed (id ${r.id.slice(0, 12)}…)`);
      }
      await refreshPushState();
    } catch (e) {
      setPushMsg(`Push: ${(e as Error).message}`);
    } finally {
      setPushBusy(false);
    }
  };

  const testPush = async () => {
    setPushBusy(true);
    setPushMsg(null);
    try {
      const r = await pushSelfTest();
      setPushMsg(`Test dispatched: ${r.ok} ok / ${r.fail} fail`);
    } catch (e) {
      setPushMsg(`Test failed: ${(e as Error).message}`);
    } finally {
      setPushBusy(false);
    }
  };

  const saveWorkspaceLayout = async () => {
    const changed = await props.onSaveWorkspaceLayout?.();
    if (changed) refreshLayouts();
  };

  const restoreWorkspaceLayout = async (layoutId: string) => {
    await props.onRestoreWorkspaceLayout?.(layoutId);
    refreshLayouts();
  };

  const deleteWorkspaceLayout = async (layoutId: string) => {
    const changed = await props.onDeleteWorkspaceLayout?.(layoutId);
    if (changed) refreshLayouts();
  };

  return (
    <Show when={props.open}>
      <div class="modal-backdrop" onClick={props.onClose}>
        <div class="modal" onClick={(e) => e.stopPropagation()}>
          <h2>Settings</h2>
          <label>
            Bearer token (MYDEVENV2_TOKEN)
            <input
              type="password"
              value={token()}
              onInput={(e) => setT(e.currentTarget.value)}
              autocomplete="off"
              spellcheck={false}
            />
            <div
              style={{
                "font-size": "11px",
                color: "var(--fg-muted)",
                "margin-top": "3px",
              }}
            >
              Stored in this browser's localStorage. Anyone with access to this
              browser profile (or able to run JS in it) can read it.
            </div>
          </label>
          <label>
            Backend base URL (blank = same origin)
            <input
              type="text"
              value={base()}
              onInput={(e) => setB(e.currentTarget.value)}
              placeholder="https://mydevenv2.sprooty.com"
              autocomplete="off"
              spellcheck={false}
            />
          </label>
          <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" }}>
            <button type="button" onClick={clearAuth}>Clear saved auth</button>
          </div>

          <hr style={{ "border-color": "var(--bd)", "border-style": "solid", margin: "4px 0" }} />

          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Layout Mode
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Choose between tabbed mode (default) or IDE mode with persistent file tree.
            </div>
            <div style={{ display: "flex", gap: "8px" }}>
              <label style={{ display: "flex", "align-items": "center", gap: "6px", cursor: "pointer" }}>
                <input
                  type="radio"
                  name="layout"
                  value="tabbed"
                  checked={layoutMode() === "tabbed"}
                  onChange={() => setL("tabbed")}
                />
                <span style={{ "font-size": "13px" }}>Tabbed (default)</span>
              </label>
              <label style={{ display: "flex", "align-items": "center", gap: "6px", cursor: "pointer" }}>
                <input
                  type="radio"
                  name="layout"
                  value="ide"
                  checked={layoutMode() === "ide"}
                  onChange={() => setL("ide")}
                />
                <span style={{ "font-size": "13px" }}>IDE Mode</span>
              </label>
            </div>
          </div>

          <hr style={{ "border-color": "var(--bd)", "border-style": "solid", margin: "4px 0" }} />

          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Terminal Theme
            </div>
            <select
              value={terminalTheme()}
              onChange={(e) => {
                const name = e.currentTarget.value;
                setTerminalTheme(name);
                setThemeName(name);
              }}
              style={{
                padding: "6px 8px",
                background: "var(--bg)",
                border: "1px solid var(--bd)",
                "border-radius": "4px",
                color: "var(--fg)",
                "font-size": "13px",
              }}
            >
              <For each={Object.keys(THEMES)}>
                {(name) => <option value={name}>{name}</option>}
              </For>
            </select>
          </div>

          <hr style={{ "border-color": "var(--bd)", "border-style": "solid", margin: "4px 0" }} />

          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Workspace Presets
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Create custom presets with repo-aware defaults, commands, and environment variables.
            </div>
            <button onClick={() => setTemplateEditorOpen(true)}>
              Manage Presets
            </button>
          </div>

          <hr style={{ "border-color": "var(--bd)", "border-style": "solid", margin: "4px 0" }} />

          <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Saved Workspace Layouts
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Save named tab layouts in this browser profile. Terminal tabs come back only while their live sessions still exist.
            </div>
            <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
              <button onClick={() => void saveWorkspaceLayout()}>
                Save Current Layout
              </button>
            </div>
            <Show
              when={workspaceLayouts().length > 0}
              fallback={
                <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                  No saved layouts yet.
                </div>
              }
            >
              <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
                <For each={workspaceLayouts()}>
                  {(layout) => (
                    <div
                      style={{
                        display: "flex",
                        "justify-content": "space-between",
                        gap: "12px",
                        padding: "10px",
                        border: "1px solid var(--bd)",
                        "border-radius": "6px",
                        background: "rgba(255,255,255,0.02)",
                      }}
                    >
                      <div style={{ display: "flex", "flex-direction": "column", gap: "4px", "min-width": 0 }}>
                        <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
                          {layout.name}
                        </div>
                        <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                          {workspaceLayoutSummary(layout)}
                        </div>
                        <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>
                          Updated {formatDate(layout.updated_at)}
                        </div>
                      </div>
                      <div style={{ display: "flex", gap: "8px", "align-items": "center", "flex-wrap": "wrap" }}>
                        <button onClick={() => void restoreWorkspaceLayout(layout.id)}>
                          Restore
                        </button>
                        <button onClick={() => void deleteWorkspaceLayout(layout.id)}>
                          Delete
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </div>

          <hr style={{ "border-color": "var(--bd)", "border-style": "solid", margin: "4px 0" }} />

          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Push notifications
            </div>
            <Show
              when={isPushAvailable()}
              fallback={
                <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                  Not supported in this browser.
                </div>
              }
            >
              <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                Notify me when a session is waiting for input
                (permission: <code>{pushPerm()}</code>).
              </div>
              <div style={{ display: "flex", gap: "8px" }}>
                <button onClick={togglePush} disabled={pushBusy()}>
                  {pushOn() ? "Disable" : "Enable"} push
                </button>
                <button
                  onClick={testPush}
                  disabled={pushBusy() || !pushOn()}
                  title="Server fans out a test notification to all subscriptions"
                >
                  Send test
                </button>
              </div>
              <Show when={pushMsg()}>
                <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{pushMsg()}</div>
              </Show>
            </Show>
          </div>

          <div class="modal-actions">
            <button onClick={props.onClose}>Cancel</button>
            <button onClick={save}>Save & reload</button>
          </div>
        </div>
      </div>
      <TemplateEditor
        open={templateEditorOpen()}
        onClose={() => setTemplateEditorOpen(false)}
      />
    </Show>
  );
};

export default Settings;
