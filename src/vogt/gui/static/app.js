// The GUI, consuming only the public REST API (FR-U1, FR-U2).
//
// Every request in this file goes through `call()`, and `call()` builds its
// URL from `API_BASE` and one entry in `ROUTES`. That is not a style rule: it
// is what makes "nothing the GUI does is absent from the API" checkable. A
// test reads this file and asserts every `/api/...` literal in it resolves to
// a registered operation, so a view that quietly grew its own endpoint fails
// the build rather than the review.
//
// Buildless on purpose — see the package docstring in `vogt/gui/__init__.py`.

// The prefix every route below is written under. It is also where this GUI
// finds them when vogt-core serves it directly, at `/ui` on its own port.
const API_BASE = "/api";

// Behind the merged product's front door the same core answers under a
// different mount: the engine publishes the only port, proxies `/api/vogt` to
// the core, and serves this bundle at `/ui-legacy` until the PWA reaches
// parity with it (NFR-D11, FR-U9). The prefix is derived from where the page
// was served rather than configured, so one bundle works both ways and
// neither deployment can be given the other's setting by mistake.
//
// ROUTES keeps its `/api/...` literals either way. They name operations, the
// parity test resolves them against the registry, and only the mount point in
// front of them moves — which is what `call()` rewrites.
const FRONT_DOOR_GUI = "/ui-legacy";
const FRONT_DOOR_API = "/api/vogt";
const API_ROOT = window.location.pathname.startsWith(FRONT_DOOR_GUI)
  ? FRONT_DOOR_API
  : API_BASE;

// One entry per operation this GUI reads. Names match the operation registry.
const ROUTES = {
  status: "/api/status",
  "project.list": "/api/projects",
  "project.brief": "/api/projects/brief",
  backlog: "/api/backlog",
  bugs: "/api/bugs",
  "drift.list": "/api/drift",
  deps: "/api/deps",
  "audit.list": "/api/audit",
  notifications: "/api/notifications",
  // The one mutating operation this GUI names, and it is named only because
  // the import view collects a reason the *user* typed (FR-U3). The rule the
  // GUI keeps is not "never write" but "never write a reason nobody meant":
  // a button cannot type one, a form with a required field can.
  "project.import": "/api/projects/import",
};

const TOKEN_KEY = "vogt.token";

// -- transport --------------------------------------------------------------

/** Call one registered operation. The only way this GUI reaches the server. */
async function call(operation, params = {}, method = "GET") {
  const path = ROUTES[operation];
  if (!path) throw new Error(`no route for ${operation}`);
  if (!path.startsWith(API_BASE)) throw new Error(`${path} is not under the API`);

  const url = new URL(API_ROOT + path.slice(API_BASE.length), window.location.origin);
  if (method === "GET") {
    for (const [key, value] of Object.entries(params)) {
      if (value === undefined || value === null || value === "") continue;
      for (const one of Array.isArray(value) ? value : [value]) {
        url.searchParams.append(key, one);
      }
    }
  }

  const headers = { accept: "application/json" };
  const token = sessionStorage.getItem(TOKEN_KEY);
  // Sent as a header, never in the URL: a token in a query string ends up in
  // logs, proxies and browser history (FR-S7).
  if (token) headers.authorization = `Bearer ${token}`;
  if (method !== "GET") headers["content-type"] = "application/json";

  const response = await fetch(url, {
    headers,
    method,
    body: method === "GET" ? undefined : JSON.stringify(params),
  });
  const body = await response.json().catch(() => null);
  if (!response.ok) {
    const error = body && body.error ? body.error : {};
    throw new ApiError(
      error.message || `${operation} failed (${response.status})`,
      response.status,
      error.code,
    );
  }
  return body;
}

class ApiError extends Error {
  constructor(message, status, code) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

// -- rendering helpers ------------------------------------------------------

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = value;
    else node.setAttribute(key, value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(child));
  }
  return node;
}

/**
 * Freshness, on every aggregating view (FR-U2, FR-V4).
 *
 * Rendered even when everything is fine, because the value of the line is
 * that an empty answer and a stale answer stop looking alike. An inbox with
 * no drift in it is reassuring only if something has looked recently.
 */
