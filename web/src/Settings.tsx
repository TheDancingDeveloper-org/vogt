import { Component, Show, For, createEffect, createSignal, onMount } from "solid-js";
import {
  api,
  clearStoredAuth,
  getBase,
  getToken,
  setBase,
  setToken,
  type OperationalStatus,
  type PushPreferences,
  type PushSubscriptionEntry,
} from "./api";
import { getLayoutMode, setLayoutMode, type LayoutMode } from "./layout";
import TemplateEditor from "./TemplateEditor";
import { THEMES, getThemeName, setThemeName } from "./terminalThemes";
import {
  clearWorkspaceLayouts,
  listWorkspaceLayouts,
  trimWorkspaceLayouts,
  workspaceLayoutSummary,
  type SavedWorkspaceLayout,
} from "./workspaceLayouts";
import {
  currentPushEnabled,
  currentPushSubscriptionId,
  isPushAvailable,
  pushPermission,
  pushSelfTest,
  subscribePushNotifications,
  unsubscribePushNotifications,
  type PushPermissionState,
} from "./push";
import {
  clearAuthProfiles,
  deleteAuthProfile,
  listAuthProfiles,
  saveAuthProfile,
  trimAuthProfiles,
  type AuthProfile,
} from "./authProfiles";
import { clearBookmarks, bookmarks, trimBookmarks } from "./bookmarks";
import {
  clearHistoryPins,
  getPinnedHistoryIds,
  trimHistoryPins,
} from "./historyPins";
import { clearRecentFiles, getRecentFiles, trimRecentFiles } from "./recentFiles";
import {
  BROWSER_STORAGE_KEYS,
  formatScrollbackBytes,
  getStoragePrefs,
  saveStoragePrefs,
  type StoragePrefs,
} from "./storagePrefs";

interface Props {
  open: boolean;
  onClose: () => void;
  onSaveWorkspaceLayout?: () => Promise<boolean | void>;
  onRestoreWorkspaceLayout?: (layoutId: string) => Promise<boolean | void>;
  onDeleteWorkspaceLayout?: (layoutId: string) => Promise<boolean | void>;
}

function defaultPushPreferences(): PushPreferences {
  return {
    waiting_for_input: true,
    agent_task_started: true,
    agent_task_notify: true,
    quiet_hours: {
      enabled: false,
      start_minute: 22 * 60,
      end_minute: 7 * 60,
      utc_offset_minutes: -new Date().getTimezoneOffset(),
      digest: true,
    },
  };
}

