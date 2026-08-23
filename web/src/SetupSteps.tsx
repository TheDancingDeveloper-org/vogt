/**
 * The first-run setup steps (#292, increment 3): forge link and first
 * project, at `#/setup`, inside the authenticated shell.
 *
 * The identity step already ran — it is the pre-auth wizard that minted the
 * first token — so this surface starts at step two. Everything here rides
 * the ordinary front door and the registry operations behind it: linking is
 * `forge.account_link` (#179), the picker is `forge.repos` (#180), and the
 * first project is `project.import` or `project.register`, followed by a
 * `sweep` and the `coverage` it produces. Nothing is wizard-only, so
 * skipping any step loses nothing — Projects and Settings own the same
 * capabilities afterwards, and the wizard says so.
 *
 * Every write collects a reason the user typed (FR-W1); nothing here
 * composes one. Each step shows a visible pass or fail with the server's own
 * words, which is what the issue asks of a test-per-step.
 */

import { createSignal, For, onMount, Show, type Component } from "solid-js";
import { useNavigate } from "@solidjs/router";
import SurfaceHeader from "./SurfaceHeader";
import {
  fetchCoverage,
  forgeAccountStatus,
  importProject,
  linkForgeAccount,
  listForgeRepos,
  registerProject,
  runSweep,
  VogtUnavailable,
  type CoverageResult,
  type ForgeRepoView,
  type SweepResult,
} from "./vogtApi";
import { SETUP_PENDING_KEY } from "./installApi";

interface SetupStepsProps {
  onError?: (message: string) => void;
}

type StepState = "pending" | "passed" | "failed" | "skipped";

const describe = (error: unknown): string =>
  error instanceof VogtUnavailable
    ? `Vogt could not be asked: ${error.message}`
    : error instanceof Error
      ? error.message
      : String(error);

