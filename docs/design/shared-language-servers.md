# Shared language servers: Python, Rust and Java

Status: **proposal, not current behaviour.** Nothing here ships yet. Adopting it
changes a recorded decision: `docs/ENGINE.md` §8 says the engine has "no
editor logic, terminal rendering, or language servers server-side". If the
operator accepts phases 2–4, that bullet and §4 ("Agent-facing MCP servers")
have to be rewritten to match, and this note is retired into them.

Tracking: initiative *Shared language servers (Python, Rust, Java)*, WI-885–WI-890. The
child work items are listed in §10.

## 1. Goal

Make one capability, available to every project registered in Vogt, that
provides code intelligence for **Python**, **Rust** and **Java**:
definition, references, hover and type information, symbols, and
diagnostics. These are the consumers:

1. **Agent sessions.** Claude Code and Codex running in engine PTYs.
2. **The PWA editor** (Monaco), optionally and last.

The capability must work for any registered project without per-repo setup.
It must keep memory bounded when many sessions and worktrees are open, and it
must not quietly widen what untrusted project code can do.

## 2. What exists today (evidence)

| Area | Finding | Where |
|---|---|---|
| Recorded non-goal | "no language servers in the client", "no … language servers server-side" | `docs/ENGINE.md` §8 (≈L2096–2100) |
| Rust, agents | Two pieces exist. `rust-analyzer` is a rustup component on floating `stable`, and `rust-analyzer-mcp` is installed with `cargo install`. The wrapper `vogt-rust-analyzer-mcp` walks up to the nearest `Cargo.toml`. It must be **registered by hand** for each agent. Every agent starts its own copy, so nothing is shared. | `engine/Dockerfile.pod:249-261`, `engine/deploy/rust-analyzer-mcp.sh`, `engine/Dockerfile:426`, `docs/ENGINE.md` §4 |
| Rust version drift | `rust-analyzer` is unpinned and moves with the weekly pod-base rebuild. Running it through the rustup proxy fails for a project whose `rust-toolchain.toml` pins a toolchain that lacks the component (egressy#8). | `Dockerfile.pod:249-253`; work item `gh:…/egressy#8` |
| Python | `uv`, `ruff` 0.16.8 and `pytest` are installed system-wide. There is **no** Python language server (no pyright, basedpyright, pylsp or ty). Python 3.14 is the system interpreter. The estate image adds 3.12 under `/opt/uv-python`. | `Dockerfile.pod:374-379`; estate `Dockerfile:138-142` |
| Java | OpenJDK 21 headless and Gradle are present. Maven is not. There is **no jdtls**. | `Dockerfile.pod:78-87, 296-307` |
| PWA editor | Monaco 0.56, core plus tokenizers only. There is no LSP client library (`monaco-languageclient`, `vscode-ws-jsonrpc`) and no tree-sitter. | `web/package.json`, `web/src/monaco.ts` |
| Claude Code | Pinned to 2.1.283 (`engine/agent-versions.env`). It has a native LSP tool fed by plugins that declare `lspServers` in `plugin.json` or `.lsp.json`. The fields are `command`, `args`, `extensionToLanguage`, `env`, `initializationOptions`, `settings`, `workspaceFolder`, `startupTimeout`, `restartOnCrash`, `maxRestarts` and `diagnostics`. `requestTimeout` needs 2.1.288 or later. The server binary must be on `PATH` or given as an absolute path. Each Claude process spawns **its own** stdio server. Diagnostics are injected into the agent's context after each Edit or Write. | code.claude.com plugins reference; CLI bundle strings |
| Official plugins | The `claude-plugins-official` marketplace is already cloned on this box. It has `rust-analyzer-lsp` (`rust-analyzer`), `pyright-lsp` (`pyright-langserver --stdio`) and `jdtls-lsp` (`jdtls`, `startupTimeout` 120000). None are installed. | `~/.claude/plugins/marketplaces/claude-plugins-official/.claude-plugin/marketplace.json` |
| Codex | Pinned to 0.149.1. It has **no native LSP**: there are no LSP strings in the binary, and upstream requests openai/codex#8745 and #8633 are open. The only route is an MCP bridge. | `engine/agent-versions.env`; binary strings |
| MCP provisioning | `vogt-agent-auth run` calls `mcp-bootstrap.sh` on every launch, which registers `vogt` for Claude, Codex and opencode. Every other MCP server is registered by hand. The engine itself writes no agent settings. | `engine/deploy/agent-auth.sh:524`, `engine/deploy/mcp-bootstrap.sh` |
| Sessions | The session cwd is the project's `root_path`. The env is cleared and secrets are stripped (`*TOKEN*`, `*SECRET*`, `INFISICAL_*`, …). The engine creates **no worktrees**. Concurrent sessions on one project share its checkout. Agents create worktrees themselves, as sibling dirs or under `.claude/worktrees/`. | `engine/server/src/sessions.rs:83-236`, `pty.rs:312-333, 495-540`, `src/vogt/application/services/sessions.py:478-511` |
| Process supervision | The engine has no long-lived child supervisor besides PTYs. It uses no cgroups, rlimits, seccomp or namespaces. The pod is the trust boundary: agents run `--dangerously-skip-permissions` or Codex's full-access wrapper, as uid 1000 with sudo. | `pty.rs:223-247`; `engine/deploy/codex-full-access.sh` |
| Deployment | The stack has no `mem_limit` and no CPU limit. The volumes are `engine-home:/home/sprooty`, `vogt-data` and `engine-agent-clis`. `/opt/rust` (CARGO_HOME, including the **cargo registry**) is in the image layer, so the registry is re-downloaded after every redeploy. Anything the image puts under `/home/sprooty` is shadowed at runtime. | `deploy/stack.compose.yml:52, 158-185`; `Dockerfile.pod:67-69, 233-244` |
| Auth surface | Engine file routes resolve against one global `workspace_root`. WebSocket attach uses first-frame auth and needs the `Sessions` capability. Scoping is by capability, not per project. | `engine/server/src/workspace_path.rs`, `ws.rs:224-338`, `auth.rs:515-600` |
| Prior art (other repo) | msp-klaudia has a Go LSP layer: a per-session pool with one server per language, rooted at the cwd. It exposes definition, references, hover, symbols, rename and diagnostics as tools, and appends diagnostics to Edit/Write results. | `msp-klaudia/internal/lsp/{pool,client,detect,ops}.go`; msp-klaudia#151, #157 |

### The estate's shape, as measured on 2026-10-05

- **64 registered projects.**
  - About 45 have a root `Cargo.toml`, mostly small single-crate libraries (arr-*, nzb-*, librtbit-*). The big Cargo workspaces are vogt/engine, komodo and rustnzb.
  - About 8 have Gradle builds, and these are mostly Android or Capacitor shells: vogt/mobile, farmeggs, aidevenv, myaiagent, wifimap-rs, transl, javascan and aus-price-scanner.
  - The Python projects are all uv-managed with `uv.lock`, and several declare `[tool.mypy]`.
  - The only large pure-Java tree on disk is `ping-legacy` (about 230k `.java` files), and it is **not** registered.
- **Worktrees multiply roots.** `git worktree list` in vogt shows **99 worktrees**, each a separate root. Most have **no `.venv`**.
- **The host has headroom, but it is already under pressure.**
  - 24 CPUs and 94 GiB RAM; **swap is 31/31 GiB used**.
  - The pod has no cgroup limit.
  - Idle Gradle and Kotlin daemons alone held about 7.7 GiB RSS (one GradleDaemon at 5.3 GiB).
- **Python servers measured on `vogt/src`** (CLI runs, peak RSS):

  | Server | Peak RSS | Wall time | Diagnostics | `.venv` found? |
  |---|---|---|---|---|
  | basedpyright 1.1.414 | about 657 MB | 4.4 s | 68 errors and 1,734 warnings under its default "recommended" rules | Yes: it picked `vogt/.venv` (Python 3.12) with no config, and showed 0 missing imports |
  | ty 0.0.84 | about 154 MB | 0.1 s | 24 | Yes: 0 unresolved imports |

## 3. Where the pieces live

| Piece | Home | Why |
|---|---|---|
| Server binaries (rust-analyzer, basedpyright, jdtls) | **pod-base image** (`engine/Dockerfile.pod`), under `/opt/…`, pinned and checksummed | The servers need the same toolchains, venvs and checkouts the agents see. Interpreter paths inside `.venv` are absolute, so a sidecar would need the whole toolchain plus the home volume. `/opt` survives the home bind-mount. |
| `vogt-lsp` launcher (root and interpreter resolution, per-root state dirs, sanitized env, per-language defaults) | **stack image** (`engine/Dockerfile`), in `/usr/local/bin`, next to `vogt-rust-analyzer-mcp` | It is Vogt-owned glue. One entry point serves every consumer. |
| Sharing hub (one instance per root, idle GC, memory budget) | **engine**: phase 2 prototypes it with an external multiplexer, and the engine later supervises it | Process supervision is the engine's job per AGENTS.md. The Python core never runs processes. |
| Agent wiring | `mcp-bootstrap.sh`: a Vogt Claude Code plugin plus a Codex MCP bridge | Bootstrap already runs on every launch and owns agent registration. |
| PWA | engine WebSocket route `/api/lsp` and a thin Monaco client | It reuses the attach first-frame auth pattern. It is gated on decision D8. |

The Python core gets **no** new operation in phases 0–3. It is involved only
as the source of project roots (`project.list`), which the hub uses as an
allowlist and for optional prewarming.

```text
Claude Code ──LSP tool──▶ vogt-lsp <lang> (stdio client) ─┐
Codex ──MCP──▶ vogt-lsp mcp <lang> ───────────────────────┤──▶ hub ──▶ one server per (lang, root)
PWA Monaco ──WS /api/lsp──▶ engine ───────────────────────┘     (idle GC, LRU, RSS budget)
```

## 4. Lifecycle and multi-project handling

**Unit of sharing: one server per (language, canonical workspace root).**
Multi-root single servers are rejected:

- jdtls keeps one `-data` workspace per process.
- rust-analyzer's memory scales with the union of the roots it holds.
- pyright resolves one interpreter per execution environment.

Per-root instances keep each project's dependency graph and interpreter
isolated from the others.

**Root resolution** (done by `vogt-lsp`, not by the client):

1. Start from the file or cwd and walk up to the nearest marker:
   - Rust: `Cargo.toml`; prefer the outermost one that has `[workspace]`.
   - Python: `pyproject.toml`, `uv.lock`, `setup.py` or `pyrightconfig.json`.
   - Java: `settings.gradle*`, `pom.xml`, or `build.gradle*`.
2. Stop at the git toplevel and never climb above the engine's `workspace_root`.
3. Accept the root only if it lies inside a registered project's `root_path`
   **or inside a git worktree of one**. To check the second case, compare
   `git rev-parse --git-common-dir` with the registered checkout's.

Step 3 makes the registry the allowlist without making worktrees second-class.

**Concurrent sessions on one project** share one instance through the hub.
Two agents in different worktrees of the same repo have different roots, so
they get different instances. That is correct, because their code differs,
and it is why the budget below is essential.

**Idle shutdown and budget** (hub-enforced; numbers are proposals, see D7):

| Language | Idle timeout | Max concurrent | Per-instance knobs |
|---|---|---|---|
| Python (basedpyright) | 10 min | 8 | none needed; about 0.3–0.7 GB |
| Rust (rust-analyzer) | 20 min | 4 | `cachePriming.enable=false`, `cargo.targetDir=true` (builds go to `target/rust-analyzer`, so agent `cargo` builds are not blocked on the build lock), `lru.capacity` left at default |
| Java (jdtls) | 10 min | 1–2 | `-Xmx2G`, `-XX:+UseSerialGC`, `java.import.gradle` daemon idle timeout shortened |

The hub has more rules beyond these limits:

- It samples `/proc/<pid>/status` VmRSS across its process trees and evicts
  the least recently used instance once a global budget is exceeded
  (proposal: 8 GiB).
- It never evicts an instance with an in-flight request.
- A client that reconnects to an evicted root simply cold-starts it.
- Prewarming is off by default. It could become a per-project opt-in later.

**Without the hub**, which is phase 1 alone, each Claude or Codex process
spawns its own servers. That is acceptable for Python. Rust is acceptable only
for small crates. Java is not acceptable, so phase 1 ships jdtls only behind
the hub or with a hard cap (see D6).

**About lspmux.** lspmux (formerly ra-multiplex,
<https://codeberg.org/p2502/lspmux>) is the prototype hub.

- Its client acts like the server binary over stdio and pipes to a loopback
  TCP server. That server reuses one instance per workspace, rewriting request
  ids per client.
- Upstream documents that it **drops requests sent from the server to the
  client**. That is the gap the launcher's static settings must cover.
- `sunshowers/lspmux-rust-analyzer` uses exactly this to share rust-analyzer
  across Claude Code sessions. It is evidence the approach works, but it is
  self-described as personal-use quality.
- Python and Java through lspmux are **unverified**, and phase 2 must prove
  them.

**Multiplexer caveats** (they apply whether the hub is lspmux or engine-native):

- The hub answers `initialize` once per server and replays the result to later
  clients, so their capabilities are the union of what the first client
  negotiated.
- The hub must answer server-to-client requests such as
  `workspace/configuration` itself, from the launcher's per-language settings,
  so that a disconnecting client cannot stall the server.
- Unsaved-buffer `didChange` from two clients on the same URI conflicts.
  Agents write to disk, so this matters only once the PWA has dirty buffers.
  The hub should then give each client its own version stream and treat disk
  as truth for other clients.
- Published diagnostics are broadcast to every client attached to the root.

## 5. Per-language packaging and configuration

### Python

| Candidate | Navigation | Types | Footprint | Interpreter discovery | Fit |
|---|---|---|---|---|---|
| **basedpyright** | full | pyright engine | about 0.66 GB on vogt | finds `<root>/.venv` automatically (verified); honours `[tool.basedpyright]` and `pyrightconfig.json` | **Recommended default.** The PyPI wheel bundles Node (`nodejs-wheel`), so a `uv tool install` pins it with no npm. It adds Pylance-only features (inlay hints, semantic tokens) that plain pyright lacks. |
| pyright | full | same engine | similar | same | Viable. It needs npm and Node (v22 is present). It is what the official `pyright-lsp` plugin expects. |
| ty (`ty server`) | definition, hover, completions, diagnostics | own checker | about 0.15 GB, very fast | finds `.venv` (verified) | Still 0.0.x (pre-1.0). It is the strongest future option on memory. Keep it behind `VOGT_LSP_PYTHON=ty` and re-evaluate at 1.0. |
| pylsp (jedi) | good | weak | moderate | must be **installed into each project's venv**, or pointed at it, to see dependencies | Poor fit for a shared server, and its plugins (pylint, mypy) execute code. |
| ruff server | none (lint and format only) | none | tiny | not needed | Complementary only, for lint diagnostics. ruff is already in the image. Optional add-on (see D2). |

**Interpreter discovery order** in `vogt-lsp python`:

1. Project config wins: `venvPath`/`venv` in `pyrightconfig.json` or `[tool.basedpyright]`.
2. `<root>/.venv`, or `$UV_PROJECT_ENVIRONMENT` if the project sets it.
3. For a worktree without a venv, the **main checkout's** `.venv`, found through
   `--git-common-dir`. Dependencies are usually identical. See D5.
4. `uv python find --project <root>`, honouring `.python-version` and
   `requires-python`. Stdlib resolves, but third-party imports show as missing
   until the venv exists.
5. **Never** run `uv sync` implicitly. Building sdists runs arbitrary build
   backends (§7). The launcher prints the one-line command instead.

The launcher also **unsets `VIRTUAL_ENV`, `PYTHONPATH` and `UV_*` that leak
from the session**, so one project's activated venv cannot be applied to
another root through the hub.

**Diagnostics policy.** basedpyright's default "recommended" ruleset produced
1,734 warnings on vogt, and Claude Code pushes diagnostics into the agent's
context after every edit. Unless the project configures basedpyright or
pyright itself, the launcher sets `typeCheckingMode=standard` and reports
errors only. Projects that type-check with mypy (vogt, cadastre, msp-agent,
agent-harness, contextkeeper) will see some disagreement; see D3.

### Rust

- **Pin a standalone `rust-analyzer` release binary** at `/opt/rust-analyzer/bin`,
  checksummed with a Renovate `github-releases` comment, following the
  existing `ARG X_VERSION` / `X_SHA256` pattern. Invoke it by absolute path
  rather than through the rustup proxy. This avoids the egressy#8 failure,
  where a project's pinned toolchain lacks the component. A current
  rust-analyzer supports older toolchains through their `cargo` and
  `rust-src`.
- **rust-src** must exist for each pinned toolchain that a project selects. The
  launcher installs it on demand with `rustup component add rust-src
  --toolchain <tc>`. That is a write under `/opt/rust`, which is image-layer
  and lost on redeploy (see D9).
- **The cargo registry** currently lives in the image layer
  (`CARGO_HOME=/opt/rust/cargo`) and re-downloads after each redeploy.
  rust-analyzer's `cargo metadata` step pays that cost on the first open after
  a deploy. The proposal splits it: keep binaries in `/opt/rust/cargo/bin` on
  `PATH`, and move `CARGO_HOME`'s `registry/` and `git/` to the home volume
  through a symlink or by setting `CARGO_HOME` (D9).
- **Build-dir lock:** `rust-analyzer.cargo.targetDir=true` sends rust-analyzer's
  `cargo check` to `target/rust-analyzer`, so it never blocks an agent's
  `cargo build` or `cargo test`.
- **The existing `vogt-rust-analyzer-mcp` is superseded** by `vogt-lsp mcp rust`
  and kept as an alias for one release.

### Java

- **jdtls** is installed from the pinned Eclipse milestone tarball (sha256)
  into `/opt/jdtls`, with a `jdtls` launcher in `/usr/local/bin`. It runs on
  the image's OpenJDK 21, and jdtls needs 21 or later to run.
- A **per-root `-data` dir** is mandatory, because jdtls corrupts a shared
  workspace. It goes in
  `~/.cache/vogt-lsp/jdtls/<sha256(canonical root)>`, which persists on the
  home volume. The hub garbage-collects dirs whose root no longer exists.
- **Gradle import starts a Gradle daemon** through Buildship. The idle daemons
  measured above already hold several GiB, so the launcher passes
  `-Dorg.gradle.daemon.idletimeout=600000` through `GRADLE_OPTS` for the
  jdtls-spawned import.
- **Expectations:** most Java in the estate is Android or Capacitor shells.
  jdtls resolves plain-Java Gradle and Maven well, but Android Gradle Plugin
  projects only partly (no `R` class, no AGP variants). The realistic payoff
  today is small, and the memory cost is the largest of the three (D6).
- **Maven** is not installed. jdtls's m2e embeds its own Maven, so nothing more
  is needed for `pom.xml` projects.

## 6. How consumers attach

**Claude Code (native LSP tool).** Ship a Vogt-owned plugin `vogt-lsp`:

- It lives in a local marketplace at `/opt/vogt/claude-plugins`, outside home
  so it is not shadowed.
- Its `.lsp.json` declares three servers. Each has an absolute `command` of
  `/usr/local/bin/vogt-lsp` and `args` of `["python"|"rust"|"java"]`.
- The extension maps are: `.py`/`.pyi` → python, `.rs` → rust,
  `.java` → java.
- Java gets `startupTimeout` 120000. `restartOnCrash` stays true, with
  `maxRestarts` 3.
- `requestTimeout` is omitted, because the pinned CLI is 2.1.283 and the field
  needs 2.1.288.

`mcp-bootstrap.sh` adds the marketplace and installs and enables the plugin
idempotently, the same way it registers `vogt` today.

We use our own plugin rather than the official
`rust-analyzer-lsp`/`pyright-lsp`/`jdtls-lsp` because those resolve bare names
on `PATH`. They cannot route through the launcher, set root or interpreter, or
reach the hub without shadowing upstream binary names.

Two things to verify live on the pinned CLI:

- Whether `ENABLE_LSP_TOOL` is still required. The bundle registers it, but
  the current docs only mention installing a plugin.
- That the tool activates inside an engine PTY session.

**Codex (MCP bridge).** Codex has no LSP client.

- `vogt-lsp mcp <lang>` exposes a small fixed tool set over MCP: definition,
  references, hover, document and workspace symbols, and diagnostics for a
  file. It connects to the same hub.
- msp-klaudia's `internal/lsp` (tool shapes, and diagnostics appended to edit
  results) is the reference design. The third-party
  `isaacphi/mcp-language-server` is the fallback if we decline to own the
  bridge (D12).
- `mcp-bootstrap.sh` registers one MCP server per language for Codex.
- Codex's MCP config is global (`~/.codex/config.toml`), so the root comes
  from the session's cwd at call time and is not fixed at registration.

**PWA (gated on D8).**

- The engine route `GET /api/lsp?lang=…&root=…` upgrades to a WebSocket with
  first-frame auth, like attach. It requires `FilesystemRead`, and `root` is
  resolved through `workspace_path::resolve_existing` like every file route.
- The engine then pipes JSON-RPC to the hub.
- In the client, a thin hand-written Monaco adapter covers hover, definition
  (open in a tab), document symbols and diagnostics as markers.
- `monaco-languageclient` is rejected. It swaps `monaco-editor` for the
  `@codingame/monaco-vscode-api` stack, which is a large bundle and a
  structural change to the editor for four features.

## 7. Security

**Baseline.** The pod is already the trust boundary. Agents run with
permission checks disabled as uid 1000 with sudo, and they routinely run
`cargo build`, Gradle and `uv sync` on project code. A language server started
*by an agent* that executes `build.rs`, proc macros or a Gradle build therefore
grants nothing the agent did not already have.

**What is new** with a shared server:

1. **New triggers.**
   - With the PWA (phase 4), *opening a file in the browser* starts
     rust-analyzer or jdtls on that root. That runs `build.rs`, proc macros
     and the Gradle configuration phase **with no agent and no explicit
     action**.
   - Prewarming from the registry would do the same for every project.
2. **Cross-session reach through the hub.** The hub is one process tree serving
   all sessions. If it inherited the engine's env, a build script in any
   project could read engine secrets.
3. **Cache poisoning.** Code executed for project A can write to shared,
   content-addressed caches (`~/.cargo/registry`, `~/.gradle`, the uv cache)
   that project B later consumes. This is equally true of agent builds today.

**Proposed controls:**

- **Env sanitization.** The hub and every server get the PTY sanitizer's
  env, with the same `ENGINE_*`, `INFISICAL_*`, `*TOKEN*`, `*SECRET*`,
  `*PASSWORD*` and `*API_KEY*` stripping as `pty.rs:312-333`, and **never** the
  agent-auth passthrough. This deployment runs in `identity` mode, so an
  inherited `INFISICAL_CLIENT_SECRET` would expose the whole vault to any
  `build.rs`.
- **Code-execution gate by project trust.** Vogt already records
  `trust_state` per project. Only when the root's project is trusted, or the
  operator has opted it in, does the launcher enable:
  - rust-analyzer: `cargo.buildScripts.enable`, `procMacro.enable` and
    `checkOnSave`
  - jdtls: Gradle and Maven import
  - Python: no gate needed. basedpyright, pyright and ty are static and
    execute nothing. pylsp plugins would, which is one more reason to reject
    it.

  Otherwise the server runs in a "static" mode with reduced accuracy
  (proc-macro output missing, no Gradle classpath). Agent-started and
  PWA-started instances follow the same rule (D4).
- **No prewarm** of untrusted roots, and the hub never starts a server for a
  root outside the registry allowlist (§4).
- **Isolation that is not proposed now.** That means bubblewrap or user
  namespaces per server. The pod runs without userns privileges, and the
  agent itself is unsandboxed, so sandboxing only the language server buys
  little. Revisit this if agents are ever sandboxed.

## 8. Phases

| Phase | Delivers | Depends on |
|---|---|---|
| **0. Packaging and launcher** | Pinned basedpyright, rust-analyzer (standalone) and jdtls in pod-base. A `vogt-lsp` launcher providing root resolution, interpreter discovery, per-root state dirs, a sanitized env and per-language defaults. Docs in ENGINE.md §3.1 and §4. | D2, D9, D11 |
| **1. Agent wiring** | A Claude Code `vogt-lsp` plugin and a Codex MCP bridge, auto-registered by `mcp-bootstrap.sh`. Supersedes the manual `vogt-rust-analyzer-mcp` docs. Each process spawns its own servers, with Java capped. | 0, D3, D12 |
| **2. Shared hub** | One instance per (lang, root): idle GC, LRU and RSS budget, `/proc` metrics, and engine supervision. Prototype with lspmux (formerly ra-multiplex), then decide whether to absorb it into an engine `lsp_hub` module. | 1, D1, D7, D10 |
| **3. Trust gating and hardening** | Build-script and proc-macro gating by project trust, jdtls data-dir GC, and cache split. | 2, D4, D5 |
| **4. PWA integration** | WS `/api/lsp` plus a thin Monaco client. Rewrite ENGINE.md §8. | 2, 3, D8 |

## 9. Decisions for the operator

| # | Decision | Options | Recommendation |
|---|---|---|---|
| D1 | Sharing model | (a) one process per agent session; (b) shared hub, one per (lang, root) | **(b)**. Mandatory for rust-analyzer and jdtls at the estate's worktree count. |
| D2 | Python server | basedpyright · pyright · ty · pylsp; plus ruff server for lint, yes or no | **basedpyright**, with ty selectable through `VOGT_LSP_PYTHON`. Leave ruff server off at first, because agents already run `ruff`. |
| D3 | Diagnostics fed to agents | project config only · launcher default `standard` with errors only · off | **Errors only, `standard`, unless the project configures it.** Revisit for mypy projects. |
| D4 | Untrusted code execution | always on (the pod is the boundary) · gated on `trust_state` or opt-in | **Gate on trust or opt-in**, and require it for anything PWA-triggered. |
| D5 | Worktree Python env | borrow the main checkout's `.venv` · stdlib only · auto `uv sync --frozen` | **Borrow the main venv.** Never auto-sync. |
| D6 | Java now or later | ship jdtls in phase 0 · defer until a non-Android Java project is registered | **Package it in phase 0, but enable it only behind the hub** (cap 1). Android support stays best-effort. |
| D7 | Budgets | numbers in §4; also whether to finally set a pod `mem_limit` | Accept the §4 numbers. A pod limit is a separate ops decision, since swap is already full. |
| D8 | PWA language features | yes (phase 4) · no (keep §8) · later | **Later.** Ship agent value first. |
| D9 | Rust pinning and caches | standalone pinned rust-analyzer · rustup component; move the cargo registry to the home volume, yes or no | **Standalone pin; move the registry.** |
| D10 | Hub implementation | lspmux (third-party) · engine-native module | **Prototype on lspmux, then decide** after measuring. Engine-native is the long-term owner. |
| D11 | Image placement | lean and full pod-base (public image) · estate image only | **Pod-base.** It is Vogt-wide by definition. basedpyright and jdtls add about 150 MB, which is small next to Flutter or Android. |
| D12 | Codex bridge | Vogt-owned `vogt-lsp mcp` · `isaacphi/mcp-language-server` | **Vogt-owned**, ported from msp-klaudia's tool shapes, so tool names are stable across both agents. |

## 10. Work items

These are tracked in Vogt under the initiative
`shared-language-servers-python-rust-java`:

| Ref | Item |
|---|---|
| WI-885 | LSP-D: operator decisions D1–D12 |
| WI-886 | LSP-0: packaging and `vogt-lsp` launcher |
| WI-887 | LSP-1: agent wiring (Claude Code plugin and Codex MCP bridge) |
| WI-888 | LSP-2: shared hub |
| WI-889 | LSP-3: trust gating and hardening |
| WI-890 | LSP-4: PWA integration (gated on D8) |