function formatMinuteOfDay(minute: number): string {
  const clamped = Math.max(0, Math.min((24 * 60) - 1, Math.round(minute)));
  const hours = Math.floor(clamped / 60);
  const minutes = clamped % 60;
  return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}`;
}

function parseMinuteOfDay(value: string): number {
  const match = value.match(/^(\d{1,2}):(\d{2})$/);
  if (!match) return 0;
  const hours = Math.max(0, Math.min(23, Number(match[1] ?? 0)));
  const minutes = Math.max(0, Math.min(59, Number(match[2] ?? 0)));
  return (hours * 60) + minutes;
}

function normalizeBaseValue(value: string): string {
  return value.trim().replace(/\/+$/, "");
}

const Settings: Component<Props> = (props) => {
  const [token, setT] = createSignal(getToken());
  const [base, setB] = createSignal(getBase());
  const [layoutMode, setL] = createSignal<LayoutMode>(getLayoutMode());
  const [pushOn, setPushOn] = createSignal(false);
  const [pushPerm, setPushPerm] = createSignal<PushPermissionState>("default");
  const [pushBusy, setPushBusy] = createSignal(false);
  const [pushMsg, setPushMsg] = createSignal<string | null>(null);
  const [pushSubscriptions, setPushSubscriptions] = createSignal<PushSubscriptionEntry[]>([]);
  const [currentPushSubId, setCurrentPushSubId] = createSignal<string | null>(null);
  const [pushPrefs, setPushPrefs] = createSignal<PushPreferences>(defaultPushPreferences());
  const [pushLabel, setPushLabel] = createSignal("");
  const [pushQuietStart, setPushQuietStart] = createSignal("22:00");
  const [pushQuietEnd, setPushQuietEnd] = createSignal("07:00");
  const [templateEditorOpen, setTemplateEditorOpen] = createSignal(false);
  const [terminalTheme, setTerminalTheme] = createSignal(getThemeName());
  const [workspaceLayouts, setWorkspaceLayouts] = createSignal<SavedWorkspaceLayout[]>(
    listWorkspaceLayouts(),
  );
  const [authProfiles, setAuthProfiles] = createSignal<AuthProfile[]>(listAuthProfiles());
  const [profileName, setProfileName] = createSignal("");
  const [profileMsg, setProfileMsg] = createSignal<string | null>(null);
  const [storagePrefs, setStoragePrefsState] = createSignal<StoragePrefs>(getStoragePrefs());
  const [storageMsg, setStorageMsg] = createSignal<string | null>(null);
  const [opsStatus, setOpsStatus] = createSignal<OperationalStatus | null>(null);
  const [opsError, setOpsError] = createSignal<string | null>(null);
  const [historyRetentionDays, setHistoryRetentionDays] = createSignal(30);
  const [taskPromptKeepRuns, setTaskPromptKeepRuns] = createSignal(10);
  const [serverCleanupBusy, setServerCleanupBusy] = createSignal(false);
  const [serverCleanupMsg, setServerCleanupMsg] = createSignal<string | null>(null);
  const [browserStorage, setBrowserStorage] = createSignal<{
    localStorageBytes: number;
    localStorageEntries: number;
    estimateUsage?: number;
    estimateQuota?: number;
  } | null>(null);
  const [managedStorage, setManagedStorage] = createSignal<
    {
      key: string;
      label: string;
      count: number;
      limit: number;
      bytes: number;
    }[]
  >([]);

  const refreshLayouts = () => {
    setWorkspaceLayouts(listWorkspaceLayouts());
  };

  const refreshAuthProfiles = () => {
    setAuthProfiles(listAuthProfiles());
  };

  const bytesForKey = (key: string) => {
    try {
      const value = localStorage.getItem(key) ?? "";
      return key.length + value.length;
    } catch {
      return 0;
    }
  };

  const refreshManagedStorage = () => {
    const prefs = getStoragePrefs();
    setManagedStorage([
      {
        key: BROWSER_STORAGE_KEYS.authProfiles,
        label: "Profiles",
        count: listAuthProfiles().length,
        limit: prefs.maxAuthProfiles,
        bytes: bytesForKey(BROWSER_STORAGE_KEYS.authProfiles),
      },
      {
        key: BROWSER_STORAGE_KEYS.workspaceLayouts,
        label: "Layouts",
        count: listWorkspaceLayouts().length,
        limit: prefs.maxWorkspaceLayouts,
        bytes: bytesForKey(BROWSER_STORAGE_KEYS.workspaceLayouts),
      },
      {
        key: BROWSER_STORAGE_KEYS.recentFiles,
        label: "Recent files",
        count: getRecentFiles().length,
        limit: prefs.maxRecentFiles,
        bytes: bytesForKey(BROWSER_STORAGE_KEYS.recentFiles),
      },
      {
        key: BROWSER_STORAGE_KEYS.sessionBookmarks,
        label: "Bookmarks",
        count: bookmarks().length,
        limit: prefs.maxSessionBookmarks,
        bytes: bytesForKey(BROWSER_STORAGE_KEYS.sessionBookmarks),
      },
      {
        key: BROWSER_STORAGE_KEYS.historyPins,
        label: "History pins",
        count: getPinnedHistoryIds().length,
        limit: prefs.maxHistoryPins,
        bytes: bytesForKey(BROWSER_STORAGE_KEYS.historyPins),
      },
    ]);
  };

  const refreshPushState = async () => {
    setPushPerm(await pushPermission());
    setPushOn(await currentPushEnabled());
    const currentId = await currentPushSubscriptionId();
    setCurrentPushSubId(currentId);
    if (!getToken()) {
      setPushSubscriptions([]);
      return;
    }
    try {
      const subs = await api.listPushSubscriptions();
      setPushSubscriptions(subs);
      const current = (currentId ? subs.find((sub) => sub.id === currentId) : null) ?? null;
      if (current) {
        setPushPrefs(current.prefs);
        setPushLabel(current.label ?? "");
        setPushQuietStart(formatMinuteOfDay(current.prefs.quiet_hours.start_minute));
        setPushQuietEnd(formatMinuteOfDay(current.prefs.quiet_hours.end_minute));
      } else {
        const defaults = defaultPushPreferences();
        setPushPrefs(defaults);
        setPushLabel("");
        setPushQuietStart(formatMinuteOfDay(defaults.quiet_hours.start_minute));
        setPushQuietEnd(formatMinuteOfDay(defaults.quiet_hours.end_minute));
      }
    } catch {
      setPushSubscriptions([]);
    }
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
    setStoragePrefsState(getStoragePrefs());
    setStorageMsg(null);
    setServerCleanupMsg(null);
    refreshLayouts();
    refreshAuthProfiles();
    refreshManagedStorage();
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

  const profileMatchesStoredAuth = (profile: AuthProfile) =>
    profile.token.trim() === getToken().trim()
    && normalizeBaseValue(profile.base) === normalizeBaseValue(getBase());

  const save = () => {
    const savedPrefs = saveStoragePrefs(storagePrefs());
    trimAuthProfiles();
    trimWorkspaceLayouts();
    trimRecentFiles();
    trimBookmarks();
    trimHistoryPins();
    setStoragePrefsState(savedPrefs);
    const newTok = token().trim();
    const newBase = normalizeBaseValue(base());
    const newLayout = layoutMode();
    const tokChanged = newTok !== getToken();
    const baseChanged = newBase !== getBase();
    const layoutChanged = newLayout !== getLayoutMode();

    setToken(newTok);
    setBase(newBase);
    setLayoutMode(newLayout);
    refreshAuthProfiles();
    refreshLayouts();
    refreshManagedStorage();

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

  const updateStoragePref = (key: keyof StoragePrefs, value: number) => {
    setStoragePrefsState((current) => ({
      ...current,
      [key]: Math.max(0, Math.round(value)),
    }));
  };

  const applyStorageLimitsNow = () => {
    const savedPrefs = saveStoragePrefs(storagePrefs());
    trimAuthProfiles();
    trimWorkspaceLayouts();
    trimRecentFiles();
    trimBookmarks();
    trimHistoryPins();
    setStoragePrefsState(savedPrefs);
    refreshAuthProfiles();
    refreshLayouts();
    refreshManagedStorage();
    void refreshBrowserStorage();
    setStorageMsg("Applied local retention limits.");
  };

  const clearManagedBrowserData = () => {
    clearAuthProfiles();
    clearWorkspaceLayouts();
    clearRecentFiles();
    clearBookmarks();
    clearHistoryPins();
    refreshAuthProfiles();
    refreshLayouts();
    refreshManagedStorage();
    void refreshBrowserStorage();
    setStorageMsg("Cleared stored profiles, layouts, recent files, bookmarks, and history pins.");
  };

  const cleanupArchivedHistory = async () => {
    setServerCleanupBusy(true);
    setServerCleanupMsg(null);
    try {
      const retentionDays = Math.max(0, Math.round(historyRetentionDays()));
      const result = await api.cleanupHistorySessions(retentionDays);
      await refreshOperationalState();
      setServerCleanupMsg(
        `Removed ${result.removed_sessions} archived session${result.removed_sessions === 1 ? "" : "s"} older than ${result.retention_days} day${result.retention_days === 1 ? "" : "s"}.`,
      );
    } catch (e) {
      setServerCleanupMsg(`History cleanup failed: ${(e as Error).message}`);
    } finally {
      setServerCleanupBusy(false);
    }
  };

  const cleanupTaskPromptArtifacts = async () => {
    setServerCleanupBusy(true);
    setServerCleanupMsg(null);
    try {
      const keepLatestRuns = Math.max(0, Math.round(taskPromptKeepRuns()));
      const result = await api.cleanupAgentTaskArtifacts(keepLatestRuns);
      await refreshOperationalState();
      setServerCleanupMsg(
        `Removed ${result.removed_prompt_file_count} prompt files, ${result.removed_context_file_count} context files, and ${result.removed_task_dir_count} task directories (${formatBytes(result.removed_bytes)}).`,
      );
    } catch (e) {
      setServerCleanupMsg(`Task artifact cleanup failed: ${(e as Error).message}`);
    } finally {
      setServerCleanupBusy(false);
    }
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
      base: normalizeBaseValue(base()),
    });
    refreshAuthProfiles();
    setProfileMsg(`Saved profile "${name}"`);
  };

  const editProfile = (profile: AuthProfile) => {
    setProfileName(profile.name);
    setT(profile.token);
    setB(profile.base);
    setProfileMsg(`Loaded profile "${profile.name}" into the form.`);
  };

  const applyProfile = (profile: AuthProfile) => {
    setToken(profile.token.trim());
    setBase(normalizeBaseValue(profile.base));
    props.onClose();
    location.reload();
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
      setPushMsg(
        `Test dispatched: ${r.ok} ok / ${r.fail} fail${r.queued ? ` / ${r.queued} queued` : ""}`,
      );
    } catch (e) {
      setPushMsg(`Test failed: ${(e as Error).message}`);
    } finally {
      setPushBusy(false);
    }
  };

  const savePushRules = async () => {
    const currentId = currentPushSubId();
    if (!currentId) {
      setPushMsg("Enable push for this device first.");
      return;
    }
    setPushBusy(true);
    setPushMsg(null);
    try {
      const nextPrefs: PushPreferences = {
        ...pushPrefs(),
        quiet_hours: {
          ...pushPrefs().quiet_hours,
          start_minute: parseMinuteOfDay(pushQuietStart()),
          end_minute: parseMinuteOfDay(pushQuietEnd()),
          utc_offset_minutes: -new Date().getTimezoneOffset(),
        },
      };
      const label = pushLabel().trim();
      await api.updatePushSubscription(currentId, {
        label: label || undefined,
        clear_label: !label,
        prefs: nextPrefs,
      });
      await refreshPushState();
      setPushMsg("Notification rules saved for this device.");
    } catch (e) {
      setPushMsg(`Push rules failed: ${(e as Error).message}`);
    } finally {
      setPushBusy(false);
    }
  };

  const flushPushDigests = async () => {
    setPushBusy(true);
    setPushMsg(null);
    try {
      const r = await api.flushPushDigests();
      setPushMsg(
        `Digest flush: ${r.ok} sent / ${r.fail} failed${r.queued ? ` / ${r.queued} queued` : ""}`,
      );
      await refreshPushState();
    } catch (e) {
      setPushMsg(`Digest flush failed: ${(e as Error).message}`);
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
            Bearer token
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
              Primary and scoped tokens both work here. For browser use, prefer
              a scoped token and keep the full primary token out of localStorage
              unless you need admin-only capabilities. Anyone with access to
              this browser profile (or able to run JS in it) can read it.
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
                          <Show when={profileMatchesStoredAuth(profile)}>
                            <span
                              style={{
                                "margin-left": "8px",
                                "font-size": "11px",
                                color: "var(--accent)",
                                "font-weight": 500,
                              }}
                            >
                              Active
                            </span>
                          </Show>
                        </div>
                        <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                          {profile.base || "Same-origin backend"}
                        </div>
                        <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>
                          Updated {formatDate(profile.updated_at)}
                        </div>
                      </div>
                      <div style={{ display: "flex", gap: "8px", "align-items": "center", "flex-wrap": "wrap" }}>
                        <button type="button" onClick={() => applyProfile(profile)}>
                          Apply
                        </button>
                        <button type="button" onClick={() => editProfile(profile)}>
                          Edit
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

          <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Retention & storage
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Cap device-local data growth and set the default scrollback budget for new sessions launched from this browser.
            </div>
            <div
              style={{
                display: "grid",
                "grid-template-columns": "repeat(auto-fit, minmax(160px, 1fr))",
                gap: "8px",
              }}
            >
              <label>
                Recent files
                <input
                  type="number"
                  min="0"
                  max="200"
                  value={storagePrefs().maxRecentFiles}
                  onInput={(e) =>
                    updateStoragePref("maxRecentFiles", Number(e.currentTarget.value || 0))
                  }
                />
              </label>
              <label>
                Saved layouts
                <input
                  type="number"
                  min="0"
                  max="100"
                  value={storagePrefs().maxWorkspaceLayouts}
                  onInput={(e) =>
                    updateStoragePref("maxWorkspaceLayouts", Number(e.currentTarget.value || 0))
                  }
                />
              </label>
              <label>
                Auth profiles
                <input
                  type="number"
                  min="0"
                  max="50"
                  value={storagePrefs().maxAuthProfiles}
                  onInput={(e) =>
                    updateStoragePref("maxAuthProfiles", Number(e.currentTarget.value || 0))
                  }
                />
              </label>
              <label>
                Session bookmarks
                <input
                  type="number"
                  min="0"
                  max="200"
                  value={storagePrefs().maxSessionBookmarks}
                  onInput={(e) =>
                    updateStoragePref("maxSessionBookmarks", Number(e.currentTarget.value || 0))
                  }
                />
              </label>
              <label>
                History pins
                <input
                  type="number"
                  min="0"
                  max="200"
                  value={storagePrefs().maxHistoryPins}
                  onInput={(e) =>
                    updateStoragePref("maxHistoryPins", Number(e.currentTarget.value || 0))
                  }
                />
              </label>
              <label>
                New-session scrollback (KiB)
                <input
                  type="number"
                  min="0"
                  max="16384"
                  step="64"
                  value={Math.round(storagePrefs().defaultSessionScrollbackBytes / 1024)}
                  onInput={(e) =>
                    updateStoragePref(
                      "defaultSessionScrollbackBytes",
                      Math.max(0, Number(e.currentTarget.value || 0)) * 1024,
                    )
                  }
                />
              </label>
            </div>
            <div style={{ "font-size": "11px", color: "var(--fg-muted)", "line-height": 1.5 }}>
              <div>
                New sessions from this browser:{" "}
                <code>{formatScrollbackBytes(storagePrefs().defaultSessionScrollbackBytes)}</code>
              </div>
              <div>
                Set a limit to <code>0</code> to stop retaining that local category.
              </div>
            </div>
            <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
              <button type="button" onClick={applyStorageLimitsNow}>
                Apply limits now
              </button>
              <button type="button" onClick={clearManagedBrowserData}>
                Clear managed browser data
              </button>
            </div>
            <Show when={storageMsg()}>
              <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{storageMsg()}</div>
            </Show>
            <div
              style={{
                display: "grid",
                "grid-template-columns": "repeat(auto-fit, minmax(150px, 1fr))",
                gap: "8px",
              }}
            >
              <For each={managedStorage()}>
                {(entry) => (
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>{entry.label}</div>
                    <div style={opsValueStyle}>{entry.count}</div>
                    <div style={opsMetaStyle}>
                      Limit {entry.limit} • {formatBytes(entry.bytes)}
                    </div>
                  </div>
                )}
              </For>
            </div>
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
                Device-specific push rules for session waits and agent-task alerts
                (permission: <code>{pushPerm()}</code>).
              </div>
              <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
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
                <button
                  onClick={savePushRules}
                  disabled={pushBusy() || !pushOn() || !currentPushSubId()}
                  title="Save rules for the current device subscription"
                >
                  Save rules
                </button>
                <button
                  onClick={flushPushDigests}
                  disabled={pushBusy()}
                  title="Try to deliver any queued digest summaries now"
                >
                  Flush digests
                </button>
              </div>
              <Show when={pushOn()}>
                <div
                  style={{
                    display: "grid",
                    "grid-template-columns": "repeat(auto-fit, minmax(180px, 1fr))",
                    gap: "8px",
                  }}
                >
                  <label>
                    Device label
                    <input
                      type="text"
                      value={pushLabel()}
                      onInput={(e) => setPushLabel(e.currentTarget.value)}
                      placeholder="Pixel 9"
                    />
                  </label>
                  <label>
                    Quiet start
                    <input
                      type="time"
                      value={pushQuietStart()}
                      onInput={(e) => setPushQuietStart(e.currentTarget.value)}
                    />
                  </label>
                  <label>
                    Quiet end
                    <input
                      type="time"
                      value={pushQuietEnd()}
                      onInput={(e) => setPushQuietEnd(e.currentTarget.value)}
                    />
                  </label>
                </div>
              </Show>
              <Show when={pushOn()}>
                <div style={{ display: "flex", gap: "12px", "flex-wrap": "wrap" }}>
                  <label style={{ display: "flex", gap: "6px", "align-items": "center" }}>
                    <input
                      type="checkbox"
                      checked={pushPrefs().waiting_for_input}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          waiting_for_input: e.currentTarget.checked,
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>Session waiting-for-input</span>
                  </label>
                  <label style={{ display: "flex", gap: "6px", "align-items": "center" }}>
                    <input
                      type="checkbox"
                      checked={pushPrefs().agent_task_started}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          agent_task_started: e.currentTarget.checked,
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>Scheduled task started</span>
                  </label>
                  <label style={{ display: "flex", gap: "6px", "align-items": "center" }}>
                    <input
                      type="checkbox"
                      checked={pushPrefs().agent_task_notify}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          agent_task_notify: e.currentTarget.checked,
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>Task phrase alerts</span>
                  </label>
                </div>
              </Show>
              <Show when={pushOn()}>
                <div style={{ display: "flex", gap: "12px", "flex-wrap": "wrap" }}>
                  <label style={{ display: "flex", gap: "6px", "align-items": "center" }}>
                    <input
                      type="checkbox"
                      checked={pushPrefs().quiet_hours.enabled}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          quiet_hours: {
                            ...current.quiet_hours,
                            enabled: e.currentTarget.checked,
                          },
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>Quiet hours</span>
                  </label>
                  <label style={{ display: "flex", gap: "6px", "align-items": "center" }}>
                    <input
                      type="checkbox"
                      checked={pushPrefs().quiet_hours.digest}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          quiet_hours: {
                            ...current.quiet_hours,
                            digest: e.currentTarget.checked,
                          },
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>Digest during quiet hours</span>
                  </label>
                </div>
              </Show>
              <Show when={pushSubscriptions().length > 0}>
                <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
                  <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                    Subscribed devices
                  </div>
                  <For each={pushSubscriptions()}>
                    {(sub) => (
                      <div style={opsCardStyle}>
                        <div style={{ display: "flex", "justify-content": "space-between", gap: "12px" }}>
                          <div style={{ display: "flex", "flex-direction": "column", gap: "4px", "min-width": 0 }}>
                            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
                              {sub.label || (sub.id === currentPushSubId() ? "This device" : sub.kind.kind)}
                            </div>
                            <div style={opsMetaStyle}>
                              {sub.kind.kind}
                              <Show when={sub.kind.endpoint_host}>
                                {(host) => <span> • {host()}</span>}
                              </Show>
                              <span> • {formatDate(sub.created_at)}</span>
                            </div>
                          </div>
                          <div style={{ "font-size": "11px", color: "var(--fg-muted)", "white-space": "nowrap" }}>
                            {sub.id === currentPushSubId() ? "Current" : ""}
                          </div>
                        </div>
                        <div style={opsMetaStyle}>
                          Rules: {sub.prefs.waiting_for_input ? "session" : ""}
                          {sub.prefs.waiting_for_input && (sub.prefs.agent_task_started || sub.prefs.agent_task_notify) ? " • " : ""}
                          {sub.prefs.agent_task_started ? "task start" : ""}
                          {sub.prefs.agent_task_started && sub.prefs.agent_task_notify ? " • " : ""}
                          {sub.prefs.agent_task_notify ? "task alerts" : ""}
                          {!sub.prefs.waiting_for_input && !sub.prefs.agent_task_started && !sub.prefs.agent_task_notify ? "none" : ""}
                          <Show when={sub.pending_digest_count > 0}>
                            <span>
                              {" "}
                              • {sub.pending_digest_count} queued
                            </span>
                          </Show>
                        </div>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={pushMsg()}>
                <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{pushMsg()}</div>
              </Show>
            </Show>
          </div>

          <hr style={{ "border-color": "var(--bd)", "border-style": "solid", margin: "4px 0" }} />

          <div style={{ display: "flex", "flex-direction": "column", gap: "8px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Server retention cleanup
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Trim archived session history by age and remove stale scheduled-task prompt files from the server state directory.
            </div>
            <div
              style={{
                display: "grid",
                "grid-template-columns": "repeat(auto-fit, minmax(180px, 1fr))",
                gap: "8px",
              }}
            >
              <label>
                History retention (days)
                <input
                  type="number"
                  min="0"
                  value={historyRetentionDays()}
                  onInput={(e) => setHistoryRetentionDays(Math.max(0, Number(e.currentTarget.value || 0)))}
                />
              </label>
              <label>
                Keep task prompt runs
                <input
                  type="number"
                  min="0"
                  value={taskPromptKeepRuns()}
                  onInput={(e) => setTaskPromptKeepRuns(Math.max(0, Number(e.currentTarget.value || 0)))}
                />
              </label>
            </div>
            <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
              <button type="button" onClick={() => void cleanupArchivedHistory()} disabled={serverCleanupBusy()}>
                Clean archived history
              </button>
              <button
                type="button"
                onClick={() => void cleanupTaskPromptArtifacts()}
                disabled={serverCleanupBusy()}
              >
                Clean task prompt artifacts
              </button>
            </div>
            <Show when={serverCleanupMsg()}>
              <div style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{serverCleanupMsg()}</div>
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
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>History logs</div>
                    <div style={opsValueStyle}>{status().history.log_file_count ?? 0}</div>
                    <div style={opsMetaStyle}>
                      {formatBytes(status().history.log_bytes)} logs • {formatBytes(status().history.db_bytes)} db
                    </div>
                  </div>
                  <div style={opsCardStyle}>
                    <div style={opsLabelStyle}>Task artifacts</div>
                    <div style={opsValueStyle}>{status().agent_tasks.prompt_file_count}</div>
                    <div style={opsMetaStyle}>
                      {status().agent_tasks.context_file_count} contexts • {status().agent_tasks.orphan_task_dir_count} orphan dirs • {formatBytes(status().agent_tasks.prompt_bytes)}
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
                  <div>Agent tasks: <code>{status().agent_tasks.task_count}</code></div>
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
