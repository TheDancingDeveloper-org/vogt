/**
 * The first-run wizard: what an operator sees at `/` while the
 * instance has no tokens at all.
 *
 * The core's install mode is the gate — `App` shows this only when
 * `/api/install/status` said so — and the bootstrap it drives is
 * self-closing, so this surface can exist exactly once per instance. The
 * operator names themselves and chooses a password; the core creates their
 * login and hands back a *session*, which "Continue" passes to the ordinary
 * sign-in path so it is stored exactly where a later login's session would
 * live. Nothing here is shown once: the durable credential is the password,
 * and API tokens for agents come from `vogt token issue` afterwards.
 */

import { createSignal, For, Show, type Component } from "solid-js";
import {
  bootstrapInstall,
  SETUP_PENDING_KEY,
  type InstallBootstrapResult,
} from "./installApi";
import { getBase } from "./api";

interface SetupWizardProps {
  /** Switch to the ordinary sign-in gate ("I already have a login"). */
  onSignIn: () => void;
  /** The app's sign-in path: validate, store, and enter the shell. */
  onAuthenticated: (token: string, base: string) => Promise<void>;
}

const STEPS = [
  { key: "identity", label: "Identity" },
  { key: "forge", label: "Forge" },
  { key: "project", label: "First project" },
] as const;

/** The core's rule, mirrored so the form can say so before a round trip. */
const MIN_PASSWORD_LEN = 8;

/** What the core derives when no username is given: `human:<slug>`'s slug. */
function suggestUsername(displayName: string): string {
  return displayName
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
}

