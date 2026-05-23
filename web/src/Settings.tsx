import { Component, Show, createSignal } from "solid-js";
import { getBase, getToken, setBase, setToken } from "./api";

interface Props {
  open: boolean;
  onClose: () => void;
}

const Settings: Component<Props> = (props) => {
  const [token, setT] = createSignal(getToken());
  const [base, setB] = createSignal(getBase());

  const save = () => {
    setToken(token().trim());
    setBase(base().trim());
    props.onClose();
    // Force a soft reload so the new credentials take effect everywhere.
    location.reload();
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
          </label>
          <label>
            Backend base URL (blank = same origin)
            <input
              type="text"
              value={base()}
              onInput={(e) => setB(e.currentTarget.value)}
              placeholder="http://mydevenv2.tailnet.ts.net:8910"
              autocomplete="off"
              spellcheck={false}
            />
          </label>
          <div class="modal-actions">
            <button onClick={props.onClose}>Cancel</button>
            <button onClick={save}>Save & reload</button>
          </div>
        </div>
      </div>
    </Show>
  );
};

export default Settings;
