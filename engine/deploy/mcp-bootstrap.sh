#!/usr/bin/env bash
# Idempotently register Vogt for clients present in the image.
# Registration stores only the endpoint and wrapper command; no bearer value.
set -euo pipefail

# Where an agent in this session should reach Vogt, in the order of what
# actually knows the answer:
#
#   1. `VOGT_MCP_URL`, an explicit override. Unchanged.
#   2. The session's own `VOGT_URL`. `vogt session start` exports the endpoint
#      the operator configured for clients, and it is the only thing
#      here that knows which deployment this session belongs to. Ignoring it
#      is what this script used to do.
#   3. The front door on loopback. In the merged stack the engine is the only
#      published port and the agent runs in this container, so
#      loopback needs no DNS and no certificate — and it cannot go on naming
#      a deployment after that deployment is retired, which is what a default
#      pointing at a specific host would do. The loopback front door belongs to
#      whatever deployment this session is part of.
_vogt_endpoint="${VOGT_MCP_URL:-}"
if [[ -z "$_vogt_endpoint" && -n "${VOGT_URL:-}" ]]; then
    _vogt_endpoint="${VOGT_URL%/}/mcp"
fi
readonly VOGT_ENDPOINT="${_vogt_endpoint:-http://127.0.0.1:8910/mcp}"
unset _vogt_endpoint
readonly VOGT_WRAPPER="/usr/local/bin/vogt-mcp"
readonly VOGT_SRC="${VOGT_SRC:-$HOME/Working/Active/apps/vogt}"