const SetupWizard: Component<SetupWizardProps> = (props) => {
  const [name, setName] = createSignal("");
  const [username, setUsername] = createSignal("");
  const [usernameEdited, setUsernameEdited] = createSignal(false);
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [result, setResult] = createSignal<InstallBootstrapResult | null>(null);
  const [continueError, setContinueError] = createSignal<string | null>(null);

  const origin = () => getBase() || window.location.origin;

  const submit = async (event: SubmitEvent) => {
    event.preventDefault();
    const displayName = name().trim();
    const login = username().trim();
    if (!displayName) {
      setError("Your name is required — it becomes the first actor.");
      return;
    }
    if (!login) {
      setError("A username is required — it is what you sign in with.");
      return;
    }
    if (password().length < MIN_PASSWORD_LEN) {
      setError(`Choose a password of at least ${MIN_PASSWORD_LEN} characters.`);
      return;
    }
    if (password() !== confirm()) {
      setError("The two passwords do not match.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setResult(await bootstrapInstall(displayName, { username: login, password: password() }));
      // The remaining steps — forge link, first project — live at `#/setup`
      // inside the shell; this flag is what brings a fresh operator there
      // after their first sign-in, however that sign-in happens.
      localStorage.setItem(SETUP_PENDING_KEY, "1");
    } catch (value) {
      setError(value instanceof Error ? value.message : String(value));
    } finally {
      setBusy(false);
    }
  };

  const continueToApp = async () => {
    const secret = result()?.secret;
    if (!secret) return;
    setBusy(true);
    setContinueError(null);
    try {
      await props.onAuthenticated(secret, "");
    } catch {
      setContinueError(
        "Your login was created, but this front door did not accept the " +
          "session it minted. Sign in with your username and password instead.",
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <main class="login-screen setup-wizard">
      <div class="login-card setup-card">
        <div class="login-eyebrow">Vogt · First run</div>
        <ol class="setup-steps" aria-label="Setup steps">
          <For each={STEPS}>
            {(step, index) => (
              <li
                classList={{
                  "setup-step": true,
                  "setup-step--active": index() === 0 && !result(),
                  "setup-step--done": index() === 0 && !!result(),
                }}
                aria-current={index() === 0 && !result() ? "step" : undefined}
              >
                {step.label}
              </li>
            )}
          </For>
        </ol>
        <Show
          when={result()}
          keyed
          fallback={
            <>
              <h1>Claim this instance</h1>
              <p class="login-copy">
                This Vogt has no logins yet, so it is in install mode: name
                yourself and choose a password, and it becomes yours. The
                moment your login exists, this door closes for good.
              </p>
              <form class="setup-form" onSubmit={submit}>
                <label>
                  Your name
                  <input
                    type="text"
                    value={name()}
                    onInput={(event) => {
                      setName(event.currentTarget.value);
                      if (!usernameEdited()) setUsername(suggestUsername(event.currentTarget.value));
                      setError(null);
                    }}
                    placeholder="Ada Lovelace"
                    autocomplete="name"
                    spellcheck={false}
                    autofocus
                  />
                </label>
                <label>
                  Username
                  <input
                    type="text"
                    value={username()}
                    onInput={(event) => {
                      setUsername(event.currentTarget.value);
                      setUsernameEdited(true);
                      setError(null);
                    }}
                    placeholder="ada"
                    autocomplete="username"
                    autocapitalize="none"
                    spellcheck={false}
                  />
                </label>
                <label>
                  Password
                  <input
                    type="password"
                    value={password()}
                    onInput={(event) => {
                      setPassword(event.currentTarget.value);
                      setError(null);
                    }}
                    autocomplete="new-password"
                  />
                </label>
                <label>
                  Confirm password
                  <input
                    type="password"
                    value={confirm()}
                    onInput={(event) => {
                      setConfirm(event.currentTarget.value);
                      setError(null);
                    }}
                    autocomplete="new-password"
                  />
                </label>
                <Show when={error()}>
                  <div class="login-error" role="alert">{error()}</div>
                </Show>
                <button class="login-submit" type="submit" disabled={busy()}>
                  {busy() ? "Claiming…" : "Claim instance & create my login"}
                </button>
              </form>
              <p class="login-help">
                Already have a login or a token?{" "}
                <button type="button" class="setup-link" onClick={props.onSignIn}>
                  Sign in instead
                </button>
              </p>
            </>
          }
        >
          {(done) => (
            <>
              <h1>Welcome, {done.actor.display_name}</h1>
              <p class="login-copy">
                You are <code>{done.actor.identity_ref}</code>. Sign in from
                any browser or phone as{" "}
                <code data-testid="setup-username">{done.username ?? done.actor.identity_ref}</code>{" "}
                with the password you chose; this session is already yours.
              </p>
              <details class="setup-equivalents">
                <summary>Use it from a terminal or an agent</summary>
                <p class="login-copy">
                  Agents and scripts hold API tokens, not your password. Mint
                  one once you are in, from the instance that owns the data:
                </p>
                <pre class="setup-snippet">{`vogt token issue --actor ${done.actor.identity_ref} --name laptop --scopes read,work.write --reason "an agent credential"`}</pre>
                <p class="login-copy">Then, for MCP via this deployment's front door:</p>
                <pre class="setup-snippet">{`VOGT_URL=${origin()} VOGT_TOKEN_FILE=~/.vogt-token vogt-mcp-remote`}</pre>
                <p class="login-copy">
                  The full connection document, endpoints included, is at{" "}
                  <code>{origin()}/connection-info</code>.
                </p>
              </details>
              <Show when={continueError()}>
                <div class="login-error" role="alert">{continueError()}</div>
              </Show>
              <button
                class="login-submit"
                type="button"
                disabled={busy()}
                onClick={() => void continueToApp()}
              >
                {busy() ? "Signing in…" : "Continue to Vogt"}
              </button>
              <Show when={continueError()}>
                <button type="button" onClick={props.onSignIn}>
                  Go to sign-in
                </button>
              </Show>
              <p class="login-help">
                Forge linking and your first project are the next two steps —
                they open automatically once you are signed in.
              </p>
            </>
          )}
        </Show>
      </div>
    </main>
  );
};

export default SetupWizard;
