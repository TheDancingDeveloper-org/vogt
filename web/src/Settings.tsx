import { Component, Show, For, createEffect, createSignal, onMount } from "solid-js";
import { api, clearStoredAuth, getBase, getToken, setBase, setToken } from "./api";
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
import type { OperationalStatus } from "./api";
import {
  deleteAuthProfile,
  listAuthProfiles,
  saveAuthProfile,
  type AuthProfile,
} from "./authProfiles";

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
  const [authProfiles, setAuthProfiles] = createSignal<AuthProfile[]>(listAuthProfiles());
  const [profileName, setProfileName] = createSignal("");
  const [profileMsg, setProfileMsg] = createSignal<string | null>(null);
  const [opsStatus, setOpsStatus] = createSignal<OperationalStatus | null>(null);
  const [opsError, setOpsError] = createSignal<string | null>(null);
  const [browserStorage, setBrowserStorage] = createSignal<{
    localStorageBytes: number;
    localStorageEntries: number;
    estimateUsage?: number;
    estimateQuota?: number;
  } | null>(null);

  const refreshLayouts = () => {
    setWorkspaceLayouts(listWorkspaceLayouts());
  };

  const refreshAuthProfiles = () => {
    setAuthProfiles(listAuthProfiles());
  };

  const refreshPushState = async () => {
    setPushPerm(await pushPermission());
    setPushOn(await currentPushEnabled());
  };

  const refreshOperationalState = async () => {
    try {
      setOpsError(null);
      setOpsStatus(await api.operationalStatus());
    } catch (e) {
      setOpsStatus(null);
      setOpsError((e as Error).message);
    }
  };

  const refreshBrowserStorage = async () => {
    let localStorageBytes = 0;
    let localStorageEntries = 0;
    try {
      for (let i = 0; i < localStorage.length; i += 1) {
        const key = localStorage.key(i);
        if (!key) continue;
        const value = localStorage.getItem(key) ?? "";
        localStorageEntries += 1;
        localStorageBytes += key.length + value.length;
      }
    } catch {
      /* localStorage unavailable */
    }

    let estimateUsage: number | undefined;
    let estimateQuota: number | undefined;
    try {
      const estimate = await navigator.storage?.estimate?.();
      estimateUsage = estimate?.usage;
      estimateQuota = estimate?.quota;
    } catch {
      /* storage estimate unavailable */
    }

    setBrowserStorage({
      localStorageBytes,
      localStorageEntries,
      estimateUsage,
      estimateQuota,
    });
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
    refreshAuthProfiles();
    setProfileName("");
    setProfileMsg(null);
    if (isPushAvailable()) {
      void refreshPushState();
    }
    void refreshOperationalState();
    void refreshBrowserStorage();
  });

  const formatDate = (value: string) => {
    try {
      return new Date(value).toLocaleString();
    } catch {
      return value;
    }
  };

  const formatBytes = (value?: number | null) => {
    if (value == null || Number.isNaN(value)) return "unknown";
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
    if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
    return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
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

  const storeCurrentProfile = () => {
    const name = profileName().trim();
    const tok = token().trim();
    if (!name) {
      setProfileMsg("Choose a profile name first.");
      return;
    }
    if (!tok) {
      setProfileMsg("Token is required to save a profile.");
      return;
    }
    saveAuthProfile({
      name,
      token: tok,
      base: base().trim(),
    });
    refreshAuthProfiles();
    setProfileMsg(`Saved profile "${name}"`);
  };

  const loadProfile = (profile: AuthProfile) => {
    setProfileName(profile.name);
    setT(profile.token);
    setB(profile.base);
    setProfileMsg(`Loaded profile "${profile.name}" into the form.`);
  };

  const removeProfile = (id: string, name: string) => {
    deleteAuthProfile(id);
    refreshAuthProfiles();
    setProfileMsg(`Deleted profile "${name}"`);
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
          <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Device-local profiles
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Save named token/base combinations in this browser profile for quicker switching.
            </div>
            <div style={{ display: "flex", gap: "8px", "align-items": "center" }}>
              <input
                type="text"
                value={profileName()}
                onInput={(e) => setProfileName(e.currentTarget.value)}
                placeholder="Profile name"
                autocomplete="off"
                spellcheck={false}
              />
              <button type="button" onClick={storeCurrentProfile}>Save profile</button>
            </div>
            <Show when={profileMsg()}>
              <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{profileMsg()}</div>
            </Show>
            <Show
              when={authProfiles().length > 0}
              fallback={
                <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                  No saved auth profiles yet.
                </div>
              }
            >
              <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
                <For each={authProfiles()}>
                  {(profile) => (
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
                          {profile.name}
                        </div>
                        <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                          {profile.base || "Same-origin backend"}
                        </div>
                        <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>
                          Updated {formatDate(profile.updated_at)}
                        </div>
                      </div>
                      <div style={{ display: "flex", gap: "8px", "align-items": "center", "flex-wrap": "wrap" }}>
                        <button type="button" onClick={() => loadProfile(profile)}>
                          Load
                        </button>
                        <button type="button" onClick={() => removeProfile(profile.id, profile.name)}>
                          Delete
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </div>
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

          <hr style={{ "border-color": "var(--bd)", "border-style": "solid", margin: "4px 0" }} />

          <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Operational visibility
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Lightweight runtime and browser state for sessions, push, GUI, auth broker, and storage.
            </div>
            <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
              <button onClick={() => void refreshOperationalState()}>Refresh runtime</button>
              <button onClick={() => void refreshBrowserStorage()}>Refresh storage</button>
            </div>
            <Show when={opsError()}>
              <div style={{ "font-size": "11px", color: "#ff7b72" }}>{opsError()}</div>
            </Show>
            <Show when={opsStatus()}>
              {(status) => (
                <div
                  style={{
                    display: "grid",
                    "grid-template-columns": "repeat(auto-fit, minmax(180px, 1fr))",
                    gap: "8px",
                  }}
                >
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>Sessions</div>
                    <div style={opsValueStyle}>{status().session_count}</div>
                    <div style={opsMetaStyle}>
                      Archived {status().history.archived_session_count ?? 0}
                    </div>
                  </div>
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>Push</div>
                    <div style={opsValueStyle}>{status().push_subscription_count}</div>
                    <div style={opsMetaStyle}>
                      {status().fcm_enabled ? "FCM enabled" : "FCM disabled"}
                    </div>
                  </div>
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>GUI</div>
                    <div style={opsValueStyle}>{status().gui_process_count}</div>
                    <div style={opsMetaStyle}>
                      {status().gui_stream_configured ? "Stream configured" : "No stream configured"}
                    </div>
                  </div>
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>Auth broker</div>
                    <div style={opsValueStyle}>
                      {status().auth_broker.auto_agent_auth ? "Auto" : "Manual"}
                    </div>
                    <div style={opsMetaStyle}>{status().auth_broker.helper}</div>
                  </div>
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>Server storage</div>
                    <div style={opsValueStyle}>{status().history.enabled ? "History on" : "History off"}</div>
                    <div style={opsMetaStyle}>
                      {status().storage.state_dir}
                    </div>
                  </div>
                  <Show when={browserStorage()}>
                    {(storage) => (
                      <div style={opsCardStyle}>
                        <div style={opsLabelStyle}>Browser storage</div>
                        <div style={opsValueStyle}>{formatBytes(storage().localStorageBytes)}</div>
                        <div style={opsMetaStyle}>
                          {storage().localStorageEntries} entries
                          <Show when={storage().estimateQuota != null}>
                            <span>
                              {" "}
                              • {formatBytes(storage().estimateUsage)} / {formatBytes(storage().estimateQuota)}
                            </span>
                          </Show>
                        </div>
                      </div>
                    )}
                  </Show>
                </div>
              )}
            </Show>
            <Show when={opsStatus()}>
              {(status) => (
                <div style={{ "font-size": "11px", color: "var(--fg-muted)", "line-height": 1.5 }}>
                  <div>Workspace root: <code>{status().storage.workspace_root}</code></div>
                  <div>Version: <code>{status().version}</code></div>
                </div>
              )}
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

const opsCardStyle = {
  display: "flex",
  "flex-direction": "column",
  gap: "4px",
  padding: "10px",
  border: "1px solid var(--bd)",
  "border-radius": "6px",
  background: "rgba(255,255,255,0.02)",
} as const;

const opsLabelStyle = {
  "font-size": "11px",
  color: "var(--fg-muted)",
  "text-transform": "uppercase",
} as const;

const opsValueStyle = {
  "font-size": "16px",
  color: "var(--fg)",
  "font-weight": 600,
} as const;

const opsMetaStyle = {
  "font-size": "11px",
  color: "var(--fg-muted)",
  "line-height": 1.4,
  "word-break": "break-word",
} as const;

export default Settings;