function freshness(state) {
  if (!state) return el("p", { class: "freshness unknown" }, "freshness: not reported");
  const status = state.status || "never_swept";
  const parts = [];
  if (status === "never_swept") {
    parts.push("nothing has been swept yet — this is 'not collected', not 'nothing found'");
  } else {
    parts.push(`evidence is ${describeAge(state.age_seconds)} old at its oldest`);
    if (status === "partial") parts.push("at least one collector did not complete");
  }
  if (state.detail) parts.push(state.detail);

  const banner = el("p", { class: `freshness ${status}` }, parts.join(" · "));
  const collectors = Object.entries(state.collectors || {});
  if (collectors.length) {
    banner.append(
      el(
        "span",
        { class: "collectors" },
        collectors.map(([name, age]) => el("span", { class: "collector" }, `${name}: ${age}`)),
      ),
    );
  }
  return banner;
}

function describeAge(seconds) {
  if (seconds === null || seconds === undefined) return "an unknown time";
  if (seconds < 90) return `${seconds}s`;
  if (seconds < 5400) return `${Math.round(seconds / 60)}m`;
  if (seconds < 172800) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86400)}d`;
}

/** Trust state, on every item that has one (FR-U2, FR-D1). */
function trust(state) {
  const value = state || "unverified";
  return el("span", { class: `trust trust-${value}`, title: `trust: ${value}` }, value);
}

function table(headings, rows) {
  return el(
    "table",
    {},
    el("thead", {}, el("tr", {}, headings.map((h) => el("th", { text: h })))),
    el("tbody", {}, rows.length ? rows : el("tr", {}, el("td", { colspan: headings.length }, "nothing here"))),
  );
}

function link(hash, label) {
  return el("a", { href: `#${hash}` }, label);
}

function when(value) {
  if (!value) return "—";
  const at = new Date(value);
  return Number.isNaN(at.valueOf()) ? String(value) : at.toISOString().replace("T", " ").slice(0, 19);
}

function counts(map) {
  const entries = Object.entries(map || {});
  if (!entries.length) return "—";
  return entries.map(([key, n]) => `${key}: ${n}`).join(", ");
}

// -- views ------------------------------------------------------------------

async function projectsView() {
  const [listed, status] = await Promise.all([call("project.list", { limit: 200 }), call("status")]);
  const projects = listed.projects || [];
  return el(
    "section",
    {},
    el("h2", {}, "Projects"),
    el(
      "p",
      { class: "muted" },
      `instance ${status.instance_id} · revision ${status.revision} · ${projects.length} project(s)`,
    ),
    table(
      ["Project", "Slug", "Repo", "Write-back"],
      projects.map((project) =>
        el(
          "tr",
          {},
          el("td", {}, link(`/project/${project.slug}`, project.name)),
          el("td", { class: "mono" }, project.slug),
          el("td", {}, project.repo_url ? el("a", { href: project.repo_url }, project.repo_url) : "—"),
          el("td", {}, project.write_back || "none"),
        ),
      ),
    ),
  );
}

async function projectView(slug) {
  const brief = await call("project.brief", { slug });
  const project = brief.project || {};
  return el(
    "section",
    {},
    el("h2", {}, project.name || slug),
    freshness(brief.freshness),
    el(
      "dl",
      { class: "facts" },
      fact("Open work", String(brief.open_work)),
      fact("Open bugs", String(brief.open_bugs)),
      fact("By state", counts(brief.by_state)),
      fact("By kind", counts(brief.by_kind)),
      fact("Declared version", brief.declared_version || "not declared"),
      fact("Observed version", brief.observed_version || "not collected"),
      fact("Versions agree", brief.version_matches === null ? "not collected" : String(brief.version_matches)),
      fact("Compliance", `${brief.compliance_status} (checked ${when(brief.compliance_checked_at)})`),
      fact("CI", (brief.ci_status && brief.ci_status.status) || "not collected"),
    ),
    el("h3", {}, "Top backlog"),
    itemsTable(brief.top_backlog || []),
    el("p", {}, link(`/deps/${slug}`, "dependency graph →"), " ", link(`/drift?project=${slug}`, "drift →")),
  );
}

