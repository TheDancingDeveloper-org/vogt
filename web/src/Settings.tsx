import { Component, Show, For, createEffect, createSignal, onMount } from "solid-js";
import {
  api,
  ApiError,
  getBase,
  getToken,
  setBase,
  setToken,
  signOut,
  validateCredentials,
  type OperationalStatus,
  type PushPreferences,
  type PushSubscriptionEntry,
} from "./api";
import { getLayoutMode, setLayoutMode, type LayoutMode } from "./layout";
import TemplateEditor from "./TemplateEditor";
import Dialog from "./Dialog";
import { THEMES, getThemeName, setThemeName } from "./terminalThemes";
import {
  APP_THEMES,
  APP_THEME_ORDER,
  SYSTEM_SELECTION,
  getAppThemeSelection,
  setAppTheme,
} from "./appThemes";
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
  defaultDeviceLabel,
  isPushAvailable,
  pushPermission,
  pushSelfTest,
  pushSupportReason,
  subscribePushNotifications,
  unsubscribePushNotifications,
  type PushPermissionState,
} from "./push";
import {
  reconcilePush,
  saveButtonLabel,
  saveDisabled,
} from "./settingsModel";
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
  /**
   * Confirm a destructive action before it runs. Threaded in from the shell's
   * `confirmUser`, the same modal that guards deleting a layout. Optional so a
   * bare mount (a test, a future embed) still functions, defaulting to "yes".
   */
  confirmAction?: (title: string, body?: string) => Promise<boolean>;
}

const SETTINGS_SECTIONS = [
  { id: "connection", label: "Connection" },
  { id: "layout", label: "Layout" },
  { id: "theme", label: "Theme" },
  { id: "presets", label: "Presets" },
  { id: "workspace-layouts", label: "Layouts" },
  { id: "storage", label: "Storage" },
  { id: "push", label: "Push" },
  { id: "server-cleanup", label: "Server" },
  { id: "ops", label: "Operations" },
] as const;

// Must agree with `PushPreferences::default()` in the engine's `push.rs`,
// which is where FR-M2's "and for nothing else by default" is actually kept.
// This copy is only what the form shows before the server has answered; a
// disagreement would make the checkboxes lie about what a new device signed
// up for.
function defaultPushPreferences(): PushPreferences {
  return {
    waiting_for_input: true,
    errored: true,
    idle_stall: false,
    agent_task_started: false,
    agent_task_notify: true,
    drift: true,
    quiet_hours: {
      enabled: false,
      start_minute: 22 * 60,
      end_minute: 7 * 60,
      utc_offset_minutes: -new Date().getTimezoneOffset(),
      digest: true,
    },
  };
}

