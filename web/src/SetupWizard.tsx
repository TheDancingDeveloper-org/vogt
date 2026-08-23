/**
 * The first-run wizard (#292): what an operator sees at `/` while the
 * instance has no tokens at all.
 *
 * The core's install mode is the gate — `App` shows this only when
 * `/api/install/status` said so — and the bootstrap it drives is
 * self-closing, so this surface can exist exactly once per instance. The
 * secret it receives is shown once, with the CLI and MCP equivalents beside
 * it, and is deliberately held in memory only: the "Continue" action hands
 * it to the ordinary sign-in path, which stores it exactly where a pasted
 * token would live, and a front door that uses its own token namespace
 * (`ENGINE.md` §3) refuses it there with guidance rather than persisting an
 * admin secret nothing can use.
 */

import { createSignal, For, Show, type Component } from "solid-js";
import {
  bootstrapInstall,
  type InstallBootstrapResult,
} from "./installApi";
import { getBase } from "./api";

interface SetupWizardProps {
  /** Switch to the ordinary sign-in gate ("I already have a token"). */
  onSignIn: () => void;
  /** The app's sign-in path: validate, store, and enter the shell. */
  onAuthenticated: (token: string, base: string) => Promise<void>;
}

const STEPS = [
  { key: "identity", label: "Identity" },
  { key: "forge", label: "Forge" },
  { key: "project", label: "First project" },
] as const;

const SetupWizard: Component<SetupWizardProps> = (props) => {
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [result, setResult] = createSignal<InstallBootstrapResult | null>(null);
  const [copied, setCopied] = createSignal(false);
  const [continueError, setContinueError] = createSignal<string | null>(null);

  const origin = () => getBase() || window.location.origin;

  const submit = async (event: SubmitEvent) => {
    event.preventDefault();
    const displayName = name().trim();
    if (!displayName) {
      setError("Your name is required — it becomes the first actor.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setResult(await bootstrapInstall(displayName));
    } catch (value) {
      setError(value instanceof Error ? value.message : String(value));
    } finally {
      setBusy(false);
    }
  };

  const copySecret = async () => {
    const secret = result()?.secret;
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
    } catch {
      setCopied(false);
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
        "This deployment's front door uses its own token namespace, so the " +
          "core token cannot sign you in here. Copy the token somewhere " +
          "safe, then sign in with a front-door token (ENGINE.md §3).",
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
                This Vogt has no tokens yet, so it is in install mode: name
                yourself and it will mint your first token. The moment that
                token exists, this door closes for good.
              </p>
              <form class="setup-form" onSubmit={submit}>
                <label>
                  Your name
                  <input
                    type="text"
                    value={name()}
                    onInput={(event) => {
                      setName(event.currentTarget.value);
                      setError(null);
                    }}
                    placeholder="Ada Lovelace"
                    autocomplete="name"
                    spellcheck={false}
                    autofocus
                  />
                </label>
                <Show when={error()}>
                  <div class="login-error" role="alert">{error()}</div>
                </Show>
                <button class="login-submit" type="submit" disabled={busy()}>
                  {busy() ? "Claiming…" : "Claim instance & mint my token"}
                </button>
              </form>
              <p class="login-help">
                Already have a token?{" "}
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
                You are <code>{done.actor.identity_ref}</code>, and this is
                your token. {done.warning}
              </p>
              <div class="setup-secret" data-testid="setup-secret">
                <code>{done.secret}</code>
                <button type="button" onClick={() => void copySecret()}>
                  {copied() ? "Copied" : "Copy"}
                </button>
              </div>
              <details class="setup-equivalents">
                <summary>Use it from a terminal or an agent</summary>
                <p class="login-copy">
                  Save the token to a file — never argv or a URL:
                </p>
                <pre class="setup-snippet">{`umask 077; printf '%s' '<the token above>' > ~/.vogt-token`}</pre>
                <p class="login-copy">MCP (agents), via this deployment's front door:</p>
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
                Forge linking and your first project come next, once you are
                signed in — Projects covers both.
              </p>
            </>
          )}
        </Show>
      </div>
    </main>
  );
};

export default SetupWizard;