function fact(term, value) {
  return el("div", { class: "fact" }, el("dt", { text: term }), el("dd", { text: value }));
}

function itemsTable(items) {
  return table(
    ["Ref", "Title", "Kind", "State", "Pri", "Trust", "Project", "Updated"],
    items.map((item) =>
      el(
        "tr",
        {},
        el("td", { class: "mono" }, item.source_url ? el("a", { href: item.source_url }, item.ref) : item.ref),
        el("td", {}, item.title),
        el("td", {}, item.kind),
        el("td", {}, item.state),
        el("td", {}, item.priority),
        el("td", {}, trust(item.trust_state)),
        el("td", {}, item.project_slug ? link(`/project/${item.project_slug}`, item.project_slug) : "—"),
        el("td", {}, when(item.updated_at)),
      ),
    ),
  );
}

async function rankedView(operation, heading, query) {
  const result = await call(operation, { limit: 100, ...query });
  return el(
    "section",
    {},
    el("h2", {}, heading),
    freshness(result.freshness),
    el(
      "p",
      { class: "muted" },
      `${result.items.length} shown of ${result.total_considered} considered · ` +
        `${result.declared} declared, ${result.observed} observed, ${result.suppressed} suppressed`,
    ),
    itemsTable(result.items || []),
  );
}

async function driftView(query) {
  const result = await call("drift.list", { limit: 100, status: "open", ...query });
  const gated = result.human_gated || {};
  return el(
    "section",
    {},
    el("h2", {}, "Drift inbox"),
    freshness(result.freshness),
    table(
      ["Kind", "Subject", "Status", "Proposed", "Raised", "Gate"],
      (result.proposals || []).map((proposal) =>
        el(
          "tr",
          {},
          el("td", { class: "mono" }, proposal.kind),
          el("td", { class: "mono small" }, proposal.subject_id || "—"),
          el("td", {}, proposal.status),
          el("td", { class: "mono small" }, JSON.stringify(proposal.proposed_change || {})),
          el("td", {}, when(proposal.opened_at)),
          // Why a human must answer this one, quoted from the API rather than
          // restated here — two copies of a policy is one copy too many.
          el("td", { class: "small" }, gated[proposal.kind] || "auto-acceptable"),
        ),
      ),
    ),
    // Read-only by design: resolving a proposal is a write, and every write
    // carries a reason its author typed. A one-click "accept" in a browser is
    // how reasons become "accepted via GUI", which is not a reason.
    el(
      "p",
      { class: "muted" },
      "Resolving a proposal is a write and needs a reason: ",
      el("code", { text: "vogt drift resolve <id> --resolution accepted --reason '…'" }),
    ),
  );
}

async function depsView(slug) {
  const result = await call("deps", { project: slug });
  return el(
    "section",
    {},
    el("h2", {}, `Dependencies · ${result.project}`),
    freshness(result.freshness),
    el("p", { class: "muted" }, `${result.unresolved} reference(s) point outside the estate`),
    el("h3", {}, "References out"),
    depsTable(result.references_out || []),
    el("h3", {}, "Referenced by"),
    depsTable(result.referenced_by || []),
  );
}

function depsTable(refs) {
  // The columns are what a dependency reference actually carries. Ecosystem
  // and constraint were here once and never had a source: r2 removed
  // lockfiles and resolved versions from the product (FR-D1), so a reference
  // is a path or a git URL between projects and nothing more. The columns
  // rendered an em dash on every row for every estate — which reads as
  // "not collected", the one thing this GUI is careful never to say by
  // accident.
  return table(
    ["From", "To", "Kind", "Reference", "Resolved"],
    refs.map((ref) =>
      el(
        "tr",
        {},
        el(
          "td",
          {},
          ref.from_project_slug
            ? link(`/project/${ref.from_project_slug}`, ref.from_project_slug)
            : "—",
        ),
        el(
          "td",
          {},
          ref.to_project_slug
            ? link(`/project/${ref.to_project_slug}`, ref.to_project_slug)
            : "—",
        ),
        el("td", {}, ref.ref_kind || "—"),
        el("td", { class: "mono small" }, ref.raw_target || "—"),
        // An unresolved reference is a real answer: the thing is depended on
        // and is not in the estate. Blank would read as a missing field.
        el("td", {}, ref.to_project_id ? "in estate" : "outside the estate"),
      ),
    ),
  );
}

