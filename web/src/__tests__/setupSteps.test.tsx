// The setup steps surface (#292, increment 3): forge link and first project,
// each with a visible pass/fail carrying the server's own words.
//
// The fake sits under `vogtApi.ts`'s one fetch, so what these tests exercise
// is the real route table — a passing test here is a claim about the URLs
// and bodies a real Vogt would receive.

import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import SetupSteps from "../SetupSteps";
import { SETUP_PENDING_KEY } from "../installApi";
import { setToken } from "../api";
import { fakeVogt, mountAt, settle } from "./harness";

beforeEach(() => {
  localStorage.clear();
  setToken("setup-test-token");
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const mount = () => mountAt("/setup", "/setup", () => <SetupSteps />);

describe("#292 — the forge step", () => {
  it("links a PAT with a typed reason and reports who it authenticated as", async () => {
    const vogt = fakeVogt({
      "GET /forge/accounts": { body: { accounts: [] } },
      "POST /forge/accounts": {
        body: { host: "github.com", login: "ada", scopes: "repo", linked: true },
      },
    });
    mount();
    await settle();

    await fireEvent.input(screen.getByLabelText("Personal Access Token"), {
      target: { value: "ghp_wizard" },
    });
    const [linkReason] = screen.getAllByLabelText("Reason (audited)");
    await fireEvent.input(linkReason!, {
      target: { value: "first-run: my own attribution" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Link account" }));

    await waitFor(() =>
      expect(
        screen.getByText(/Linked as ada — token scopes: repo\./),
      ).toBeInTheDocument(),
    );
    const linkCalls = vogt.matching("POST /forge/accounts");
    expect(linkCalls).toHaveLength(1);
    expect(linkCalls[0]!.body).toMatchObject({
      token: "ghp_wizard",
      reason: "first-run: my own attribution",
    });
  });

  it("shows the server's refusal and refuses to link without a reason", async () => {
    const vogt = fakeVogt({
      "GET /forge/accounts": { body: { accounts: [] } },
      "POST /forge/accounts": {
        status: 400,
        body: { error: { message: "the token was rejected by the forge" } },
      },
    });
    mount();
    await settle();

    // No reason typed: nothing is sent.
    await fireEvent.input(screen.getByLabelText("Personal Access Token"), {
      target: { value: "ghp_wizard" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Link account" }));
    expect(await screen.findByText(/typed reason/)).toBeInTheDocument();
    expect(vogt.matching("POST /forge/accounts")).toHaveLength(0);

    const [linkReason] = screen.getAllByLabelText("Reason (audited)");
    await fireEvent.input(linkReason!, { target: { value: "trying" } });
    await fireEvent.click(screen.getByRole("button", { name: "Link account" }));
    await waitFor(() =>
      expect(
        screen.getByText(/the token was rejected by the forge/),
      ).toBeInTheDocument(),
    );
  });

  it("reports an already-linked account as a passed step", async () => {
    fakeVogt({
      "GET /forge/accounts": {
        body: {
          accounts: [
            { host: "github.com", login: "ada", scopes: "repo", linked: true },
          ],
        },
      },
    });
    mount();
    await waitFor(() =>
      expect(
        screen.getByText("Already linked as ada (github.com)."),
      ).toBeInTheDocument(),
    );
  });
});

describe("#292 — the first project step", () => {
  it("imports a picked repository, sweeps it, and shows coverage", async () => {
    const vogt = fakeVogt({
      "GET /forge/accounts": { body: { accounts: [] } },
      "GET /forge/repos": {
        body: {
          repos: [
            {
              owner: "ada",
              name: "engine",
              default_branch: "main",
              visibility: "private",
              url: "https://github.com/ada/engine",
              already_registered: false,
            },
          ],
          login: "ada",
          detail: null,
        },
      },
      "POST /projects/import": {
        body: { project: { slug: "engine", name: "engine" } },
      },
      "POST /sweep": {
        body: { scope: "project:engine", projects: 1, subjects: 12, dep_refs: 3, reports: [] },
      },
      "GET /coverage": {
        body: {
          collectors: [
            { collector: "git_local", status: "current", last_swept_at: null, age_seconds: 0, projects: 1 },
          ],
          swept_project_ids: ["prj-1"],
          unswept_project_ids: [],
        },
      },
    });
    mount();
    await settle();

    await fireEvent.click(
      screen.getByRole("button", { name: "Browse my repositories" }),
    );
    const repo = await screen.findByRole("radio");
    await fireEvent.click(repo);
    const reasons = screen.getAllByLabelText("Reason (audited)");
    await fireEvent.input(reasons[1]!, {
      target: { value: "first project for this instance" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Import it" }));

    await waitFor(() =>
      expect(screen.getByText(/Imported engine/)).toBeInTheDocument(),
    );
    expect(vogt.matching("POST /projects/import")[0]!.body).toMatchObject({
      repo: "https://github.com/ada/engine",
      reason: "first project for this instance",
    });

    // The first sweep, with its own typed reason, and the coverage it earns.
    const sweepReason = screen
      .getAllByLabelText("Reason (audited)")
      .at(-1)!;
    await fireEvent.input(sweepReason, {
      target: { value: "baseline evidence" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Run the first sweep" }),
    );
    await waitFor(() =>
      expect(screen.getByText(/Swept 1 project/)).toBeInTheDocument(),
    );
    expect(
      await screen.findByRole("table", { name: "Collector coverage" }),
    ).toHaveTextContent("git_local");
    expect(vogt.matching("POST /sweep")[0]!.body).toMatchObject({
      project: "engine",
      reason: "baseline evidence",
    });
  });

  it("registers a mounted path as the alternative to importing", async () => {
    const vogt = fakeVogt({
      "GET /forge/accounts": { body: { accounts: [] } },
      "POST /projects": {
        body: { project: { slug: "my-project", name: "My project" } },
      },
    });
    mount();
    await settle();

    await fireEvent.click(
      screen.getByRole("button", { name: "Register a path" }),
    );
    await fireEvent.input(screen.getByLabelText("Project name"), {
      target: { value: "My project" },
    });
    await fireEvent.input(screen.getByLabelText("Path on this instance"), {
      target: { value: "/workspace/my-project" },
    });
    const reasons = screen.getAllByLabelText("Reason (audited)");
    await fireEvent.input(reasons[1]!, {
      target: { value: "the mounted checkout" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Register it" }));

    await waitFor(() =>
      expect(
        screen.getByText("Registered My project from /workspace/my-project."),
      ).toBeInTheDocument(),
    );
    expect(vogt.matching("POST /projects")[0]!.body).toMatchObject({
      name: "My project",
      root_path: "/workspace/my-project",
      reason: "the mounted checkout",
    });
  });

  it("renders an outage as Vogt's own words, not as emptiness", async () => {
    fakeVogt({
      "GET /forge/accounts": { body: { accounts: [] } },
      "GET /forge/repos": {
        status: 503,
        body: { error: { message: "no core is configured for this front door" } },
      },
    });
    mount();
    await settle();
    await fireEvent.click(
      screen.getByRole("button", { name: "Browse my repositories" }),
    );
    await waitFor(() =>
      expect(
        screen.getByText(/Vogt could not be asked: no core is configured/),
      ).toBeInTheDocument(),
    );
  });
});

describe("#292 — finishing", () => {
  it("clears the pending flag and leaves for Projects", async () => {
    fakeVogt({ "GET /forge/accounts": { body: { accounts: [] } } });
    localStorage.setItem(SETUP_PENDING_KEY, "1");
    const mounted = mount();
    await settle();
    await fireEvent.click(screen.getByRole("button", { name: "Finish setup" }));
    await settle();
    expect(localStorage.getItem(SETUP_PENDING_KEY)).toBeNull();
    expect(mounted.url()).toBe("/projects");
  });
});
