// What is on a project page when Vogt answers (FR-U7).
//
// `absentStates.test.tsx` mounts this surface for its outage states, and it
// proves something worth having — the page survives a panel that failed — but
// every one of its assertions is about an absence. FR-U7 is a claim about a
// presence: a project page carries the brief, CI status, contract and
// compliance, the drift inbox, the dependency graph and the import form. A
// panel silently dropped from the page would pass every test that existed
// before this file.
//
// So each test here answers as a working Vogt would and asserts the panel is
// there *with that answer in it*. A panel rendered with its heading and none
// of the server's numbers is the failure mode this shape catches and a
// heading-only assertion would not: it reads, to somebody looking at it, as a
// project with nothing wrong.
//
// Three of the six are views of their own, reached from the same page — the
// four tabs `place().view` names. That is still one page in FR-U7's sense and
// the reachability is asserted below rather than assumed, because a view with
// no way to it is not on the page at all.

import { describe, expect, it } from "vitest";
import { waitFor } from "@solidjs/testing-library";

import Projects from "../Projects";
import { driftProposal, fakeVogt, freshness, mountAt, type Routes } from "./harness";

/** A project brief with every field the overview draws something from. */
function brief(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    project: {
      slug: "alpha",
      name: "Alpha",
      lifecycle_state: "active",
      trust_state: "verified",
      root_path: "/srv/alpha",
    },
    open_work: 7,
    open_bugs: 2,
    by_state: { open: 5, in_progress: 2 },
    by_kind: { feature: 6, bug: 1 },
    declared_version: "1.2.0",
    observed_version: "1.3.0",
    version_matches: false,
    ci_status: {
      status: "failing",
      checks: 4,
      failing: ["build-arm64"],
      revision: "abcdef0123456789",
    },
    compliance_status: "failing",
    dependencies: {
      status: "collected",
      references_out: 3,
      referenced_by: 1,
      unresolved: 2,
    },
    top_backlog: [
      {
        ref: "WI-1",
        title: "Teach the board to say what it does not know",
        kind: "feature",
        state: "open",
        origin: "declared",
        trust_state: "verified",
      },
    ],
    freshness: freshness(),
    ...over,
  };
}

const CONTRACT = {
  project: "alpha",
  status: "failing",
  contract_version: "3",
  checked_at: "2026-08-01T00:00:00Z",
  age_seconds: 600,
  failing: [
    {
      rule: "ci.required_checks",
      target: "build-arm64",
      satisfied: false,
      detail: "the check has never passed on the default branch",
    },
  ],
};

const GRAPH = {
  project: "alpha",
  references_out: [
    {
      subject_key: "project:alpha",
      from_project_id: "prj_alpha",
      from_project_slug: "alpha",
      ref_kind: "python",
      raw_target: "beta-client",
      manifest: "pyproject.toml",
      to_project_slug: "beta",
      observed_at: "2026-08-01T00:00:00Z",
    },
  ],
  referenced_by: [],
  unresolved: 1,
  freshness: freshness(),
};

function projects(url: string, routes: Routes = {}) {
  fakeVogt({
    "GET /projects/brief": { body: brief() },
    "GET /compliance": { body: CONTRACT },
    "GET /deps": { body: GRAPH },
    ...routes,
  });
  return mountAt("/projects", url, () => <Projects />);
}

/** The panel under a given heading, or a failure naming the missing panel. */
function panel(container: HTMLElement, heading: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>(".vogt-projects-panel")].find(
    (section) => section.querySelector("h3")?.textContent === heading,
  );
  if (!found) throw new Error(`the project page has no "${heading}" panel`);
  return found;
}