# Whether Claude Code already runs `name` with `command` (and `arg`, when
# given), read straight from its config (WI-927). `claude mcp get` answers the
# same question but starts node and health-checks the server, ~0.7 s a call,
# and this script asks it several times on every session launch. Only a "yes"
# is trusted: anything else (no jq, no file, a project-scope .mcp.json, a stale
# entry) falls through to the CLI path below, which is unchanged.
claude_runs() {
    local name="$1" command="$2" arg="${3:-}" config="${CLAUDE_CONFIG_DIR:-$HOME}/.claude.json"
    command -v jq >/dev/null 2>&1 && [[ -f "$config" ]] || return 1
    jq -e --arg n "$name" --arg c "$command" --arg a "$arg" --arg cwd "$PWD" '
        [.mcpServers[$n]?, .projects[$cwd].mcpServers[$n]?]
        | map(select(. != null))
        | length > 0 and all(.command == $c
            and ($a == "" or ((.args // []) | index($a)) != null))' \
        "$config" >/dev/null 2>&1
}

# Whether `name` appears in no Claude Code config this session can see: user
# or local scope in Claude Code's config (`$CLAUDE_CONFIG_DIR/.claude.json`, else
# ~/.claude.json: a profile has its own), or the project's .mcp.json. Lets an
# unwanted read-only server be skipped without asking the CLI to look for it.
claude_lacks() {
    local name="$1" config="${CLAUDE_CONFIG_DIR:-$HOME}/.claude.json"
    command -v jq >/dev/null 2>&1 && [[ -f "$config" ]] || return 1
    jq -e --arg n "$name" --arg cwd "$PWD" '
        (.mcpServers[$n]? == null) and (.projects[$cwd].mcpServers[$n]? == null)' \
        "$config" >/dev/null 2>&1 || return 1
    [[ ! -f "$PWD/.mcp.json" ]] || ! grep -qF "\"$name\"" "$PWD/.mcp.json"
}

install_vogt_bridge() {
    # Vogt is not on PyPI, so the image cannot install the bridge at build
    # time. The workspace checkout is the only source. When vogt goes public
    # this moves into the Dockerfile and this becomes a no-op fallback.
    command -v vogt-mcp-remote >/dev/null 2>&1 && return 0
    if [[ ! -f "$VOGT_SRC/pyproject.toml" ]]; then
        printf 'mcp-bootstrap: no vogt checkout at %s; Vogt bridge unavailable\n' \
            "$VOGT_SRC" >&2
        return 0
    fi
    if ! pip3 install --user --break-system-packages --no-cache-dir --quiet \
        -e "$VOGT_SRC"; then
        printf 'mcp-bootstrap: failed to install vogt-mcp-remote from %s\n' \
            "$VOGT_SRC" >&2
        return 0
    fi
    command -v vogt-mcp-remote >/dev/null 2>&1 || printf \
        'mcp-bootstrap: vogt-mcp-remote still not on PATH after install\n' >&2
}

install_vogt_codex() {
    command -v codex >/dev/null 2>&1 || return 0
    # Reconcile the URL rather than checking the key exists: a presence-only
    # check pins whatever URL was current when the client was first registered,
    # so a moved endpoint keeps failing to hand-shake and re-running changes
    # nothing.
    local registered
    if registered="$(codex mcp get vogt 2>/dev/null)"; then
        if grep -qF "$VOGT_ENDPOINT" <<<"$registered"; then
            return 0
        fi
        codex mcp remove vogt >/dev/null 2>&1 || return 0
    fi
    codex mcp add vogt --url "$VOGT_ENDPOINT" \
        --bearer-token-env-var VOGT_HTTP_TOKEN >/dev/null
}

install_vogt_claude() {
    command -v claude >/dev/null 2>&1 || return 0
    # Reconcile the COMMAND, not mere name-presence. A name-only guard pins
    # whatever launcher path was stored when the client was first registered, so
    # a renamed or relocated wrapper (e.g. an older `mydevenv2-*` path carried
    # into a new image) leaves a stale `command` that Claude fails to spawn
    # (`ENOENT`), and re-running never heals it. Mirror the codex branch:
    # replace the registration unless its command already is the current wrapper.
    claude_runs vogt "$VOGT_WRAPPER" && return 0
    if claude mcp get vogt >/dev/null 2>&1; then
        if claude mcp get vogt 2>/dev/null | grep -qF "$VOGT_WRAPPER"; then
            return 0
        fi
        # No --scope: remove wherever the stale entry lives.
        claude mcp remove vogt >/dev/null 2>&1 || return 0
    fi
    claude mcp add --scope user vogt -- "$VOGT_WRAPPER" >/dev/null
}

install_vogt_opencode() {
    command -v opencode >/dev/null 2>&1 || return 0
    # Reconcile the COMMAND, not mere name-presence (same stale-launcher bug as
    # the claude branch). opencode has no `mcp remove`, but `mcp add` upserts,
    # so re-adding overwrites a stale command with the current wrapper. Skip only
    # when the current wrapper is already the registered command.
    if rg -qF "$VOGT_WRAPPER" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/opencode.json" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/opencode.jsonc" \
        "$PWD/opencode.json" \
        "$PWD/opencode.jsonc" 2>/dev/null; then
        return 0
    fi
    # No `--env VOGT_URL`: this registration is written once and reused by every
    # later session, so pinning a URL here freezes whichever deployment happened
    # to be current when a client was first registered — and it would *override*
    # the session's own `VOGT_URL`, the one value that knows where this session's
    # Vogt is. The wrapper reads it at spawn instead.
    opencode mcp add vogt \
        -- "$VOGT_WRAPPER" >/dev/null
}

# Klaudia (WI-974) reads MCP servers only from its own `.mcp.json` and has no
# `mcp add` to write one, so `vogt-klaudia-mcp` does it. Without this a Klaudia
# session ran with no Vogt tools at all, though its token was valid.
# Overridable only so the registration logic can be exercised outside the image.
readonly KLAUDIA_MCP="${VOGT_KLAUDIA_MCP:-/usr/local/bin/vogt-klaudia-mcp}"

install_vogt_klaudia() {
    command -v klaudia >/dev/null 2>&1 || return 0
    "$KLAUDIA_MCP" set vogt "$VOGT_WRAPPER"
}

# Optional read-only MCP servers (docs/ENGINE.md §4). Each is registered only
# while this session holds its token, and unregistered when it does not, so a
# deployment opts in by adding the token to its agent-auth manifest and opts
# out by removing it. The registration names `vogt-readonly-mcp <server>` and
# nothing else: the token stays in the session's environment, read at spawn,
# and the read-only switches live in the wrapper where a session cannot edit
# them. Names end in `-ro` so a server an operator registered by hand under
# the plain name is never replaced or removed.
# Overridable only so the registration logic can be exercised outside the image.
readonly READONLY_WRAPPER="${VOGT_READONLY_MCP_WRAPPER:-/usr/local/bin/vogt-readonly-mcp}"
# name | wrapper argument | upstream binary | variables that enable it (all
# required) and that Codex must pass through to the server (Codex starts stdio
# servers with a minimal environment unless told which variables to forward).
readonly READONLY_SERVERS=(
    "github-ro|github|github-mcp-server|GITHUB_MCP_TOKEN|VOGT_GITHUB_MCP_TOOLSETS"
    "grafana-ro|grafana|mcp-grafana|GRAFANA_URL GRAFANA_SERVICE_ACCOUNT_TOKEN|"
    "forgejo-ro|gitea|gitea-mcp|GITEA_HOST GITEA_MCP_TOKEN|"
)

readonly_wanted() {
    local var
    for var in $1; do
        [[ -n "${!var:-}" ]] || return 1
    done
    return 0
}

# Write one read-only server table into Codex's config.toml (GitHub #914).
#
# Several sessions start at once, and each used to check then append (`>>`)
# with nothing held between the two, so the file grew a second copy of every
# `[mcp_servers.*]` table. Duplicate tables do not parse, and a file that does
# not parse then failed the "already registered" check, so every later start
# appended again.
#
# The whole read-modify-write is one flock on a lock next to the config. The
# new file is written beside the old one and renamed over it, so a reader never
# sees a partial write. A config that does not parse is left exactly as it is:
# appending to it cannot repair it and would only add another copy. A table
# this script already wrote (one whose command is the wrapper) is replaced, so
# a second start changes nothing; a table an operator wrote under the same
# name, running something else, is left where it is.
readonly_codex_write() {
    local config="$1" name="$2" arg="$3" vars="$4" lockdir
    lockdir="$(dirname "$config")"
    mkdir -p "$lockdir"
    (
        flock 9
        CODEX_CONFIG="$config" CODEX_SERVER="$name" CODEX_ARG="$arg" \
            CODEX_VARS="$vars" CODEX_WRAPPER="$READONLY_WRAPPER" \
            python3 - <<'PY'
import os, sys, tempfile

path = os.environ["CODEX_CONFIG"]
name = os.environ["CODEX_SERVER"]
header = f"[mcp_servers.{name}]"

def approval_row(source: str) -> str:
    # This writer replaces its own table whole, so an operator's explicit mode
    # has to be read first and carried into the new block (WI-1634). Without
    # that, every start would turn a "prompt" back into "approve".
    for row in source.splitlines():
        head = row.split("#", 1)[0]
        if head.split("=", 1)[0].strip() == "default_tools_approval_mode":
            return row if row.endswith("\n") else row + "\n"
    return 'default_tools_approval_mode = "approve"\n'
try:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
except FileNotFoundError:
    text = ""
except OSError as exc:
    print(f"mcp-bootstrap: cannot read {path} ({exc}); left alone", file=sys.stderr)
    sys.exit(1)

if text.strip():
    try:
        import tomllib
        tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        print(
            f"mcp-bootstrap: {path} is not valid TOML ({exc}); left alone",
            file=sys.stderr,
        )
        sys.exit(1)

lines = text.splitlines(keepends=True)
kept: list[str] = []
existing = ""
index = 0
while index < len(lines):
    stripped = lines[index].strip()
    if stripped == header or stripped.startswith(header + "."):
        index += 1
        while index < len(lines) and not lines[index].lstrip().startswith("["):
            if stripped == header:
                existing += lines[index]
            index += 1
        continue
    kept.append(lines[index])
    index += 1

# Read-only by construction, so pre-approved (WI-1634). Written here as well
# as by the driven-session bundle, or each start would drop it. The key lives
# in the session's own config.toml, which the session can edit, so it is a
# convenience for autonomous work and not a security boundary.
block = (
    f"{header}\n"
    f'command = "{os.environ["CODEX_WRAPPER"]}"\n'
    f'args = ["{os.environ["CODEX_ARG"]}"]\n'
    f'env_vars = [{os.environ["CODEX_VARS"]}]\n'
    f"{approval_row(existing)}"
)
body = "".join(kept).rstrip("\n")
new = f"{body}\n\n{block}" if body else block
if new == text:
    sys.exit(0)
directory = os.path.dirname(path) or "."
fd, tmp = tempfile.mkstemp(prefix=".config.toml.", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(new)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
except BaseException:
    os.unlink(tmp)
    raise
PY
    ) 9>"$lockdir/.config.toml.lock"
}

# The app-server daemon (WI-1142) copies the invoking binary into
# $CODEX_HOME/packages and execs that copy, which then sits outside the pin.
# Written on its own path: a session with no read-only server still needs it,
# and an existing [features] table must gain the key rather than hide it.
codex_disable_daemon() {
    local config="$1" lockdir
    lockdir="$(dirname "$config")"
    mkdir -p "$lockdir"
    (
        flock 9
        CODEX_CONFIG="$config" python3 - <<'PY'
import os, sys, tempfile

path = os.environ["CODEX_CONFIG"]
try:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
except FileNotFoundError:
    text = ""
except OSError as exc:
    print(f"mcp-bootstrap: cannot read {path} ({exc}); left alone", file=sys.stderr)
    sys.exit(1)

if text.strip():
    try:
        import tomllib
        tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        print(
            f"mcp-bootstrap: {path} is not valid TOML ({exc}); left alone",
            file=sys.stderr,
        )
        sys.exit(1)

lines = text.splitlines(keepends=True)
out: list[str] = []
index = 0
found_table = False
found_key = False
while index < len(lines):
    stripped = lines[index].strip()
    if stripped == "[features]":
        found_table = True
        out.append(lines[index])
        index += 1
        while index < len(lines) and not lines[index].lstrip().startswith("["):
            row = lines[index]
            key = row.split("=", 1)[0].strip() if "=" in row else ""
            if key == "daemon_auto_start":
                found_key = True
                out.append("daemon_auto_start = false\n")
            else:
                out.append(row)
            index += 1
        if not found_key:
            while out and out[-1].strip() == "":
                out.pop()
            out.append("daemon_auto_start = false\n")
        if index < len(lines) and lines[index].lstrip().startswith("["):
            while out and out[-1].strip() == "":
                out.pop()
            out.append("\n")
        continue
    out.append(lines[index])
    index += 1

if not found_table:
    # After the root keys, not before them: a table header swallows every
    # following assignment until the next header, and model/model_provider
    # live at the root.
    split_at = next(
        (i for i, row in enumerate(out) if row.lstrip().startswith("[")),
        len(out),
    )
    table = "[features]\ndaemon_auto_start = false\n"
    head = "".join(out[:split_at]).rstrip("\n")
    tail = "".join(out[split_at:]).rstrip("\n")
    parts = [part for part in (head, table.rstrip("\n"), tail) if part]
    new = "\n\n".join(parts) + "\n"
else:
    new = "".join(out)
    if not new.endswith("\n"):
        new += "\n"
if new == text:
    sys.exit(0)
directory = os.path.dirname(path) or "."
fd, tmp = tempfile.mkstemp(prefix=".config.toml.", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(new)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
except BaseException:
    os.unlink(tmp)
    raise
PY
    ) 9>"$lockdir/.config.toml.lock"
}

# Codex compacts its own context at min(model_auto_compact_token_limit, 90% of
# the window) (WI-1575). Vogt owns that limit so a fresh pod does not run the
# model default: the top-level key (ENGINE_CODEX_AUTO_COMPACT_TOKENS, default
# 200000) and its scope "total" are written only when absent. An operator's
# value is never overridden, and a config that does not parse is left alone.
codex_auto_compact() {
    local config="$1" lockdir
    lockdir="$(dirname "$config")"
    mkdir -p "$lockdir"
    (
        flock 9
        CODEX_CONFIG="$config" \
            CODEX_AUTO_COMPACT="${ENGINE_CODEX_AUTO_COMPACT_TOKENS:-200000}" \
            python3 - <<'PY'
import os, sys, tempfile, tomllib

path = os.environ["CODEX_CONFIG"]
limit = os.environ["CODEX_AUTO_COMPACT"]
if not limit.isdigit() or int(limit) <= 0:
    print(
        f"mcp-bootstrap: ENGINE_CODEX_AUTO_COMPACT_TOKENS={limit!r} is not a "
        "positive integer; left alone",
        file=sys.stderr,
    )
    sys.exit(1)
try:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
except FileNotFoundError:
    text = ""
except OSError as exc:
    print(f"mcp-bootstrap: cannot read {path} ({exc}); left alone", file=sys.stderr)
    sys.exit(1)
try:
    parsed = tomllib.loads(text)
except tomllib.TOMLDecodeError as exc:
    print(f"mcp-bootstrap: {path} is not valid TOML ({exc}); left alone", file=sys.stderr)
    sys.exit(1)

wanted = [
    ("model_auto_compact_token_limit", str(int(limit))),
    ("model_auto_compact_token_limit_scope", '"total"'),
]
rows = [f"{key} = {value}\n" for key, value in wanted if key not in parsed]
if not rows:
    sys.exit(0)
lines = text.splitlines(keepends=True)
if lines and not lines[-1].endswith("\n"):
    lines[-1] += "\n"
# Root keys go before the first table header, which would swallow them.
split_at = next(
    (i for i, row in enumerate(lines) if row.lstrip().startswith("[")),
    len(lines),
)
head = lines[:split_at]
while head and head[-1].strip() == "":
    head.pop()
tail = lines[split_at:]
new = "".join(head + rows + (["\n"] if tail else []) + tail)
directory = os.path.dirname(path) or "."
fd, tmp = tempfile.mkstemp(prefix=".config.toml.", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(new)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
except BaseException:
    os.unlink(tmp)
    raise
PY
    ) 9>"$lockdir/.config.toml.lock"
}

# Route Codex through the estate LiteLLM router (WI-1147). The estate fork
# (WI-1286) speaks the Chat Completions wire as well as Responses, and grok
# returns tool calls only over chat, so the router is used with
# wire_api = "chat". The session key is the env var named by env_key — never a
# token written into this file. theclawbay stays as a fallback profile and is
# not given a bearer here: a profile this script did not write is left as the
# operator has it, and one it did write carries no secret.
#
# The router's groups have no entry in Codex's bundled catalog, and the
# fallback metadata offers no apply_patch tool and does not turn code mode off.
# Alongside the provider this writes a catalog of the groups that answered a
# chat completion with the session key (WI-1592), apply_patch freeform and
# tool_mode omitted, and points model_catalog_json at it. Listed, in order:
# grok, code, small, reasoning, vision, the OpenRouter groups that answer
# (or-deepseek-flash, or-gemini-flash, or-glm-5-3 — not or-deepseek, which
# LiteLLM blocks), and the DLab models whose plan actually serves them.
# qwen3-coder, qwen3-8-27b and gpt-oss-120b are not listed: live probes
# (WI-1147, cmt_01M4YZWG8B7E9YE5Y8FNJ7H2GH) showed they fail inside LiteLLM
# (408 / hang). Re-adding them is WI-1631. Priority is the index in that
# order,
# because the fork sorts ascending and defaults to the first visible model,
# so grok stays the default. premium is hidden from the picker. Not listed,
# because a completion does not succeed: bedrock (no such model on the
# router), the DLab Gemini preview and Claude Haiku (plan / availability
# errors), and the DLab free gpt-6 aliases (auth error).
# absent or when it already names the file this writer owns; an operator's
# catalog is left alone. Top-level model and model_provider are set only when
# absent or when they already name this bundle (litellm, or theclawbay as the
# provider). An operator's other provider and model are left alone. A config
# that does not parse is left alone, same as the other writers above.
codex_litellm_provider() {
    local config="$1" lockdir
    lockdir="$(dirname "$config")"
    mkdir -p "$lockdir"
    (
        flock 9
        CODEX_CONFIG="$config" python3 - <<'PY'
import json, os, sys, tempfile

path = os.environ["CODEX_CONFIG"]
# The catalog Codex deserializes as ModelsResponse (protocol openai_models.rs).
# One entry per LiteLLM router group, the same shape the grok entry had.
# apply_patch_tool_type "freeform" is what makes Codex offer the apply_patch
# tool; tool_mode is omitted, which is code mode off. availability_nux has no
# serde default, so it must be present (null). The catalog deserializer rejects
# a model with neither base_instructions nor model_messages.instructions_template,
# so the template is a short non-empty string. The fork sorts by priority
# ascending and marks the first visible model as the default, so priority is
# the index in router order and grok stays first. premium is the router's
# opt-in group, so its visibility is "hide": choosable by name, not in the picker.
# Groups a chat completion with the session key actually answered (WI-1592).
# or-deepseek is blocked at the router and is not here. Bedrock is not a
# model on this router. DLab models that 400/401/503 are not here either.
# qwen3-coder, qwen3-8-27b and gpt-oss-120b are not here either: they fail
# inside LiteLLM (408 / hang, WI-1147 cmt_01M4YZWG8B7E9YE5Y8FNJ7H2GH).
# Re-adding them is WI-1631.
ROUTER_GROUPS = (
    "grok",
    "code",
    "small",
    "reasoning",
    "vision",
    "or-deepseek-flash",
    "or-gemini-flash",
    "or-glm-5-3",
    "dlab-deepseek-v4-pro",
    "dlab-deepseek-v4-1-flash",
    "dlab-grok-4-5",
    "dlab-grok-4-6",
    "dlab-grok-4-7",
    "dlab-gemini-3-8-flash",
    "dlab-glm-5-3",
    "dlab-glm-5-3-flash",
    "dlab-glm-5-3-flashx",
    "dlab-kimi-k3",
    "premium",
)

OURS = "Estate LiteLLM "
DEFAULT_CONTEXT = 256000
#: Families whose real window the router misreports (swack-haiku says 1M;
#: Claude serves 200k). Matched on a dash-separated word of the slug.
FAMILY_CONTEXT = {"haiku": 200000, "sonnet": 200000, "opus": 200000, "claude": 200000}

def _context_window(slug: str, reported: object) -> int:
    words = slug.lower().replace(".", "-").split("-")
    for family, window in FAMILY_CONTEXT.items():
        if family in words:
            return window
    if isinstance(reported, int) and reported > 0:
        return reported
    return DEFAULT_CONTEXT

def _catalog_model(
    priority: int, slug: str, context_window: int = DEFAULT_CONTEXT
) -> dict[str, object]:
    return {
        "slug": slug,
        "display_name": slug,
        "description": f"{OURS}{slug} group.",
        "supported_reasoning_levels": [],
        "shell_type": "unified_exec",
        "visibility": "hide" if slug == "premium" else "list",
        "supported_in_api": True,
        "priority": priority,
        "availability_nux": None,
        "upgrade": None,
        "model_messages": {
            "instructions_template": (
                f"You are Codex, running through the estate LiteLLM router as {slug}."
            ),
            "approvals": None,
            "collaboration_modes": None,
            "auto_review": None,
            "permissions": None,
            "multi_agent": None,
        },
        "default_reasoning_summary": "none",
        "support_verbosity": False,
        "default_verbosity": None,
        "apply_patch_tool_type": "freeform",
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "context_window": context_window,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
    }

catalog_path = os.path.join(os.path.dirname(path), "vogt-model-catalog.json")
probe_path = os.path.join(os.path.dirname(path), "vogt-model-catalog.probe.json")

# WI-2740: the groups come from the router itself when the session key is at
# hand: every group /model/info lists that answers a one-token completion,
# with its context window. The answers are cached for a few hours so a launch
# does not wait on the probes. Without a key, or with the router down, the
# last answers (or the list above) stand.
def _discover() -> dict[str, int] | None:
    import time
    import urllib.request
    from concurrent.futures import ThreadPoolExecutor

    key = os.environ.get("HOMELAB_LITELLM_SESSION_KEY", "").strip()
    base = os.environ.get("VOGT_LITELLM_BASE_URL", "http://100.92.54.45:4000/v1").rstrip("/")
    ttl = int(os.environ.get("VOGT_CODEX_CATALOG_TTL", "21600") or 0)
    try:
        with open(probe_path, encoding="utf-8") as fh:
            cached = json.load(fh)
        if (
            cached.get("base") == base
            and time.time() - float(cached.get("at", 0)) < ttl
            and isinstance(cached.get("groups"), dict)
        ):
            return {str(k): int(v) for k, v in cached["groups"].items()}
    except (OSError, ValueError, TypeError):
        cached = None
    if not key or os.environ.get("VOGT_CODEX_CATALOG_PROBE", "1") == "0":
        if cached and isinstance(cached.get("groups"), dict):
            return {str(k): int(v) for k, v in cached["groups"].items()}
        return None
    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}

    def _get(url: str) -> object:
        request = urllib.request.Request(url, headers=headers)
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)

    try:
        info = _get(f"{base}/model/info")
        rows = info.get("data", []) if isinstance(info, dict) else []
        reported = {
            str(row.get("model_name")): (row.get("model_info") or {}).get("max_input_tokens")
            for row in rows
            if isinstance(row, dict) and row.get("model_name")
        }
    except Exception as exc:  # the router is optional at bootstrap
        print(f"mcp-bootstrap: LiteLLM groups not listed ({type(exc).__name__}); catalog kept", file=sys.stderr)
        return None

    def _answers(slug: str) -> bool:
        body = json.dumps(
            {"model": slug, "max_tokens": 1, "messages": [{"role": "user", "content": "ok"}]}
        ).encode()
        request = urllib.request.Request(
            f"{base}/chat/completions", data=body, headers=headers, method="POST"
        )
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                return 200 <= response.status < 300
        except Exception:
            return False

    with ThreadPoolExecutor(max_workers=8) as pool:
        ok = dict(zip(reported, pool.map(_answers, reported)))
    groups = {slug: _context_window(slug, reported[slug]) for slug in reported if ok[slug]}
    if not groups:
        return None
    try:
        fd, tmp = tempfile.mkstemp(prefix=".vogt-model-catalog.probe.", dir=os.path.dirname(path) or ".")
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump({"at": time.time(), "base": base, "groups": groups}, fh)
        os.replace(tmp, probe_path)
    except OSError:
        pass
    return groups

discovered = _discover()
if discovered is None:
    ordered = [(slug, _context_window(slug, None)) for slug in ROUTER_GROUPS]
else:
    # The known order first (grok stays the default where it is served),
    # then the rest by name.
    known = [slug for slug in ROUTER_GROUPS if slug in discovered]
    ordered = [(slug, discovered[slug]) for slug in known] + [
        (slug, discovered[slug]) for slug in sorted(discovered) if slug not in known
    ]
OUR_MODELS = [_catalog_model(i, slug, window) for i, (slug, window) in enumerate(ordered)]
SLUGS = [slug for slug, _ in ordered]

# An entry an operator added by hand (any description but ours) is kept,
# after the router's groups, unless a group of the same slug replaces it.
try:
    with open(catalog_path, encoding="utf-8") as fh:
        _previous = json.load(fh).get("models", [])
except (OSError, ValueError, AttributeError):
    _previous = []
OPERATOR_MODELS = [
    row
    for row in _previous
    if isinstance(row, dict)
    and not str(row.get("description", "")).startswith(OURS)
    and row.get("slug") not in SLUGS
]
CATALOG = {"models": OUR_MODELS + OPERATOR_MODELS}
try:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
except FileNotFoundError:
    text = ""
except OSError as exc:
    print(f"mcp-bootstrap: cannot read {path} ({exc}); left alone", file=sys.stderr)
    sys.exit(1)

if text.strip():
    try:
        import tomllib
        tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        print(
            f"mcp-bootstrap: {path} is not valid TOML ({exc}); left alone",
            file=sys.stderr,
        )
        sys.exit(1)

LITELLM = (
    "[model_providers.litellm]\n"
    'name = "LiteLLM"\n'
    'base_url = "http://100.92.54.45:4000/v1"\n'
    'wire_api = "chat"\n'
    "requires_openai_auth = false\n"
    "supports_websockets = false\n"
    'env_key = "HOMELAB_LITELLM_SESSION_KEY"\n'
)
CLAWBAY = (
    "[model_providers.theclawbay]\n"
    'name = "OpenAI"\n'
    'base_url = "https://api.theclawbay.com/backend-api/codex"\n'
    'wire_api = "responses"\n'
    "requires_openai_auth = false\n"
    "supports_websockets = false\n"
)

def _drop(lines: list[str], header: str) -> list[str]:
    kept: list[str] = []
    index = 0
    while index < len(lines):
        stripped = lines[index].strip()
        if stripped == header or stripped.startswith(header + "."):
            index += 1
            while index < len(lines) and not lines[index].lstrip().startswith("["):
                index += 1
            continue
        kept.append(lines[index])
        index += 1
    return kept

lines = text.splitlines(keepends=True)
# Keep an operator-written theclawbay profile (it may hold their own auth).
# Replace only a profile this writer owns: no experimental_bearer_token.
owns_clawbay = False
in_clawbay = False
clawbay_body: list[str] = []
for row in lines:
    stripped = row.strip()
    if stripped.startswith("["):
        if in_clawbay and not any(
            part.strip().startswith("experimental_bearer_token")
            for part in clawbay_body
        ):
            owns_clawbay = True
        in_clawbay = stripped == "[model_providers.theclawbay]"
        clawbay_body = []
        continue
    if in_clawbay:
        clawbay_body.append(row)
if in_clawbay and not any(
    part.strip().startswith("experimental_bearer_token") for part in clawbay_body
):
    owns_clawbay = True

lines = _drop(lines, "[model_providers.litellm]")
if owns_clawbay or not any(
    row.strip() == "[model_providers.theclawbay]" for row in lines
):
    lines = _drop(lines, "[model_providers.theclawbay]")
    write_clawbay = True
else:
    write_clawbay = False

def _in_table(index: int) -> bool:
    for earlier in range(index - 1, -1, -1):
        stripped = lines[earlier].strip()
        if stripped.startswith("["):
            return True
    return False

def _root_slots(key: str) -> list[int]:
    slots: list[int] = []
    for index, row in enumerate(lines):
        if _in_table(index) or "=" not in row or row.lstrip().startswith("#"):
            continue
        if row.split("=", 1)[0].strip() == key:
            slots.append(index)
    return slots

def _root_value(key: str) -> str | None:
    slots = _root_slots(key)
    if not slots:
        return None
    return lines[slots[-1]].split("=", 1)[1].strip().strip('"').strip("'")

def _set_root(key: str, value: str, when: set[str | None]) -> None:
    current = _root_value(key)
    if current not in when:
        return
    rendered = f'{key} = "{value}"\n'
    slots = _root_slots(key)
    if slots:
        lines[slots[-1]] = rendered
        return
    # A key appended after the last table belongs to that table. Root keys
    # that are new go before the first header.
    at = next(
        (i for i, row in enumerate(lines) if row.lstrip().startswith("[")),
        len(lines),
    )
    lines.insert(at, rendered)

# Switch the default only when it is unset or already one of ours.
_set_root("model_provider", "litellm", {None, "litellm", "theclawbay"})
# Under the router, a model the catalog does not list cannot be served: a
# leftover theclawbay default such as gpt-6-astra makes every model-less
# `codex exec` (the codex-subagent child among them) fail (WI-1640). So the
# default moves to grok unless it already names a catalog group.
if _root_value("model_provider") == "litellm":
    _model = _root_value("model")
    _served = SLUGS + [str(row.get("slug")) for row in OPERATOR_MODELS]
    if _model not in _served and SLUGS:
        _set_root("model", SLUGS[0], {_model})
# Point at the catalog this writer owns. A path an operator chose stays.
_set_root("model_catalog_json", catalog_path, {None, catalog_path})

body = "".join(lines).rstrip("\n")
extra = LITELLM + ("\n" + CLAWBAY if write_clawbay else "")
new = f"{body}\n\n{extra}" if body else extra
if not new.endswith("\n"):
    new += "\n"
catalog_text = json.dumps(CATALOG, indent=2) + "\n"
try:
    with open(catalog_path, encoding="utf-8") as fh:
        catalog_current = fh.read()
except FileNotFoundError:
    catalog_current = None
except OSError as exc:
    print(
        f"mcp-bootstrap: cannot read {catalog_path} ({exc}); left alone",
        file=sys.stderr,
    )
    sys.exit(1)
# The catalog is only rewritten while config points at it. An operator who
# keeps their own model_catalog_json is not handed a file they did not ask for.
owns_catalog = _root_value("model_catalog_json") == catalog_path
if new == text and (not owns_catalog or catalog_current == catalog_text):
    sys.exit(0)
directory = os.path.dirname(path) or "."
if owns_catalog and catalog_current != catalog_text:
    fd, tmp = tempfile.mkstemp(prefix=".vogt-model-catalog.json.", dir=directory)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(catalog_text)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, catalog_path)
    except BaseException:
        os.unlink(tmp)
        raise
if new == text:
    sys.exit(0)
fd, tmp = tempfile.mkstemp(prefix=".config.toml.", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(new)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
except BaseException:
    os.unlink(tmp)
    raise
PY
    ) 9>"$lockdir/.config.toml.lock"
}

readonly_codex() {
    local name="$1" arg="$2" required="$3" optional="$4" config vars var
    command -v codex >/dev/null 2>&1 || return 0
    config="${CODEX_HOME:-$HOME/.codex}/config.toml"
    if [[ "$5" != "yes" ]]; then
        # Remove only what this script wrote: an entry running the wrapper.
        if codex mcp get "$name" 2>/dev/null | grep -qF "$READONLY_WRAPPER"; then
            codex mcp remove "$name" >/dev/null 2>&1 || true
        fi
        return 0
    fi
    vars=""
    for var in $required $optional; do
        vars+="${vars:+, }\"$var\""
    done
    readonly_codex_write "$config" "$name" "$arg" "$vars"
}

readonly_claude() {
    local name="$1" arg="$2"
    command -v claude >/dev/null 2>&1 || return 0
    if [[ "$3" != "yes" ]]; then
        claude_lacks "$name" && return 0
        if claude mcp get "$name" 2>/dev/null | grep -qF "$READONLY_WRAPPER"; then
            claude mcp remove --scope user "$name" >/dev/null 2>&1 || true
        fi
        return 0
    fi
    # Claude Code hands a stdio server the session's own environment, so the
    # registration needs no `-e`: an `-e TOKEN=...` would store the value.
    claude_runs "$name" "$READONLY_WRAPPER" "$arg" && return 0
    if claude mcp get "$name" 2>/dev/null | grep -qF "$READONLY_WRAPPER"; then
        return 0
    fi
    claude mcp remove --scope user "$name" >/dev/null 2>&1 || true
    claude mcp add --scope user "$name" -- "$READONLY_WRAPPER" "$arg" >/dev/null
}

readonly_opencode() {
    local name="$1" arg="$2" config_dir
    command -v opencode >/dev/null 2>&1 || return 0
    # opencode has no `mcp remove`, and its config is JSONC an operator edits
    # by hand, so an unwanted entry is left where it is: without its token
    # the wrapper refuses to start, so a stale entry is one failed server,
    # never a credential (WI-975).
    [[ "$3" == "yes" ]] || return 0
    config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/opencode"
    # opencode passes a local server the session's own environment, so the
    # registration carries no `--env`. `mcp add` upserts, so a present name
    # that does not yet run the wrapper is simply overwritten.
    if grep -qsF "\"$name\"" "$config_dir/opencode.json" "$config_dir/opencode.jsonc" \
        && grep -qsF "$READONLY_WRAPPER" "$config_dir/opencode.json" "$config_dir/opencode.jsonc"; then
        return 0
    fi
    opencode mcp add "$name" -- "$READONLY_WRAPPER" "$arg" >/dev/null
}

readonly_klaudia() {
    command -v klaudia >/dev/null 2>&1 || return 0
    if [[ "$3" == "yes" ]]; then
        "$KLAUDIA_MCP" set "$1" "$READONLY_WRAPPER" "$2"
    else
        "$KLAUDIA_MCP" remove "$1" "$READONLY_WRAPPER"
    fi
}

install_readonly_servers() {
    local entry name arg bin required optional wanted
    for entry in "${READONLY_SERVERS[@]}"; do
        IFS='|' read -r name arg bin required optional <<<"$entry"
        wanted=no
        if readonly_wanted "$required"; then
            if [[ -x "$READONLY_WRAPPER" ]] && command -v "$bin" >/dev/null 2>&1; then
                wanted=yes
            else
                printf 'mcp-bootstrap: %s token present but %s or %s is not installed; not registered\n' \
                    "$name" "$READONLY_WRAPPER" "$bin" >&2
            fi
        fi
        readonly_codex "$name" "$arg" "$required" "$optional" "$wanted" \
            || printf 'mcp-bootstrap: codex registration of %s failed\n' "$name" >&2
        readonly_claude "$name" "$arg" "$wanted" \
            || printf 'mcp-bootstrap: claude registration of %s failed\n' "$name" >&2
        readonly_opencode "$name" "$arg" "$wanted" \
            || printf 'mcp-bootstrap: opencode registration of %s failed\n' "$name" >&2
        readonly_klaudia "$name" "$arg" "$wanted" \
            || printf 'mcp-bootstrap: klaudia registration of %s failed\n' "$name" >&2
    done
}

# The browser and the isolated sub-agent (WI-1358). Both are stdio servers the
# klaudia binary already serves — `klaudia mcp browser` (WI-1137) and
# `klaudia mcp codex-subagent` (WI-1138) — and Codex is the CLI that has neither
# of its own. Claude and Klaudia carry a browser inside the binary, so only
# Codex's config is written here.
#
# Nothing secret goes in the file: the command and its arguments only. The
# sub-agent's `--codex` must be the real binary, not `codex` on PATH — on a Vogt
# pod that name is the full-access wrapper, which prepends
# `--dangerously-bypass-approvals-and-sandbox` and so would hand every child an
# unsandboxed shell (WI-1138). `--root` is this session's own working directory,
# the one confinement the server applies when the engine has not cut a worktree.
codex_klaudia_tools() {
    local config="$1" lockdir klaudia
    command -v codex >/dev/null 2>&1 || return 0
    klaudia="$(command -v klaudia 2>/dev/null || true)"
    [[ -n "$klaudia" ]] || return 0
    lockdir="$(dirname "$config")"
    mkdir -p "$lockdir"
    (
        flock 9
        CODEX_CONFIG="$config" CODEX_KLAUDIA="$klaudia" CODEX_REAL="${CODEX_REAL:-/usr/local/libexec/codex-real}" CODEX_ROOT="$PWD" \
            python3 - <<'PY'
import os, sys, tempfile

path = os.environ["CODEX_CONFIG"]
klaudia = os.environ["CODEX_KLAUDIA"]
real = os.environ["CODEX_REAL"]
root = os.environ["CODEX_ROOT"]

def approval_row(source: str) -> str:
    # The table is replaced whole, so carry an operator's explicit mode
    # (WI-1634) rather than resetting it to "approve" on the next start.
    for row in source.splitlines():
        head = row.split("#", 1)[0]
        if head.split("=", 1)[0].strip() == "default_tools_approval_mode":
            return row if row.endswith("\n") else row + "\n"
    return 'default_tools_approval_mode = "approve"\n'

def block(name: str, args: list[str], extra: str = "", mode: str = "") -> str:
    rendered = ", ".join(f'"{arg}"' for arg in args)
    # Pre-approved for driven sessions (WI-1634); the bundle below would add
    # it anyway, so writing it here keeps reruns quiet. The key lives in the
    # session's own config.toml, which the session can edit, so it is a
    # convenience for autonomous work and not a security boundary.
    return (
        f"[mcp_servers.{name}]\n"
        f'command = "{klaudia}"\n'
        f"args = [{rendered}]\n"
        f"{extra}"
        f"{mode or 'default_tools_approval_mode = "approve"\n'}"
    )

wanted = {
    "browser": block("browser", ["mcp", "browser"], "tool_timeout_sec = 60\n"),
    # Codex starts a stdio server with a cleared environment (HOME, PATH and a
    # few more) plus the names in env_vars. The child `codex exec` the
    # sub-agent spawns authenticates to the same provider, whose env_key is
    # HOMELAB_LITELLM_SESSION_KEY, so that name is passed through by name; the
    # value never lands in the file (WI-1639).
    "codex-subagent": block(
        "codex-subagent",
        ["mcp", "codex-subagent", "--codex", real, "--root", root],
        'env_vars = ["HOMELAB_LITELLM_SESSION_KEY"]\n',
    ),
}

try:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
except FileNotFoundError:
    text = ""
except OSError as exc:
    print(f"mcp-bootstrap: cannot read {path} ({exc}); left alone", file=sys.stderr)
    sys.exit(1)

if text.strip():
    try:
        import tomllib
        tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        print(
            f"mcp-bootstrap: {path} is not valid TOML ({exc}); left alone",
            file=sys.stderr,
        )
        sys.exit(1)

lines = text.splitlines(keepends=True)
kept: list[str] = []
existing: dict[str, str] = {}
index = 0
while index < len(lines):
    stripped = lines[index].strip()
    header = next(
        (name for name in wanted if stripped == f"[mcp_servers.{name}]"
         or stripped.startswith(f"[mcp_servers.{name}].")),
        None,
    )
    if header is not None:
        index += 1
        while index < len(lines) and not lines[index].lstrip().startswith("["):
            if stripped == f"[mcp_servers.{header}]":
                existing[header] = existing.get(header, "") + lines[index]
            index += 1
        continue
    kept.append(lines[index])
    index += 1

for name, mode in existing.items():
    current = wanted[name]
    wanted[name] = current.replace(
        'default_tools_approval_mode = "approve"\n', approval_row(mode), 1
    )

body = "".join(kept).rstrip("\n")
addition = "\n\n".join(wanted[name] for name in ("browser", "codex-subagent"))
new = f"{body}\n\n{addition}\n" if body else addition + "\n"
if new == text:
    sys.exit(0)
directory = os.path.dirname(path) or "."
fd, tmp = tempfile.mkstemp(prefix=".config.toml.", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(new)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
except BaseException:
    os.unlink(tmp)
    raise
PY
    ) 9>"$lockdir/.config.toml.lock"
}

# The driven-session bundle (WI-1149). A Codex the engine starts is not an
# operator's interactive CLI, so the config it reads is shaped for that:
#
#   - project_doc_fallback_filenames = ["CLAUDE.md"], so a checkout that only
#     carries CLAUDE.md still has instructions. project_doc_mode = "all" and the
#     claude-compat provider are the fork's job (WI-1131, WI-1132), not this one.
#   - [tui] animations = false and terminal_title = ["agent-state"]: a driven
#     session's title is state the engine reads, and animation bytes are noise.
#   - [features] memories = true, beside the daemon_auto_start key the other
#     writer forces off.
#
# Skills are a symlink, not a config key. [skills] extra_roots is the fork's
# (WI-1133); until then ~/.agents/skills points at the workspace .claude/skills
# when that directory exists, and otherwise at ~/.claude/skills. An existing
# directory or symlink is left where it is.
#
# vogt and cadastre publish tool schemas larger than Codex's 5,000 byte default,
# so a table this script can see for either server gets
# tool_input_schema_max_bytes raised. A server an operator never registered is
# not invented here, and a table that already carries a larger cap is not
# lowered. A config that does not parse is left alone, like every writer above.
#
# The estate's trusted servers get default_tools_approval_mode = "approve"
# (WI-1634, operator-approved 2026-10-15). A driven Codex runs `-a on-request`,
# so every MCP call otherwise stops at a permission prompt only a person may
# answer, and an autonomous session cannot use the estate's tools at all. The
# fork's per-server key (codex-rs/config mcp_types.rs AppToolApproval, read by
# requires_mcp_tool_approval_for_mode in core mcp_tool_call.rs) skips the
# prompt for that server only; requirements.toml cannot carry it, its
# [mcp_servers] is an identity allowlist. The list mirrors what the Claude
# driven-session policy lets a Claude session call without a person: vogt and
# cadastre (the session's own scoped token decides what it may write), the
# klaudia browser and isolated sub-agent, and the three read-only forge and
# Grafana wrappers. komodo is deliberately absent, because its deploy_stack and
# write_stack_file mutate running stacks, and so is every server not named
# here: those keep the person gate. A table an operator gave an explicit mode
# keeps it, and a server nobody registered is not created. The key lives in the
# session's own ~/.codex/config.toml, which the session can edit, so it is a
# convenience for autonomous work and not a security boundary; the engine's
# person gate (WI-1355) is the boundary.
readonly CODEX_SCHEMA_MAX_BYTES="${CODEX_SCHEMA_MAX_BYTES:-200000}"

codex_driven_session() {
    local config="$1" lockdir skills
    lockdir="$(dirname "$config")"
    mkdir -p "$lockdir"
    skills="${CODEX_SKILLS_LINK:-$HOME/.agents/skills}"
    if [[ -d "$PWD/.claude/skills" ]]; then
        _codex_link_skills "$skills" "$PWD/.claude/skills"
    elif [[ -d "$HOME/.claude/skills" ]]; then
        _codex_link_skills "$skills" "$HOME/.claude/skills"
    fi
    (
        flock 9
        CODEX_CONFIG="$config" CODEX_SCHEMA_MAX="$CODEX_SCHEMA_MAX_BYTES" python3 - <<'PY'
import os, sys, tempfile

path = os.environ["CODEX_CONFIG"]
schema_max = int(os.environ["CODEX_SCHEMA_MAX"])
try:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
except FileNotFoundError:
    text = ""
except OSError as exc:
    print(f"mcp-bootstrap: cannot read {path} ({exc}); left alone", file=sys.stderr)
    sys.exit(1)

if text.strip():
    try:
        import tomllib
        tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        print(
            f"mcp-bootstrap: {path} is not valid TOML ({exc}); left alone",
            file=sys.stderr,
        )
        sys.exit(1)

ROOT = {
    "project_doc_fallback_filenames": 'project_doc_fallback_filenames = ["CLAUDE.md"]\n',
}
TABLES = {
    "tui": 'animations = false\nterminal_title = ["agent-state"]\n',
    "features": "memories = true\n",
}
SCHEMA_SERVERS = ("vogt", "cadastre")
APPROVED_SERVERS = (
    "vogt",
    "cadastre",
    "codex-subagent",
    "browser",
    "github-ro",
    "forgejo-ro",
    "grafana-ro",
)
APPROVAL_ROW = 'default_tools_approval_mode = "approve"\n'

def key_of(row: str) -> str:
    head = row.split("#", 1)[0]
    return head.split("=", 1)[0].strip() if "=" in head else ""

def render(rows: list[str]) -> str:
    body = "".join(rows).rstrip("\n")
    return f"{body}\n" if body else ""

def header_line(row: str) -> bool:
    # A value such as terminal_title = ["agent-state"] is not a table header.
    text_row = row.lstrip()
    return text_row.startswith("[") and '"' not in text_row.split("]", 1)[0]

lines = text.splitlines(keepends=True)
out: list[str] = []
index = 0
seen_tables: set[str] = set()
seen_root: set[str] = set()
while index < len(lines):
    stripped = lines[index].strip()
    table = next((name for name in TABLES if stripped.lstrip() == f"[{name}]"), None)
    if table is not None:
        seen_tables.add(table)
        wanted = {
            row.split("=", 1)[0].strip(): row
            for row in TABLES[table].splitlines(keepends=True)
        }
        out.append(lines[index])
        index += 1
        while index < len(lines) and not header_line(lines[index]):
            row = lines[index]
            key = key_of(row)
            if key in wanted:
                out.append(wanted.pop(key))
            else:
                out.append(row)
            index += 1
        while out and out[-1].strip() == "":
            out.pop()
        out.extend(wanted.values())
        if index < len(lines) and header_line(lines[index]):
            out.append("\n")
        continue
    server = next(
        (name for name in APPROVED_SERVERS if stripped == f"[mcp_servers.{name}]"),
        None,
    )
    if server is not None:
        out.append(lines[index])
        index += 1
        present = None
        approval = False
        while index < len(lines) and not header_line(lines[index]):
            row = lines[index]
            if server in SCHEMA_SERVERS and key_of(row) == "tool_input_schema_max_bytes":
                present = row
            else:
                approval = approval or key_of(row) == "default_tools_approval_mode"
                out.append(row)
            index += 1
        while out and out[-1].strip() == "":
            out.pop()
        # Before the cap, which is always lifted and re-appended last: the
        # other order would move a line on every run and rewrite the file.
        if not approval:
            out.append(APPROVAL_ROW)
        if server not in SCHEMA_SERVERS:
            if index < len(lines) and header_line(lines[index]):
                out.append("\n")
            continue
        keep = present
        if present is not None:
            try:
                if int(present.split("=", 1)[1].strip()) < schema_max:
                    keep = None
            except ValueError:
                pass
        # The cap line was lifted out of the table, so the blank line that
        # separated it from the next header has to come back with it. Without
        # that, a kept cap sits flush against the following [table].
        while out and out[-1].strip() == "":
            out.pop()
        out.append(keep or f"tool_input_schema_max_bytes = {schema_max}\n")
        if index < len(lines) and header_line(lines[index]):
            out.append("\n")
        continue
    root_key = key_of(lines[index])
    if root_key in ROOT:
        seen_root.add(root_key)
        out.append(ROOT[root_key])
        index += 1
        continue
    out.append(lines[index])
    index += 1

missing_root = [row for key, row in ROOT.items() if key not in seen_root]
if missing_root:
    split_at = next(
        (i for i, row in enumerate(out) if header_line(row)),
        len(out),
    )
    out[split_at:split_at] = missing_root

missing_tables = [
    f"[{name}]\n{TABLES[name]}" for name in TABLES if name not in seen_tables
]
if missing_tables:
    split_at = next(
        (i for i, row in enumerate(out) if header_line(row)),
        len(out),
    )
    head = render(out[:split_at])
    tail = render(out[split_at:])
    parts = [part.rstrip("\n") for part in (head, *missing_tables, tail) if part.strip()]
    new = "\n\n".join(parts) + "\n"
else:
    new = render(out)

if new == text:
    sys.exit(0)
directory = os.path.dirname(path) or "."
fd, tmp = tempfile.mkstemp(prefix=".config.toml.", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(new)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
except BaseException:
    os.unlink(tmp)
    raise
PY
    ) 9>"$lockdir/.config.toml.lock"
}

# Point the skills link at target, but only when nothing is there yet. A
# directory or a symlink an operator already made is theirs.
_codex_link_skills() {
    local link="$1" target="$2"
    [[ -e "$link" || -L "$link" ]] && return 0
    mkdir -p "$(dirname "$link")"
    ln -s "$target" "$link" 2>/dev/null || true
}

# Vogt registrations are best-effort: a failure here must not cost an agent its
# git/gh credentials.
install_vogt_bridge
install_vogt_codex
install_vogt_claude
install_vogt_opencode
install_vogt_klaudia || printf 'mcp-bootstrap: klaudia registration of vogt failed\n' >&2
if command -v codex >/dev/null 2>&1; then
    codex_disable_daemon "${CODEX_HOME:-$HOME/.codex}/config.toml" \
        || printf 'mcp-bootstrap: could not disable the Codex app-server daemon\n' >&2
    codex_auto_compact "${CODEX_HOME:-$HOME/.codex}/config.toml" \
        || printf 'mcp-bootstrap: could not write the Codex auto-compact limit\n' >&2
    codex_litellm_provider "${CODEX_HOME:-$HOME/.codex}/config.toml" \
        || printf 'mcp-bootstrap: could not write the Codex LiteLLM provider\n' >&2
    codex_klaudia_tools "${CODEX_HOME:-$HOME/.codex}/config.toml" \
        || printf 'mcp-bootstrap: could not register the Codex browser and sub-agent\n' >&2
    codex_driven_session "${CODEX_HOME:-$HOME/.codex}/config.toml" \
        || printf 'mcp-bootstrap: could not write the Codex driven-session bundle\n' >&2
fi
install_readonly_servers

# stderr, like every other message in this script, because of who calls it.
#
# `vogt-agent-auth run` invokes this bootstrap on every launch, and one of
# the things it launches is a stdio MCP server — `vogt-mcp`. For a
# stdio MCP server, stdout *is* the transport, so a line here is not a banner:
# it is the first frame of the protocol, and it does not parse. `tests/test_bridge.py`
# already forbids this of the bridge — "a diagnostic on stdout corrupts framing
# and looks like a client bug" — and the bridge obeys it. The launcher above it
# did not, so the rule held in the one place that was tested and broke in the
# layer that wraps it.
#
# Everything an operator wants to see is still shown: an interactive shell
# prints stderr too. The only reader that notices the difference is the one
# that must.
printf 'mcp-bootstrap: Vogt MCP client registrations written; endpoint was not probed — run `vogt-agent-auth check` for that\n' >&2