const SetupSteps: Component<SetupStepsProps> = (props) => {
  const navigate = useNavigate();

  // -- step two: the forge -------------------------------------------------
  const [forgeState, setForgeState] = createSignal<StepState>("pending");
  const [forgeDetail, setForgeDetail] = createSignal<string | null>(null);
  const [pat, setPat] = createSignal("");
  const [linkReason, setLinkReason] = createSignal("");
  const [linking, setLinking] = createSignal(false);

  // -- step three: the first project ---------------------------------------
  const [projectState, setProjectState] = createSignal<StepState>("pending");
  const [projectDetail, setProjectDetail] = createSignal<string | null>(null);
  const [mode, setMode] = createSignal<"import" | "path">("import");
  const [repos, setRepos] = createSignal<ForgeRepoView[] | null>(null);
  const [reposDetail, setReposDetail] = createSignal<string | null>(null);
  const [reposLoading, setReposLoading] = createSignal(false);
  const [pickedRepo, setPickedRepo] = createSignal<string>("");
  const [pathName, setPathName] = createSignal("");
  const [rootPath, setRootPath] = createSignal("");
  const [projectReason, setProjectReason] = createSignal("");
  const [registering, setRegistering] = createSignal(false);
  const [projectSlug, setProjectSlug] = createSignal<string | null>(null);

  // -- the first sweep and its coverage ------------------------------------
  const [sweepReason, setSweepReason] = createSignal("");
  const [sweeping, setSweeping] = createSignal(false);
  const [sweepOutcome, setSweepOutcome] = createSignal<SweepResult | null>(null);
  const [sweepDetail, setSweepDetail] = createSignal<string | null>(null);
  const [coverage, setCoverage] = createSignal<CoverageResult | null>(null);

  onMount(() => {
    void (async () => {
      try {
        const status = await forgeAccountStatus();
        const linked = status.accounts.find((account) => account.linked);
        if (linked) {
          setForgeState("passed");
          setForgeDetail(`Already linked as ${linked.login} (${linked.host}).`);
        }
      } catch (error) {
        // Not knowing the status is not a failed step — the reader can still
        // link, and the link call will say what is actually wrong.
        setForgeDetail(describe(error));
      }
    })();
  });

  const link = async (event: SubmitEvent) => {
    event.preventDefault();
    if (!pat().trim() || !linkReason().trim()) {
      setForgeState("failed");
      setForgeDetail("A PAT and a typed reason are both required to link.");
      return;
    }
    setLinking(true);
    try {
      const result = await linkForgeAccount({
        token: pat().trim(),
        reason: linkReason().trim(),
      });
      setForgeState("passed");
      setForgeDetail(
        `Linked as ${result.login ?? "unknown"}` +
          (result.scopes ? ` — token scopes: ${result.scopes}.` : "."),
      );
      setPat("");
    } catch (error) {
      setForgeState("failed");
      setForgeDetail(describe(error));
      props.onError?.(describe(error));
    } finally {
      setLinking(false);
    }
  };

  const loadRepos = async () => {
    setReposLoading(true);
    setReposDetail(null);
    try {
      const result = await listForgeRepos();
      setRepos(result.repos);
      if (result.repos.length === 0) {
        setReposDetail(
          result.detail ??
            "No repositories were listed — link a forge account above first.",
        );
      }
    } catch (error) {
      setRepos(null);
      setReposDetail(describe(error));
    } finally {
      setReposLoading(false);
    }
  };

  const claimProject = async (event: SubmitEvent) => {
    event.preventDefault();
    const reason = projectReason().trim();
    if (!reason) {
      setProjectState("failed");
      setProjectDetail("A typed reason is required — the write is audited.");
      return;
    }
    setRegistering(true);
    try {
      if (mode() === "import") {
        if (!pickedRepo()) {
          setProjectState("failed");
          setProjectDetail("Pick a repository to import first.");
          return;
        }
        const result = (await importProject({
          repo: pickedRepo(),
          consolidate: true,
          reason,
        })) as { project?: { slug?: string; name?: string } };
        setProjectSlug(result.project?.slug ?? null);
        setProjectState("passed");
        setProjectDetail(
          `Imported ${result.project?.name ?? pickedRepo()} — cloned, registered, and read back from the forge.`,
        );
      } else {
        if (!pathName().trim() || !rootPath().trim()) {
          setProjectState("failed");
          setProjectDetail("A name and a path are both required to register.");
          return;
        }
        const result = await registerProject({
          name: pathName().trim(),
          root_path: rootPath().trim(),
          reason,
        });
        setProjectSlug(result.project.slug);
        setProjectState("passed");
        setProjectDetail(`Registered ${result.project.name} from ${rootPath().trim()}.`);
      }
    } catch (error) {
      setProjectState("failed");
      setProjectDetail(describe(error));
      props.onError?.(describe(error));
    } finally {
      setRegistering(false);
    }
  };

  const sweep = async (event: SubmitEvent) => {
    event.preventDefault();
    const reason = sweepReason().trim();
    if (!reason) {
      setSweepDetail("A typed reason is required — the sweep is on the record.");
      return;
    }
    setSweeping(true);
    setSweepDetail(null);
    try {
      const slug = projectSlug();
      const result = await runSweep(slug ? { project: slug, reason } : { reason });
      setSweepOutcome(result);
      try {
        setCoverage(await fetchCoverage());
      } catch (error) {
        setSweepDetail(describe(error));
      }
    } catch (error) {
      setSweepDetail(describe(error));
      props.onError?.(describe(error));
    } finally {
      setSweeping(false);
    }
  };

  const finish = () => {
    localStorage.removeItem(SETUP_PENDING_KEY);
    navigate("/projects");
  };

  const stateLabel = (state: StepState) =>
    state === "passed"
      ? "passed"
      : state === "failed"
        ? "failed"
        : state === "skipped"
          ? "skipped"
          : "pending";

  const stepBadge = (state: StepState) => (
    <span
      class={`vogt-setup-state vogt-setup-state--${state}`}
      role="status"
    >
      {stateLabel(state)}
    </span>
  );

  return (
    <section class="vogt-setup" aria-label="First-run setup">
      <SurfaceHeader
        title={<h1>Setup</h1>}
        label="Setup header"
        honesty={
          <span>
            Everything here is also in Projects and Settings — skipping a step
            loses nothing.
          </span>
        }
      />
      <div class="vogt-setup-body">
        <ol class="setup-steps" aria-label="Setup steps">
          <li class="setup-step setup-step--done">Identity</li>
          <li
            classList={{
              "setup-step": true,
              "setup-step--active": forgeState() === "pending",
              "setup-step--done": forgeState() === "passed",
            }}
          >
            Forge
          </li>
          <li
            classList={{
              "setup-step": true,
              "setup-step--active":
                forgeState() !== "pending" && projectState() === "pending",
              "setup-step--done": projectState() === "passed",
            }}
          >
            First project
          </li>
        </ol>

        {/* -- forge ------------------------------------------------------ */}
        <article class="vogt-setup-panel" aria-label="Forge step">
          <h2>Link your forge account {stepBadge(forgeState())}</h2>
          <p>
            Paste a GitHub Personal Access Token and upstream writes are
            attributed to you (#179). It is validated against the forge, then
            stored encrypted — no surface ever returns it.
          </p>
          <Show when={forgeDetail()}>
            {(detail) => (
              <p
                class={
                  forgeState() === "failed"
                    ? "vogt-setup-fail"
                    : "vogt-setup-pass"
                }
                role="status"
              >
                {detail()}
              </p>
            )}
          </Show>
          <form class="vogt-setup-form" onSubmit={(event) => void link(event)}>
            <label>
              Personal Access Token
              <input
                type="password"
                value={pat()}
                onInput={(event) => setPat(event.currentTarget.value)}
                autocomplete="off"
                spellcheck={false}
              />
            </label>
            <label>
              Reason (audited)
              <input
                type="text"
                value={linkReason()}
                onInput={(event) => setLinkReason(event.currentTarget.value)}
                placeholder="why you are linking this account"
              />
            </label>
            <div class="vogt-setup-actions">
              <button type="submit" disabled={linking()}>
                {linking() ? "Validating…" : "Link account"}
              </button>
              <Show when={forgeState() !== "passed"}>
                <button
                  type="button"
                  onClick={() => {
                    setForgeState("skipped");
                    setForgeDetail(
                      "Skipped — link later in Settings, or keep using the instance token.",
                    );
                  }}
                >
                  Skip for now
                </button>
              </Show>
            </div>
          </form>
        </article>

        {/* -- first project ---------------------------------------------- */}
        <article class="vogt-setup-panel" aria-label="First project step">
          <h2>Your first project {stepBadge(projectState())}</h2>
          <p>
            Import a repository your credential can see, or register a path
            already mounted into this instance. Vogt collects only what is
            registered — it discovers nothing.
          </p>
          <div class="vogt-setup-actions" role="group" aria-label="Project source">
            <button
              type="button"
              aria-pressed={mode() === "import"}
              onClick={() => setMode("import")}
            >
              Import from the forge
            </button>
            <button
              type="button"
              aria-pressed={mode() === "path"}
              onClick={() => setMode("path")}
            >
              Register a path
            </button>
          </div>
          <form
            class="vogt-setup-form"
            onSubmit={(event) => void claimProject(event)}
          >
            <Show when={mode() === "import"}>
              <div class="vogt-setup-actions">
                <button
                  type="button"
                  onClick={() => void loadRepos()}
                  disabled={reposLoading()}
                >
                  {reposLoading()
                    ? "Listing…"
                    : repos()
                      ? "Refresh my repositories"
                      : "Browse my repositories"}
                </button>
              </div>
              <Show when={reposDetail()}>
                {(detail) => <p class="vogt-setup-fail">{detail()}</p>}
              </Show>
              <Show when={(repos() ?? []).length > 0}>
                <ul class="vogt-setup-repos" aria-label="Repositories">
                  <For each={repos()}>
                    {(entry) => (
                      <li>
                        <label class="vogt-setup-repo">
                          <input
                            type="radio"
                            name="setup-repo"
                            checked={pickedRepo() === entry.url}
                            disabled={entry.already_registered}
                            onChange={() => setPickedRepo(entry.url)}
                          />
                          <span>
                            {entry.owner}/{entry.name}{" "}
                            <em>
                              {entry.visibility}
                              {entry.already_registered
                                ? " · already imported"
                                : ""}
                            </em>
                          </span>
                        </label>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </Show>
            <Show when={mode() === "path"}>
              <label>
                Project name
                <input
                  type="text"
                  value={pathName()}
                  onInput={(event) => setPathName(event.currentTarget.value)}
                  placeholder="My project"
                />
              </label>
              <label>
                Path on this instance
                <input
                  type="text"
                  value={rootPath()}
                  onInput={(event) => setRootPath(event.currentTarget.value)}
                  placeholder="/workspace/my-project"
                  spellcheck={false}
                />
              </label>
            </Show>
            <label>
              Reason (audited)
              <input
                type="text"
                value={projectReason()}
                onInput={(event) => setProjectReason(event.currentTarget.value)}
                placeholder="why this project is being registered"
              />
            </label>
            <div class="vogt-setup-actions">
              <button type="submit" disabled={registering()}>
                {registering()
                  ? "Working…"
                  : mode() === "import"
                    ? "Import it"
                    : "Register it"}
              </button>
            </div>
          </form>
          <Show when={projectDetail()}>
            {(detail) => (
              <p
                class={
                  projectState() === "failed"
                    ? "vogt-setup-fail"
                    : "vogt-setup-pass"
                }
                role="status"
              >
                {detail()}
              </p>
            )}
          </Show>

          <Show when={projectState() === "passed"}>
            <h3>First sweep</h3>
            <p>
              A sweep is what turns a registered project into evidence — the
              backlog, the board, and coverage all read from it.
            </p>
            <form class="vogt-setup-form" onSubmit={(event) => void sweep(event)}>
              <label>
                Reason (audited)
                <input
                  type="text"
                  value={sweepReason()}
                  onInput={(event) => setSweepReason(event.currentTarget.value)}
                  placeholder="why this sweep is being run"
                />
              </label>
              <div class="vogt-setup-actions">
                <button type="submit" disabled={sweeping()}>
                  {sweeping() ? "Sweeping…" : "Run the first sweep"}
                </button>
              </div>
            </form>
            <Show when={sweepDetail()}>
              {(detail) => <p class="vogt-setup-fail">{detail()}</p>}
            </Show>
            <Show when={sweepOutcome()} keyed>
              {(outcome) => (
                <p class="vogt-setup-pass" role="status">
                  Swept {outcome.projects} project(s):{" "}
                  {outcome.subjects} subject(s), {outcome.dep_refs} dependency
                  reference(s).
                </p>
              )}
            </Show>
            <Show when={coverage()} keyed>
              {(cov) => (
                <div class="vogt-setup-coverage-wrap">
                  <table class="vogt-setup-coverage" aria-label="Collector coverage">
                    <thead>
                      <tr>
                        <th>Collector</th>
                        <th>Status</th>
                        <th>Projects</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={cov.collectors}>
                        {(entry) => (
                          <tr>
                            <td>{entry.collector}</td>
                            <td>{entry.status}</td>
                            <td>{entry.projects}</td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              )}
            </Show>
          </Show>
        </article>

        <div class="vogt-setup-actions vogt-setup-finish">
          <button type="button" onClick={finish}>
            Finish setup
          </button>
          <span>
            Finishing goes to Projects; every step here remains available
            there and in Settings.
          </span>
        </div>
      </div>
    </section>
  );
};

export default SetupSteps;
