import { Component, Show, createSignal, onMount } from "solid-js";
import { getBase, getToken, setBase, setToken } from "./api";
import { getLayoutMode, setLayoutMode, type LayoutMode } from "./layout";
import TemplateEditor from "./TemplateEditor";
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

  const refreshPushState = async () => {
    setPushPerm(await pushPermission());
    setPushOn(await currentPushEnabled());
  };

  onMount(() => {
    if (isPushAvailable()) {
      void refreshPushState();
    }
  });

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
              Session Templates
            </div>
            <div style={{ "font-size": "12px", color: "var(--fg-muted)" }}>
              Create custom session templates with preset commands and environment variables.
            </div>
            <button onClick={() => setTemplateEditorOpen(true)}>
              Manage Custom Templates
            </button>
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