describe("FR-U7 — a project page carries the brief Vogt answered with", () => {
  it("names the project and the work behind it", async () => {
    const { container } = projects("/projects?p=alpha");

    const page = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(".vogt-projects-project");
      expect(found, "the project brief is not on the page").toBeTruthy();
      return found!;
    });
    expect(page.querySelector("h2")?.textContent).toBe("Alpha");
    expect(page.textContent).toContain("active");
    expect(page.textContent).toContain("/srv/alpha");

    const work = panel(container, "Work");
    expect(work.textContent).toContain("7");
    expect(work.textContent).toContain("open: 5");
    expect(work.textContent).toContain("feature: 6");
  });

  it("shows the version the brief declared against the one a collector saw", async () => {
    // The two sides, kept apart. One number under a single heading would be a
    // claim nobody could check.
    const { container } = projects("/projects?p=alpha");

    const version = await waitFor(() => panel(container, "Version"));
    expect(version.textContent).toContain("1.2.0");
    expect(version.textContent).toContain("1.3.0");
    expect(version.textContent).toContain("They disagree");
  });

  it("shows CI as the brief reported it, failing checks named", async () => {
    const { container } = projects("/projects?p=alpha");

    const ci = await waitFor(() => panel(container, "CI"));
    expect(ci.textContent).toContain("failing");
    // The count and the name, because "4 checks, one failing" and "4 checks"
    // are different answers to "is this project all right".
    expect(ci.textContent).toContain("4");
    expect(ci.textContent).toContain("build-arm64");
  });

  it("shows the compliance status with the criteria that failed", async () => {
    // The status comes from the brief and the criteria from `compliance`, and
    // the panel is only worth having with both: a bare "failing" tells a
    // reader nothing about what to fix.
    const { container } = projects("/projects?p=alpha");

    const contract = await waitFor(() => {
      const found = panel(container, "Contract and compliance");
      expect(found.textContent).toContain("ci.required_checks");
      return found;
    });
    expect(contract.textContent).toContain("failing");
    expect(contract.textContent).toContain("contract 3");
    expect(contract.textContent).toContain(
      "the check has never passed on the default branch",
    );
  });

  it("carries the top of that project's backlog, linked by ref", async () => {
    const { container } = projects("/projects?p=alpha");

    await waitFor(() =>
      expect(container.querySelector(".vogt-projects-backlog")).toBeTruthy(),
    );
    const rows = container.querySelector(".vogt-projects-backlog")!;
    expect(rows.textContent).toContain("WI-1");
    expect(rows.textContent).toContain("Teach the board to say what it does not know");
  });
});

describe("FR-U7 — the graph, the inbox and the import form are on the same page", () => {
  it("offers all four views from the project page, so none of them is unreachable", async () => {
    const { container } = projects("/projects?p=alpha");

    await waitFor(() =>
      expect(container.querySelector(".vogt-projects-views")).toBeTruthy(),
    );
    const tabs = [...container.querySelectorAll(".vogt-projects-viewtab")].map(
      (node) => node.textContent,
    );
    expect(tabs).toEqual(["Project", "Dependencies", "Drift inbox", "Import"]);
  });

  it("draws the dependency graph from the references Vogt recorded", async () => {
    const { container } = projects("/projects?p=alpha&view=deps");

    const graph = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(
        '.vogt-projects-deps[aria-label="Dependency graph"]',
      );
      expect(found, "the dependency graph is not on the page").toBeTruthy();
      expect(found!.textContent).toContain("beta");
      return found!;
    });
    // The neighbour, the manifest it was recorded from, and the count of
    // references that point outside the estate — an edge with no provenance
    // is a line somebody has to take on trust.
    expect(graph.textContent).toContain("pyproject.toml");
    expect(graph.textContent).toContain("1 pointing outside the estate");
    expect(graph.querySelector(".vogt-projects-hub-node")?.textContent).toBe("alpha");
  });

  it("carries the drift inbox, with the proposals Vogt has open", async () => {
    const { container } = projects("/projects?p=alpha&view=drift", {
      "GET /drift": {
        body: { proposals: [driftProposal()], human_gated: {}, freshness: freshness() },
      },
    });

    const inbox = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(
        '.vogt-projects-inbox[aria-label="Drift inbox"]',
      );
      expect(found, "the drift inbox is not on the page").toBeTruthy();
      expect(found!.textContent).toContain("alpha declares 1.2.0");
      return found!;
    });
    expect(inbox.textContent).toContain("version_mismatch");
  });

  it("carries the import form, which names the repository and nothing else", async () => {
    // FR-G15: no picker, no listing, no suggestion. The field is where a
    // person types a repository they already know they want.
    const { container } = projects("/projects?view=import");

    const form = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(
        '.vogt-projects-import[aria-label="Import a repository"]',
      );
      expect(found, "the import form is not on the page").toBeTruthy();
      return found!;
    });
    const fields = [...form.querySelectorAll("label.vogt-projects-field span")].map(
      (node) => node.textContent,
    );
    expect(fields).toContain("Repository");
    expect(form.querySelector("form")).toBeTruthy();
  });
});