async function auditView() {
  const result = await call("audit.list", { limit: 200 });
  return el(
    "section",
    {},
    el("h2", {}, "Audit"),
    el("p", { class: "muted" }, "Every write, with the reason its author gave."),
    table(
      ["At", "Operation", "Entity", "Actor", "Reason"],
      (result.records || []).map((record) =>
        el(
          "tr",
          {},
          el("td", {}, when(record.at)),
          el("td", { class: "mono" }, record.operation),
          el("td", { class: "mono" }, `${record.entity_kind}:${record.entity_id}`),
          el("td", {}, record.actor_id),
          el("td", {}, record.reason),
        ),
      ),
    ),
  );
}

// -- the forge inbox (FR-N3, FR-U3) -----------------------------------------

/**
 * What GitHub is trying to say about the registered projects.
 *
 * Deliberately not folded into any other view. These are observations about
 * somebody else's system, they belong to the token's account rather than to
 * the person reading, and the response says so — which is why `scope` is
 * rendered rather than dropped.
 */
async function inboxView(query) {
  const params = { limit: 100 };
  if (query.project) params.project = query.project;
  if (query.reason) params.reason = query.reason;
  if (query.unread === "1") params.unread_only = true;
  const data = await call("notifications", params);

  const filters = el(
    "p",
    { class: "filters" },
    link("/inbox", "all"),
    link("/inbox?unread=1", "unread only"),
    ...Object.entries(data.by_reason || {}).map(([reason, count]) =>
      link(`/inbox?reason=${encodeURIComponent(reason)}`, `${reason} (${count})`),
    ),
  );

  return el(
    "section",
    {},
    el("h2", {}, "Inbox"),
    el("p", { class: "scope" }, data.scope),
    freshness(data.freshness),
    filters,
    data.detail ? el("p", { class: "empty" }, data.detail) : null,
    el(
      "table",
      {},
      el(
        "thead",
        {},
        el(
          "tr",
          {},
          el("th", {}, "When"),
          el("th", {}, "Project"),
          el("th", {}, "Reason"),
          el("th", {}, "Subject"),
        ),
      ),
      el(
        "tbody",
        {},
        (data.notifications || []).map((entry) =>
          el(
            "tr",
            { class: entry.unread ? "unread" : null },
            el("td", {}, when(entry.updated_at || entry.observed_at)),
            el("td", {}, entry.project_slug || entry.repo || "—"),
            el("td", { class: "mono" }, entry.reason || "unknown"),
            el(
              "td",
              {},
              entry.url ? el("a", { href: entry.url, target: "_blank", rel: "noreferrer" }, entry.title || entry.thread) : (entry.title || entry.thread),
              entry.subject_type ? el("span", { class: "kind" }, entry.subject_type) : null,
            ),
          ),
        ),
      ),
    ),
  );
}

// -- importing a repository (FR-P6, FR-U3) ----------------------------------

/**
 * The one view that writes, and the reason it is allowed to.
 *
 * `reason` is a required field the user types, so the audit row records why
 * a human imported this repository rather than "via GUI". There is no
 * repository list, picker or search here and there must never be one: that
 * is the registration-candidate listing r3 removed (FR-G15).
 */