// What this device will actually be interrupted by, enumerated rather than
// hand-chained. The chain this replaced named three of the five kinds, so a
// device subscribed only to `errored` read as "none" — which is the one
// answer a summary of a notification channel must never give wrongly.
function describePushRules(prefs: PushPreferences): string {
  const on = [
    prefs.waiting_for_input && "waiting",
    prefs.errored && "errored",
    prefs.idle_stall && "idle stall",
    prefs.agent_task_started && "task start",
    prefs.agent_task_notify && "task alerts",
    prefs.drift && "drift",
  ].filter((label): label is string => typeof label === "string");
  return on.length ? on.join(" • ") : "none";
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
  const [showToken, setShowToken] = createSignal(false);
  const [authCheck, setAuthCheck] = createSignal<
    "idle" | "checking" | "valid" | "invalid"
  >("idle");
  const [authCheckMsg, setAuthCheckMsg] = createSignal<string | null>(null);
  const [layoutMode, setL] = createSignal<LayoutMode>(getLayoutMode());
  const [pushOn, setPushOn] = createSignal(false);
  const [pushPerm, setPushPerm] = createSignal<PushPermissionState>("default");
  const [pushBusy, setPushBusy] = createSignal(false);
  const [pushMsg, setPushMsg] = createSignal<string | null>(null);
  const [pushSubscriptions, setPushSubscriptions] = createSignal<PushSubscriptionEntry[]>([]);
  const [currentPushSubId, setCurrentPushSubId] = createSignal<string | null>(null);
  const [pushServerDropped, setPushServerDropped] = createSignal(false);
  const [pushPrefs, setPushPrefs] = createSignal<PushPreferences>(defaultPushPreferences());
  const [pushLabel, setPushLabel] = createSignal("");
  const [pushQuietStart, setPushQuietStart] = createSignal("22:00");
  const [pushQuietEnd, setPushQuietEnd] = createSignal("07:00");
  const [templateEditorOpen, setTemplateEditorOpen] = createSignal(false);
  const [terminalTheme, setTerminalTheme] = createSignal(getThemeName());
  const [appTheme, setAppThemeSel] = createSignal(getAppThemeSelection());
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
    const enabled = await currentPushEnabled();
    setPushOn(enabled);
    const currentId = await currentPushSubscriptionId();
    setCurrentPushSubId(currentId);
    if (!getToken()) {
      setPushSubscriptions([]);
      setPushServerDropped(false);
      return;
    }
    try {
      const subs = await api.listPushSubscriptions();
      setPushSubscriptions(subs);
      // Compare this device's live subscription against the server's list: if
      // the browser still believes it is subscribed but the server dropped the
      // row, offer to re-register rather than silently receiving nothing.
      setPushServerDropped(reconcilePush(currentId, subs, enabled).offerReEnable);
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
      setPushServerDropped(false);
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
    setShowToken(false);
    setAuthCheck("idle");
    setAuthCheckMsg(null);
    setL(getLayoutMode());
    setAppThemeSel(getAppThemeSelection());
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
    if (getToken()) void validateAuth();
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

  const authFailureMessage = (error: unknown): string => {
    if (error instanceof ApiError && error.status === 401) {
      return "Token rejected (401). Check that you copied the current Vogt token exactly.";
    }
    if (error instanceof ApiError && error.status === 403) {
      return "Token accepted, but it does not have permission to access this app (403).";
    }
    if (error instanceof ApiError) {
      return `Server rejected the validation request (HTTP ${error.status}).`;
    }
    return `Could not reach the backend: ${error instanceof Error ? error.message : String(error)}`;
  };

  const validateAuth = async (
    candidateToken = token().trim(),
    candidateBase = normalizeBaseValue(base()),
  ): Promise<boolean> => {
    setAuthCheck("checking");
    setAuthCheckMsg("Checking token with the backend…");
    try {
      const status = await validateCredentials(candidateToken, candidateBase);
      // Ignore a stale success if the user edited the fields while the request ran.
      if (
        candidateToken !== token().trim()
        || candidateBase !== normalizeBaseValue(base())
      ) {
        setAuthCheck("idle");
        setAuthCheckMsg("Credentials changed; validate again.");
        return false;
      }
      setAuthCheck("valid");
      setAuthCheckMsg(`Authenticated successfully with Vogt ${status.version}.`);
      return true;
    } catch (error) {
      setAuthCheck("invalid");
      setAuthCheckMsg(authFailureMessage(error));
      return false;
    }
  };

  const save = async () => {
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

    // Only revalidate when the credential actually moved. A preferences-only
    // save (layout, retention limits) must not require a round-trip — and a
    // same-origin deployment with a blank-but-unchanged token could never
    // save at all while validation was mandatory.
    if ((tokChanged || baseChanged) && !(await validateAuth(newTok, newBase))) {
      return;
    }

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

  // Destructive actions were one-click. Route them through the shell's
  // confirmation modal (the same one that guards deleting a layout); a bare
  // mount with no `confirmAction` still proceeds.
  const confirmDestructive = (title: string, body?: string): Promise<boolean> =>
    props.confirmAction ? props.confirmAction(title, body) : Promise.resolve(true);

  // What differs from what is persisted — drives the Save button's label and
  // whether a reload is coming.
  const dirtyFlags = () => ({
    token: token().trim() !== getToken(),
    base: normalizeBaseValue(base()) !== getBase(),
    layout: layoutMode() !== getLayoutMode(),
    storage: JSON.stringify(storagePrefs()) !== JSON.stringify(getStoragePrefs()),
  });

  const scrollToSection = (id: string) => {
    document
      .getElementById(`settings-section-${id}`)
      ?.scrollIntoView({ block: "start", behavior: "smooth" });
  };

  const clearAuth = async () => {
    if (
      !(await confirmDestructive(
        "Sign out of Vogt?",
        "Clears the saved token and base URL from this browser and returns to the login screen.",
      ))
    ) {
      return;
    }
    // Handing the credential back is the same session-level fact as having it
    // refused, so it travels the same way (#195): `signOut` clears both token
    // and base and publishes the rejection, and the shell returns to the
    // login screen from its one subscriber. That is why this no longer
    // reloads the page to get there — and why the other tabs follow.
    // Closed first: `signOut` returns the shell to the login screen in the
    // same tick, which unmounts this modal underneath the call.
    props.onClose();
    signOut();
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

  const clearManagedBrowserData = async () => {
    if (
      !(await confirmDestructive(
        "Clear managed browser data?",
        "Removes stored profiles, layouts, recent files, bookmarks, and history pins from this browser. Server data is untouched.",
      ))
    ) {
      return;
    }
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
    if (
      !(await confirmDestructive(
        "Purge archived history?",
        `Permanently removes archived session history older than ${Math.max(0, Math.round(historyRetentionDays()))} day(s) from the server.`,
      ))
    ) {
      return;
    }
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
    if (
      !(await confirmDestructive(
        "Remove task prompt artifacts?",
        `Permanently deletes scheduled-task prompt and context files from the server, keeping the latest ${Math.max(0, Math.round(taskPromptKeepRuns()))} run(s).`,
      ))
    ) {
      return;
    }
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

  const applyProfile = async (profile: AuthProfile) => {
    setT(profile.token);
    setB(profile.base);
    if (!(await validateAuth(profile.token.trim(), normalizeBaseValue(profile.base)))) {
      setProfileMsg(`Profile "${profile.name}" was not applied because validation failed.`);
      return;
    }
    setToken(profile.token.trim());
    setBase(normalizeBaseValue(profile.base));
    props.onClose();
    location.reload();
  };

  const removeProfile = async (id: string, name: string) => {
    if (
      !(await confirmDestructive(
        "Delete this profile?",
        `Removes the saved auth profile "${name}" from this browser.`,
      ))
    ) {
      return;
    }
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
        const label = pushLabel().trim() || defaultDeviceLabel();
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

  // The browser still holds a live subscription but the server dropped it;
  // re-POST it so this device starts receiving again.
  const reEnablePush = async () => {
    setPushBusy(true);
    setPushMsg(null);
    try {
      const label = pushLabel().trim() || defaultDeviceLabel();
      const r = await subscribePushNotifications(label);
      setPushMsg(`Re-registered with the server (id ${r.id.slice(0, 12)}…)`);
      await refreshPushState();
    } catch (e) {
      setPushMsg(`Re-enable failed: ${(e as Error).message}`);
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
      <Dialog label="Settings" dialogClass="modal settings-modal" onClose={props.onClose}>
        <div class="settings-modal-top">
          <div class="settings-modal-header">
            <h2 class="settings-modal-title">Settings</h2>
            <button
              type="button"
              class="settings-modal-close"
              aria-label="Close settings"
              onClick={props.onClose}
            >
              ×
            </button>
          </div>
          <nav class="settings-modal-nav" aria-label="Settings sections">
            <For each={SETTINGS_SECTIONS}>
              {(section) => (
                <button type="button" onClick={() => scrollToSection(section.id)}>
                  {section.label}
                </button>
              )}
            </For>
          </nav>
        </div>
        <div class="settings-modal-body">
          <section id="settings-section-connection" class="settings-section">
          <label>
            Bearer token
            <div class="settings-token-row">
              <input
                type={showToken() ? "text" : "password"}
                value={token()}
                onInput={(e) => {
                  setT(e.currentTarget.value);
                  setAuthCheck("idle");
                  setAuthCheckMsg("Token changed; validate before saving.");
                }}
                autocomplete="off"
                spellcheck={false}
              />
              <label class="settings-show-token">
                <input
                  type="checkbox"
                  checked={showToken()}
                  onChange={(e) => setShowToken(e.currentTarget.checked)}
                />
                Show token
              </label>
            </div>
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
              onInput={(e) => {
                setB(e.currentTarget.value);
                setAuthCheck("idle");
                setAuthCheckMsg("Backend changed; validate before saving.");
              }}
              placeholder="https://your-vogt.example"
              autocomplete="off"
              spellcheck={false}
            />
          </label>
          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <button
              type="button"
              disabled={authCheck() === "checking" || !token().trim()}
              onClick={() => void validateAuth()}
            >
              {authCheck() === "checking" ? "Validating…" : "Validate token"}
            </button>
            <Show when={authCheckMsg()}>
              <div
                role="status"
                style={{
                  "font-size": "12px",
                  color: authCheck() === "valid"
                    ? "var(--activity-done)"
                    : authCheck() === "invalid"
                      ? "var(--danger)"
                      : "var(--fg-muted)",
                }}
              >
                {authCheckMsg()}
              </div>
            </Show>
          </div>
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
              <div role="status" style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{profileMsg()}</div>
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
                        <button type="button" onClick={() => void applyProfile(profile)}>
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
            <button type="button" onClick={() => void clearAuth()}>Sign out &amp; clear saved auth</button>
          </div>
          </section>

          <section id="settings-section-layout" class="settings-section">
          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Layout Mode{" "}
              <span style={{ color: "var(--fg-muted)", "font-weight": 400, "font-size": "12px" }}>
                (requires reload)
              </span>
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Choose between tabbed mode (default) or IDE mode with persistent file tree.
              Switching reloads the app.
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

          </section>

          <section id="settings-section-theme" class="settings-section">
          <div style={{ display: "flex", "flex-direction": "column", gap: "6px", "margin-bottom": "16px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              App Theme{" "}
              <span style={{ color: "var(--fg-muted)", "font-weight": 400, "font-size": "12px" }}>
                (applies immediately)
              </span>
            </div>
            <select
              aria-label="App theme"
              value={appTheme()}
              onChange={(e) => {
                const sel = e.currentTarget.value;
                setAppThemeSel(sel);
                setAppTheme(sel);
                // The terminal follows the shell unless the reader pinned one,
                // so reflect any newly-derived preset in its picker.
                setTerminalTheme(getThemeName());
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
              <option value={SYSTEM_SELECTION}>System (follow device)</option>
              <For each={[...APP_THEME_ORDER]}>
                {(id) => <option value={id}>{APP_THEMES[id]!.label}</option>}
              </For>
            </select>
          </div>

          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Terminal Theme{" "}
              <span style={{ color: "var(--fg-muted)", "font-weight": 400, "font-size": "12px" }}>
                (applies immediately)
              </span>
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

          </section>

          <section id="settings-section-presets" class="settings-section">
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

          </section>

          <section id="settings-section-workspace-layouts" class="settings-section">
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

          </section>

          <section id="settings-section-storage" class="settings-section">
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
              <div role="status" style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{storageMsg()}</div>
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

          </section>

          <section id="settings-section-push" class="settings-section">
          <div style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
            <div style={{ "font-size": "13px", color: "var(--fg)", "font-weight": 600 }}>
              Push notifications
            </div>
            <Show
              when={isPushAvailable()}
              fallback={
                <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                  <Show
                    when={pushSupportReason() === "ios-home-screen"}
                    fallback={<span>Not supported in this browser.</span>}
                  >
                    <span>
                      To receive push on iOS, open the Share menu and choose
                      {" "}<strong>Add to Home Screen</strong>, then open Vogt
                      from that icon and enable push here.
                    </span>
                  </Show>
                </div>
              }
            >
              <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
                Device-specific push rules for session waits and agent-task alerts
                (permission: <code>{pushPerm()}</code>).
              </div>
              <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
                <button
                  onClick={togglePush}
                  disabled={pushBusy() || (pushPerm() === "denied" && !pushOn())}
                >
                  {pushOn() ? "Disable" : "Enable"} push
                </button>
                <Show when={pushServerDropped()}>
                  <button onClick={() => void reEnablePush()} disabled={pushBusy()}>
                    Re-enable
                  </button>
                </Show>
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
              <Show when={pushPerm() === "denied" && !pushOn()}>
                <div role="status" style={{ "font-size": "12px", color: "var(--danger)" }}>
                  Notifications are blocked — allow in site settings, then reload.
                </div>
              </Show>
              <Show when={pushServerDropped()}>
                <div role="status" style={{ "font-size": "12px", color: "var(--activity-warning)" }}>
                  The server no longer lists this device's subscription.
                  {" "}Re-enable to start receiving again.
                </div>
              </Show>
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
                      checked={pushPrefs().errored}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          errored: e.currentTarget.checked,
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>Session errored</span>
                  </label>
                  <label style={{ display: "flex", gap: "6px", "align-items": "center" }}>
                    <input
                      type="checkbox"
                      checked={pushPrefs().idle_stall}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          idle_stall: e.currentTarget.checked,
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>Idle stall</span>
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
                  <label style={{ display: "flex", gap: "6px", "align-items": "center" }}>
                    <input
                      type="checkbox"
                      checked={pushPrefs().drift}
                      onChange={(e) =>
                        setPushPrefs((current) => ({
                          ...current,
                          drift: e.currentTarget.checked,
                        }))
                      }
                    />
                    <span style={{ "font-size": "12px" }}>New drift</span>
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
                          Rules: {describePushRules(sub.prefs)}
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
                <div role="status" style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{pushMsg()}</div>
              </Show>
            </Show>
          </div>

          </section>

          <section id="settings-section-server-cleanup" class="settings-section">
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
              <div role="status" style={{ "font-size": "11px", color: "var(--fg-muted)" }}>{serverCleanupMsg()}</div>
            </Show>
          </div>

          </section>

          <section id="settings-section-ops" class="settings-section">
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
              <div style={{ "font-size": "11px", color: "var(--danger)" }}>{opsError()}</div>
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
          </section>
        </div>

        <div class="settings-modal-footer">
          <button type="button" onClick={props.onClose}>Cancel</button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={saveDisabled({
              checking: authCheck() === "checking",
              tokenBlank: !token().trim(),
              tokenChanged: token().trim() !== getToken(),
            })}
          >
            {saveButtonLabel(dirtyFlags())}
          </button>
        </div>
      </Dialog>
      <TemplateEditor
        open={templateEditorOpen()}
        onClose={() => setTemplateEditorOpen(false)}
        confirmAction={confirmDestructive}
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