async function importView() {
  const form = el("form", { class: "import" });
  const repo = el("input", { name: "repo", required: "required", placeholder: "owner/name or a GitHub URL" });
  const name = el("input", { name: "name", placeholder: "Display name (defaults to the repository name)" });
  const reason = el("input", { name: "reason", required: "required", placeholder: "Why are you importing this? (audited)" });
  const consolidate = el("input", { name: "consolidate", type: "checkbox", checked: "checked" });
  const outcome = el("div", { class: "outcome" });

  form.append(
    el("label", {}, "Repository", repo),
    el("label", {}, "Name", name),
    el("label", {}, "Reason", reason),
    el("label", { class: "check" }, consolidate, "Read existing issues, PRs and releases (changes nothing upstream)"),
    el("button", { type: "submit" }, "Import"),
  );

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    outcome.replaceChildren(el("p", { class: "loading" }, "Cloning…"));
    try {
      const result = await call(
        "project.import",
        {
          repo: repo.value.trim(),
          name: name.value.trim() || undefined,
          reason: reason.value.trim(),
          consolidate: consolidate.checked,
        },
        "POST",
      );
      outcome.replaceChildren(
        el("p", { class: "ok" }, `${result.project.name} imported to ${result.root_path}`),
        result.detail ? el("p", { class: "note" }, result.detail) : null,
        link(`/project/${result.project.slug}`, "Open the project"),
      );
    } catch (error) {
      outcome.replaceChildren(errorView(error));
    }
  });

  return el(
    "section",
    {},
    el("h2", {}, "Import a repository"),
    el(
      "p",
      { class: "note" },
      "Name the repository. Vogt clones it, registers it, and reads what is " +
        "already on GitHub — it changes nothing upstream (FR-B3).",
    ),
    form,
    outcome,
  );
}

// -- routing ----------------------------------------------------------------

const NAV = [
  ["/projects", "Projects"],
  ["/backlog", "Backlog"],
  ["/bugs", "Bugs"],
  ["/drift", "Drift"],
  ["/inbox", "Inbox"],
  ["/audit", "Audit"],
  ["/import", "Import"],
];

function parse(hash) {
  const [path, search] = (hash.replace(/^#/, "") || "/projects").split("?");
  const parts = path.split("/").filter(Boolean);
  return { parts, query: Object.fromEntries(new URLSearchParams(search || "")) };
}

async function render() {
  const view = document.getElementById("view");
  const { parts, query } = parse(window.location.hash);
  view.replaceChildren(el("p", { class: "loading" }, "Loading…"));

  try {
    let section;
    switch (parts[0]) {
      case "project":
        section = await projectView(parts[1]);
        break;
      case "backlog":
        section = await rankedView("backlog", "Backlog", query);
        break;
      case "bugs":
        section = await rankedView("bugs", "Bugs", query);
        break;
      case "drift":
        section = await driftView(query);
        break;
      case "deps":
        section = await depsView(parts[1]);
        break;
      case "inbox":
        section = await inboxView(query);
        break;
      case "import":
        section = await importView();
        break;
      case "audit":
        section = await auditView();
        break;
      default:
        section = await projectsView();
    }
    view.replaceChildren(section);
  } catch (error) {
    view.replaceChildren(errorView(error));
  }

  for (const anchor of document.querySelectorAll("#nav a")) {
    anchor.classList.toggle("current", anchor.getAttribute("href") === `#/${parts[0] || "projects"}`);
  }
}

function errorView(error) {
  const needsToken = error instanceof ApiError && (error.status === 401 || error.status === 403);
  return el(
    "section",
    { class: "error" },
    el("h2", {}, needsToken ? "Not authorised" : "Something went wrong"),
    el("p", {}, error.message),
    needsToken
      ? el("p", {}, "Set a bearer token with the ", el("b", {}, "token"), " button, then reload.")
      : null,
  );
}

function promptForToken() {
  const current = sessionStorage.getItem(TOKEN_KEY) ? "(a token is set)" : "(none set)";
  const entered = window.prompt(`Bearer token ${current}. Leave blank to clear.`, "");
  if (entered === null) return;
  // sessionStorage, not localStorage: the token goes away with the tab. A
  // credential that outlives the session by default is a credential somebody
  // forgets they granted.
  if (entered) sessionStorage.setItem(TOKEN_KEY, entered);
  else sessionStorage.removeItem(TOKEN_KEY);
  render();
}

function start() {
  const nav = document.getElementById("nav");
  nav.replaceChildren(...NAV.map(([hash, label]) => link(hash, label)));
  document.getElementById("token-button").addEventListener("click", promptForToken);
  window.addEventListener("hashchange", render);

  call("status")
    .then((status) => {
      document.getElementById("footer").replaceChildren(
        el("span", {}, `vogt ${status.vogt_version} · schema ${status.declared_schema_version}`),
        el("span", {}, `as ${status.principal}`),
      );
    })
    .catch(() => {
      document.getElementById("footer").replaceChildren(el("span", {}, "not connected"));
    });

  render();
}

start();
