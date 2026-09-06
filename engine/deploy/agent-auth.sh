#!/usr/bin/env bash
# Reference agent-auth helper: load service credentials on demand from a
# secrets manager for agent commands, without persisting tokens to PID 1.
#
# This is one *example* of an `ENGINE_AGENT_AUTH_HELPER`, not a required part
# of Vogt. It is written against Infisical, and it is entirely data-driven: it
# bakes in no address, project, secret name or service list. A deployment that
# wants credential brokering points `ENGINE_AGENT_AUTH_HELPER` at this script
# and describes its own secrets through the environment (below); a deployment
# that does not simply leaves the helper unselected, and the entrypoint skips
# it. A clean clone running the image with only a token needs none of this.
#
# What it reads from the environment when it is actually invoked (never at
# import — a stranger sourcing this file to reuse `probe_mcp` gets no failure):
#
#   INFISICAL_API_URL              required — the secrets-manager API
#   INFISICAL_CLIENT_ID/_SECRET    required — the machine identity
#   ENGINE_INFISICAL_ENV           Infisical environment slug (default: prod)
#   ENGINE_AGENT_AUTH_SECRETS      the secrets to load, one per line:
#                                    VAR PROJECT_ID SECRET_NAME [flags]
#                                  where flags is a comma-separated list drawn
#                                  from {optional, ondemand, writable}. Each is
#                                  fetched and exported as VAR; a missing
#                                  required secret fails, naming the secret.
#                                  `ondemand` declares VAR for sessions but
#                                  does NOT fetch it at launch: it is reachable
#                                  only through the engine's broker, on request
#                                  (`fetch`, below), and every fetch is audited.
#                                  `writable` lets a session *store* VAR through
#                                  the broker (`store`/`set`, below), audited by
#                                  name; it is orthogonal to the read policy.
#   ENGINE_AGENT_AUTH_GH_TOKEN_FROM  a VAR from the manifest to also export as
#                                    GH_TOKEN (the git/gh default identity).
#   ENGINE_AGENT_AUTH_PROBES       optional `check` probes, one per line:
#                                    NAME URL TOKEN_VAR   (a bearer GET)
#
# Vogt and Cadastre are handled directly rather than through the manifest,
# because their credential is brokered as a *file* and Cadastre is an explicit
# opt-in (NFR-O5):
#
#   MYDEVENV2_VOGT_SECRET_NAME     the Vogt token secret for this instance
#   MYDEVENV2_CADASTRE_SECRET_NAME the Cadastre token secret (opt-in only)
#   ENGINE_AGENT_AUTH_TOKEN_PROJECT_ID  the project holding those two secrets
#   CADASTRE_MCP_ENABLED           "1" turns the Cadastre integration on
#   CADASTRE_MCP_URL               the Cadastre MCP endpoint (no default)
#
# The contract this helper gives a session (#511, #566): a session gets exactly
# the manifest — every secret above, resolved once at launch and exported as a
# brokered token — and nothing more. The machine identity that could read the
# rest of the vault is dropped before the shell/command starts, so an agent's
# own "log in to the secrets manager and fetch a secret" step finds
# INFISICAL_CLIENT_ID empty *by design*, not because the credential is missing.
# Two names-only breadcrumbs mark the boundary for tooling that needs to tell
# those apart: `AGENT_AUTH_MODE=brokered` and `AGENT_AUTH_GRANTED`, the
# space-separated names (never values) of the credential variables granted. To
# make a new secret reachable from sessions, add a line to
# ENGINE_AGENT_AUTH_SECRETS — do not reach for the identity. (The strip is a
# propagation boundary between processes, not a kernel one: a same-uid process
# can still read PID 1's environment via /proc/1/environ; the identity is out
# of the manifest's reach, not out of the machine's.)
#
# The on-demand broker (#568) is the one sanctioned way to get a manifest
# secret *after* launch, and it has two halves in this file:
#
#   get VAR      engine-side. Run by the engine, which holds the identity,
#                with the same re-granted environment a session launch gets.
#                Resolves exactly one manifest entry and prints its value —
#                nothing else — to stdout. Refuses a VAR not in the manifest.
#   fetch VAR    session-side. Runs inside a session, which holds no identity,
#                only MYDEVENV2_BROKER_URL and MYDEVENV2_BROKER_TOKEN (minted
#                by the engine for that session). Asks the engine's
#                `POST /api/agent-auth/fetch/VAR` and prints the value. The
#                engine enforces the manifest, rate-limits, and audits the
#                fetch by session and name, never value.
#
# The write mirror (#598), for a session that has just *produced* a secret:
#
#   set VAR      engine-side. Reads the value on stdin (never argv), re-checks
#                the manifest and the `writable` flag, upserts, and prints
#                `created`/`updated`. Used by the engine's store broker.
#   store VAR    session-side. Sends a value on stdin to the engine's
#                `POST /api/agent-auth/store/VAR`; the engine enforces the
#                manifest and the `writable` flag, rate-limits, and audits the
#                write by session and name, never value.
#
# A session's environment therefore says which names it may ask for, in
# `AGENT_AUTH_ONDEMAND`, and which it may store, in `AGENT_AUTH_WRITABLE`,
# beside what it already holds in `AGENT_AUTH_GRANTED`.

set -euo pipefail

readonly INFISICAL_ENV="${ENGINE_INFISICAL_ENV:-prod}"
# Cadastre is an explicit private-stack integration, not a prerequisite for an
# authenticated shell or for Vogt. Only fetched when the stack opts in.
readonly CADASTRE_MCP_ENABLED="${CADASTRE_MCP_ENABLED:-0}"
readonly CADASTRE_SECRET_NAME="${MYDEVENV2_CADASTRE_SECRET_NAME:-}"
readonly VOGT_SECRET_NAME="${MYDEVENV2_VOGT_SECRET_NAME:-}"
readonly TOKEN_PROJECT_ID="${ENGINE_AGENT_AUTH_TOKEN_PROJECT_ID:-}"
# The front door on loopback, the same default `vogt-mcp-auth.sh` uses: in the
# merged stack the engine is the only published port (NFR-D11) and this runs
# inside that container, so loopback needs no DNS and no certificate. A
# session's own `VOGT_URL` still wins where one is set.
readonly DEFAULT_VOGT_URL="http://127.0.0.1:8910"
AUTH_TMP_DIR=""

# Names of the credential variables this helper grants the launched session,
# accumulated as each is exported. Published as the `AGENT_AUTH_GRANTED`
# breadcrumb (names only, never values) just before the brokering identity is
# dropped, so a session that follows the near-universal "log in to the secrets
# manager, fetch what you need" pattern sees the brokered model instead of an
# empty INFISICAL_CLIENT_ID and the wrong conclusion that the credential does
# not exist (#566).
AGENT_AUTH_GRANTED_VARS=""
# Names declared `ondemand`: reachable through the broker, never exported at
# launch. Published as the `AGENT_AUTH_ONDEMAND` breadcrumb.
AGENT_AUTH_ONDEMAND_VARS=""
# Names flagged `writable`: a session may *store* these through the engine's
# broker (#598). Published as the `AGENT_AUTH_WRITABLE` breadcrumb, so a session
# knows what it may write rather than guessing and being refused.
AGENT_AUTH_WRITABLE_VARS=""
# Set when a manifest entry matched ENGINE_AGENT_AUTH_GH_TOKEN_FROM and GH_TOKEN
# was aliased from it. Checked after the manifest load: a GH_TOKEN_FROM that
# names no manifest entry aliased nothing and, before #566, did so in silence.
GH_TOKEN_ALIASED=0

# Record a granted credential variable name for the breadcrumb. Names only:
# no value ever leaves this helper through it.
grant_var() {
    AGENT_AUTH_GRANTED_VARS+="${AGENT_AUTH_GRANTED_VARS:+ }$1"
}

cleanup_auth_artifacts() {
    if [[ -n "$AUTH_TMP_DIR" && -d "$AUTH_TMP_DIR" ]]; then
        rm -rf "$AUTH_TMP_DIR"
    fi
}

usage() {
    cat <<'EOF'
Usage:
  mydevenv2-agent-auth check
  mydevenv2-agent-auth run -- <command> [args...]
  mydevenv2-agent-auth shell
  mydevenv2-agent-auth get <VAR>
  mydevenv2-agent-auth fetch <VAR>
  mydevenv2-agent-auth set <VAR>     (value on stdin)
  mydevenv2-agent-auth store <VAR>   (value on stdin)

Commands:
  check  Fetch credentials and validate secrets-manager access, any configured
         service probes, and the Cadastre and Vogt MCP endpoints.
  run    Execute one command with service credentials on demand in memory.
  shell  Start a login shell with service credentials on demand in memory.
  get    (engine-side) Resolve one ENGINE_AGENT_AUTH_SECRETS entry with the
         machine identity and print its value to stdout. Used by the engine's
         on-demand broker; needs the identity, so it cannot run in a session.
  fetch  (session-side) Ask the engine's broker for one manifest secret and
         print its value to stdout. Needs only the session's broker token.
  set    (engine-side) Store one `writable` ENGINE_AGENT_AUTH_SECRETS entry,
         reading the value from stdin and printing created/updated. Used by the
         engine's store broker; needs the identity, so it cannot run in a
         session.
  store  (session-side) Send a value on stdin to the engine's broker to store
         under one `writable` manifest entry. Needs only the broker token.
EOF
}

die() {
    printf 'mydevenv2-agent-auth: %s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

require_identity() {
    [[ -n "${INFISICAL_API_URL:-}" ]] || die \
        "INFISICAL_API_URL is not set; the Infisical agent-auth helper needs the secrets-manager API URL"
    [[ -n "${INFISICAL_CLIENT_ID:-}" ]] || die \
        "INFISICAL_CLIENT_ID is not set; the Infisical agent-auth helper needs a machine identity"
    [[ -n "${INFISICAL_CLIENT_SECRET:-}" ]] || die \
        "INFISICAL_CLIENT_SECRET is not set; the Infisical agent-auth helper needs a machine identity"
}

probe_mcp() {
    local service="$1" url="$2" token="$3" credential="$4"
    local response_file="$5" error_file="$6" failure_hint="$7"
    local status detail
    shift 7

    # One JSON-RPC-aware probe serves every MCP endpoint. Keep the response
    # files caller-owned so credentials and refusal details remain in the
    # protected agent-auth temporary lifecycle.
    : >"$response_file"
    : >"$error_file"
    status="$(curl -sS --max-time 15 "$@" \
        -o "$response_file" -w '%{http_code}' \
        -H "Authorization: Bearer $token" \
        -H 'Content-Type: application/json' \
        -H 'Accept: application/json, text/event-stream' \
        --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"mydevenv2-agent-auth","version":"1"}}}' \
        "$url" 2>"$error_file")" || status="000"
    detail="$(tr -d '\r\n' <"$response_file" | cut -c1-300)"

    if [[ "$status" == "000" ]]; then
        die "$service is unreachable at $url: $(tr -d '\r\n' <"$error_file" | cut -c1-200)"
    elif [[ "$status" != "200" ]]; then
        die "$service rejected $credential at $url (HTTP $status): ${detail:-<empty body>}$failure_hint"
    elif [[ "$detail" == *'"error"'* ]]; then
        # JSON-RPC refusals normally ride on HTTP 200. Treating transport
        # success as service success is the false green this helper prevents.
        die "$service answered at $url but refused the handshake: $detail$failure_hint"
    fi
    printf 'ok: %s (%s)\n' "$service" "$url"
}

mint_access_token() {
    local temp_home token
    temp_home="$(mktemp -d)"
    if ! token="$(HOME="$temp_home" infisical login \
        --method universal-auth \
        --domain "${INFISICAL_API_URL:-}" \
        --client-id "$INFISICAL_CLIENT_ID" \
        --client-secret "$INFISICAL_CLIENT_SECRET" \
        --plain --silent)"; then
        rm -rf "$temp_home"
        return 1
    fi
    rm -rf "$temp_home"
    printf '%s' "$token"
}

get_secret() {
    local access_token="$1" project_id="$2" secret_name="$3"
    infisical secrets get "$secret_name" \
        --domain "${INFISICAL_API_URL:-}" \
        --projectId "$project_id" \
        --env "$INFISICAL_ENV" \
        --token "$access_token" \
        --plain --silent
}

# Write one secret value, reading the value from stdin (never argv — this is the
# pluggable write half of the broker, mirroring `get_secret`; a different
# secrets manager replaces exactly this function). #598.
write_secret() {
    local access_token="$1" project_id="$2" secret_name="$3" value
    value="$(cat)"
    infisical secrets set "$secret_name=$value" \
        --domain "${INFISICAL_API_URL:-}" \
        --projectId "$project_id" \
        --env "$INFISICAL_ENV" \
        --token "$access_token" \
        --silent
}

# True when a comma-separated flag list contains a given flag. The flag field
# of a manifest line is now a list — `ondemand,writable` — parsed the same way
# here and in the engine's `parse_flags` so the two cannot disagree (#598).
flags_contain() {
    local list="$1" want="$2"
    case ",${list}," in
        *",${want},"*) return 0 ;;
        *) return 1 ;;
    esac
}

# Reject an unknown flag or two conflicting policy flags, mirroring the engine's
# `parse_flags` so a bad manifest fails the launch here as it does there (#598).
validate_flags() {
    local list="$1" line="$2" flag seen_policy=""
    [[ -n "$list" ]] || return 0
    local -a parts
    IFS=',' read -ra parts <<<"$list"
    for flag in "${parts[@]}"; do
        case "$flag" in
            required | optional | ondemand)
                if [[ -n "$seen_policy" && "$seen_policy" != "$flag" ]]; then
                    die "ENGINE_AGENT_AUTH_SECRETS entry sets conflicting policy flags: $line"
                fi
                seen_policy="$flag"
                ;;
            writable) ;;
            *)
                die "ENGINE_AGENT_AUTH_SECRETS entry has unknown flag '$flag' (flags are a comma-separated list of optional, ondemand, writable): $line"
                ;;
        esac
    done
}

# Print `PROJECT_ID SECRET_NAME FLAGS` for one manifest variable, or return 1
# when the manifest does not declare it. The one place the manifest is read
# for a *single* name; the launch-time loop below reads it for all of them.
manifest_entry() {
    local want="$1" line var project name flag
    [[ -n "${ENGINE_AGENT_AUTH_SECRETS:-}" ]] || return 1
    while IFS= read -r line; do
        line="${line%%#*}"
        read -r var project name flag <<<"$line"
        [[ "${var:-}" == "$want" ]] || continue
        [[ -n "${project:-}" && -n "${name:-}" ]] || die \
            "malformed ENGINE_AGENT_AUTH_SECRETS entry (want 'VAR PROJECT_ID SECRET_NAME [flags]'): $line"
        validate_flags "${flag:-}" "$line"
        printf '%s %s %s' "$project" "$name" "${flag:-required}"
        return 0
    done <<<"${ENGINE_AGENT_AUTH_SECRETS}"
    return 1
}

# `get VAR`: the engine's half of the on-demand broker (#568). The engine
# holds the identity and runs this with the same re-granted environment a
# session launch gets, after enforcing the manifest itself; this enforces it
# again because the helper is the part that is pluggable and must stand
# alone. stdout *is* the value, so nothing else is ever printed there.
get_one() {
    local var="$1" entry project name flag access_token value
    entry="$(manifest_entry "$var")" || die \
        "$var is not a variable in ENGINE_AGENT_AUTH_SECRETS; nothing is fetched that the deployment did not declare"
    read -r project name flag <<<"$entry"
    require_identity
    require_command infisical
    access_token="$(mint_access_token)" || die "Infisical universal-auth login failed"
    value="$(get_secret "$access_token" "$project" "$name" || true)"
    [[ -n "$value" ]] || die "Infisical secret $name is missing or empty"
    printf '%s' "$value"
}

# `set VAR`: the engine's half of the *write* broker (#598). The engine holds
# the identity and runs this with the same re-granted environment a session
# launch gets, after enforcing the manifest and the `writable` flag itself;
# this enforces both again because the helper is the pluggable part and must
# stand alone. The value is read from stdin — never argv, so `ps`, shell
# history and audit excerpts never see it — and stdout is only `created` or
# `updated`, nothing else.
set_one() {
    local var="$1" entry project name flag access_token value existing
    entry="$(manifest_entry "$var")" || die \
        "$var is not a variable in ENGINE_AGENT_AUTH_SECRETS; nothing is stored that the deployment did not declare"
    read -r project name flag <<<"$entry"
    flags_contain "${flag:-}" writable || die \
        "$var is declared in ENGINE_AGENT_AUTH_SECRETS but not writable; add the 'writable' flag to its line (e.g. 'ondemand,writable') to let sessions store it"
    value="$(cat)"
    [[ -n "$value" ]] || die "no value on stdin to store for $var"
    require_identity
    require_command infisical
    access_token="$(mint_access_token)" || die "Infisical universal-auth login failed"
    # Create vs update is the one thing the caller cares to hear back; decide it
    # by whether the secret already resolves, then upsert.
    existing="$(get_secret "$access_token" "$project" "$name" 2>/dev/null || true)"
    printf '%s' "$value" | write_secret "$access_token" "$project" "$name" >/dev/null \
        || die "Infisical secret $name could not be stored"
    if [[ -n "$existing" ]]; then printf 'updated'; else printf 'created'; fi
}

# `store VAR`: the session's half of the write broker (#598). Runs *inside* a
# session, which holds no identity — only the broker address and token — and
# POSTs the value (read from stdin, never argv) to the engine's store route.
# Every refusal repeats the engine's own reason, which for an undeclared or
# non-writable name is the manifest line or flag to add.
store_one() {
    local var="$1" value response status body
    [[ -n "${MYDEVENV2_BROKER_URL:-}" && -n "${MYDEVENV2_BROKER_TOKEN:-}" ]] || die \
        "no secret broker in this environment (MYDEVENV2_BROKER_URL/_TOKEN are unset): not an engine session, or the engine declares nothing in ENGINE_AGENT_AUTH_SECRETS"
    require_command curl
    value="$(cat)"
    [[ -n "$value" ]] || die "no value on stdin to store for $var"
    umask 077
    response="$(mktemp)"
    status="$(printf '%s' "$value" | curl -sS --max-time 45 -o "$response" -w '%{http_code}' \
        -X POST -H "Authorization: Bearer $MYDEVENV2_BROKER_TOKEN" \
        -H 'Content-Type: text/plain' --data-binary @- \
        "${MYDEVENV2_BROKER_URL%/}/api/agent-auth/store/$var" 2>/dev/null)" || status="000"
    if [[ "$status" == "200" ]]; then
        cat "$response"
        rm -f "$response"
        return 0
    fi
    body="$(tr -d '\r\n' <"$response" | cut -c1-300)"
    rm -f "$response"
    case "$status" in
        000) die "secret broker unreachable at $MYDEVENV2_BROKER_URL" ;;
        401) die "secret broker refused this session's token (HTTP 401); the engine no longer knows this session" ;;
        403) die "secret broker refused storing $var (HTTP 403): ${body:-not writable in the manifest}" ;;
        413) die "secret broker refused storing $var (HTTP 413): value over the size cap" ;;
        *) die "secret broker failed storing $var (HTTP $status): ${body:-<empty body>}" ;;
    esac
}

# `fetch VAR`: the session's half of the broker (#568). Runs *inside* a
# session, which holds no identity — only the broker address and the token
# the engine minted for this session — and asks the engine for one manifest
# secret. Prints the value verbatim to stdout. Every refusal repeats the
# engine's own reason, which for an undeclared name is the manifest line to
# add; the identity is never involved on this side.
fetch_one() {
    local var="$1" response status body
    [[ -n "${MYDEVENV2_BROKER_URL:-}" && -n "${MYDEVENV2_BROKER_TOKEN:-}" ]] || die \
        "no secret broker in this environment (MYDEVENV2_BROKER_URL/_TOKEN are unset): not an engine session, or the engine declares nothing in ENGINE_AGENT_AUTH_SECRETS"
    require_command curl
    umask 077
    response="$(mktemp)"
    status="$(curl -sS --max-time 45 -o "$response" -w '%{http_code}' \
        -X POST -H "Authorization: Bearer $MYDEVENV2_BROKER_TOKEN" \
        "${MYDEVENV2_BROKER_URL%/}/api/agent-auth/fetch/$var" 2>/dev/null)" || status="000"
    if [[ "$status" == "200" ]]; then
        cat "$response"
        rm -f "$response"
        return 0
    fi
    body="$(tr -d '\r\n' <"$response" | cut -c1-300)"
    rm -f "$response"
    case "$status" in
        000) die "secret broker unreachable at $MYDEVENV2_BROKER_URL" ;;
        401) die "secret broker refused this session's token (HTTP 401); the engine no longer knows this session" ;;
        403) die "secret broker refused $var (HTTP 403): ${body:-not in the manifest}" ;;
        *) die "secret broker failed for $var (HTTP $status): ${body:-<empty body>}" ;;
    esac
}

# Load every secret named in ENGINE_AGENT_AUTH_SECRETS, export it under its
# manifest variable, and optionally alias one of them to GH_TOKEN.
#
# Assign, check, then export — never `export VAR="$(...)"`. `export` is a
# command with its own exit status, and it succeeds; under `set -e` a
# `get_secret` that fails inside that substitution would leave the variable
# empty and be swallowed, surfacing later as a 401 from whichever service is
# asked first — a long way from the secret store that was actually
# unavailable. `printf -v` assigns first, so the emptiness guard below is real.
load_manifest_secrets() {
    local access_token="$1"
    local line var project name flag value
    [[ -n "${ENGINE_AGENT_AUTH_SECRETS:-}" ]] || return 0
    while IFS= read -r line; do
        line="${line%%#*}"
        read -r var project name flag <<<"$line"
        [[ -n "${var:-}" ]] || continue
        [[ -n "${project:-}" && -n "${name:-}" ]] || die \
            "malformed ENGINE_AGENT_AUTH_SECRETS entry (want 'VAR PROJECT_ID SECRET_NAME [flags]'): $line"
        validate_flags "${flag:-}" "$line"
        # `writable` is orthogonal to the read policy: record it for the
        # breadcrumb whether the entry is launched or ondemand (#598).
        if flags_contain "${flag:-}" writable; then
            AGENT_AUTH_WRITABLE_VARS+="${AGENT_AUTH_WRITABLE_VARS:+ }$var"
        fi
        if flags_contain "${flag:-}" ondemand; then
            # Declared for sessions, deliberately not resolved here (#568): it
            # never sits in the session's environment for the whole session.
            # Listed by name so the session knows what it may ask for.
            [[ "$var" != "${ENGINE_AGENT_AUTH_GH_TOKEN_FROM:-}" ]] || die \
                "ENGINE_AGENT_AUTH_GH_TOKEN_FROM names '$var', which is an ondemand entry; the git/gh identity must be resolved at launch"
            AGENT_AUTH_ONDEMAND_VARS+="${AGENT_AUTH_ONDEMAND_VARS:+ }$var"
            continue
        fi
        value="$(get_secret "$access_token" "$project" "$name" || true)"
        if [[ -z "$value" ]] && ! flags_contain "${flag:-}" optional; then
            die "Infisical secret $name is missing or empty"
        fi
        printf -v "$var" '%s' "$value"
        export "${var?}"
        [[ -n "$value" ]] && grant_var "$var"
        if [[ "$var" == "${ENGINE_AGENT_AUTH_GH_TOKEN_FROM:-}" ]]; then
            # The name matched a manifest entry, which is what the post-load
            # check below verifies; the alias is still worth only a non-empty
            # value.
            GH_TOKEN_ALIASED=1
            if [[ -n "$value" ]]; then
                export GH_TOKEN="$value"
                grant_var GH_TOKEN
            fi
        fi
    done <<<"${ENGINE_AGENT_AUTH_SECRETS}"
}

load_agent_environment() {
    require_identity
    require_command infisical
    local access_token
    access_token="$(mint_access_token)" || die "Infisical universal-auth login failed"

    # Retired release-automation names whose values are revoked. If they are
    # inherited they masquerade as a GitHub credential and every `gh` call
    # fails with "Bad credentials"; clear them so a stale value cannot outlive
    # a fresh login. GH_TOKEN is (re)set from the manifest alias below.
    unset GITHUB_PAT GH_RELEASE_TOKEN
    load_manifest_secrets "$access_token"

    # A GH_TOKEN alias that names nothing is the one manifest error that stayed
    # silent: the alias above only fires while iterating manifest entries, so a
    # GH_TOKEN_FROM that is not among them left GH_TOKEN unset with no warning,
    # and every `gh` call then failed as if unauthenticated — a long way from
    # the typo that caused it. Fail here, naming the mismatch; under
    # ENGINE_AGENT_AUTH_REQUIRED the entrypoint runs this at boot, so it
    # surfaces before a session ever starts.
    if [[ -n "${ENGINE_AGENT_AUTH_GH_TOKEN_FROM:-}" && "$GH_TOKEN_ALIASED" != "1" ]]; then
        die "ENGINE_AGENT_AUTH_GH_TOKEN_FROM names '${ENGINE_AGENT_AUTH_GH_TOKEN_FROM}', which is not a variable in ENGINE_AGENT_AUTH_SECRETS; no GH_TOKEN was aliased"
    fi

    # Cadastre is an explicit private-stack integration. Only fetch its
    # credential when that stack has opted in.
    if [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
        [[ -n "$CADASTRE_SECRET_NAME" ]] || die \
            "CADASTRE_MCP_ENABLED=1 but MYDEVENV2_CADASTRE_SECRET_NAME names no secret"
        [[ -n "$TOKEN_PROJECT_ID" ]] || die \
            "CADASTRE_MCP_ENABLED=1 but ENGINE_AGENT_AUTH_TOKEN_PROJECT_ID is not set"
        CADASTRE_HTTP_TOKEN="$(get_secret "$access_token" "$TOKEN_PROJECT_ID" "$CADASTRE_SECRET_NAME" || true)"
        [[ -n "$CADASTRE_HTTP_TOKEN" ]] || die \
            "Infisical secret $CADASTRE_SECRET_NAME is missing or empty"
        export CADASTRE_HTTP_TOKEN
        grant_var CADASTRE_HTTP_TOKEN
    else
        unset CADASTRE_HTTP_TOKEN CADASTRE_HTTP_TOKEN_FILE 2>/dev/null || true
    fi

    # Vogt is the estate's backlog/project tracker, reached the same way. An
    # absent token is not fatal, unlike Cadastre's: an instance may legitimately
    # not be deployed yet, and agent auth must keep working for git/gh
    # regardless.
    #
    # Inside a coding session, do not fetch: the session already holds a token
    # Vogt minted for its own actor (FR-S10), and this helper is what launches
    # the session's shell when auto-agent-auth is on. Fetching here would
    # replace that credential with the pod's before the agent ever ran, and
    # nothing would look wrong — the writes still land, the audit log just
    # attributes a session's work to the pod.
    if [[ -n "${VOGT_SESSION_ID:-}" && -n "${VOGT_HTTP_TOKEN:-}" ]]; then
        printf 'agent-auth: keeping the session token for %s\n' \
            "$VOGT_SESSION_ID" >&2
    elif [[ -n "$VOGT_SECRET_NAME" && -n "$TOKEN_PROJECT_ID" ]]; then
        # Deliberately unguarded on emptiness: absent is a supported state —
        # an instance may not be deployed yet — and `check` reports that as
        # `skip`, never as success and never as failure.
        VOGT_HTTP_TOKEN="$(get_secret "$access_token" "$TOKEN_PROJECT_ID" "$VOGT_SECRET_NAME" || true)"
        export VOGT_HTTP_TOKEN
    else
        unset VOGT_HTTP_TOKEN 2>/dev/null || true
    fi
    [[ -n "${VOGT_HTTP_TOKEN:-}" ]] && grant_var VOGT_HTTP_TOKEN

    umask 077
    AUTH_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/mydevenv2-agent-auth.XXXXXXXX")"
    if [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
        printf '%s' "$CADASTRE_HTTP_TOKEN" >"$AUTH_TMP_DIR/cadastre-http-token"
        export CADASTRE_HTTP_TOKEN_FILE="$AUTH_TMP_DIR/cadastre-http-token"
        grant_var CADASTRE_HTTP_TOKEN_FILE
    fi
    if [[ -n "${VOGT_HTTP_TOKEN:-}" ]]; then
        printf '%s' "$VOGT_HTTP_TOKEN" >"$AUTH_TMP_DIR/vogt-http-token"
        export VOGT_TOKEN_FILE="$AUTH_TMP_DIR/vogt-http-token"
        grant_var VOGT_TOKEN_FILE
    fi

    # Not gated on Cadastre. The bootstrap registers Vogt unconditionally and
    # Cadastre only when it is enabled; gating the call itself would take
    # Vogt's own registrations with it whenever Cadastre is off — which is
    # every generic deployment, and the opposite of making an optional
    # integration optional.
    if [[ "${MYDEVENV2_AUTO_CADASTRE_MCP:-1}" == "1" ]]; then
        /usr/local/bin/mydevenv2-mcp-bootstrap
    fi

    export GIT_ASKPASS=/usr/local/bin/mydevenv2-git-askpass
    export GIT_TERMINAL_PROMPT=0

    # Signpost the strip (#566). The identity and the manifest are about to be
    # dropped; without a breadcrumb the launched session sees only empty
    # INFISICAL_* vars, its `infisical login` fails, and the natural wrong
    # conclusion is "no such credential exists." These two names-only vars —
    # never a value, and deliberately not the API URL — let session tooling
    # detect the brokered model and either read a credential it was granted or
    # fail with an actionable message. See docs/ENGINE.md §9.
    # #637: identity-passthrough is an explicit per-deployment opt-in that keeps
    # the machine identity in the session (reverses the #511/#566 strip below).
    # Decided here, before the identity is unset, and reused for both.
    local identity_passthrough=0
    if [[ "${ENGINE_AGENT_AUTH_IDENTITY_PASSTHROUGH:-}" =~ ^(1|true|TRUE|yes|on)$ ]]; then
        identity_passthrough=1
    fi
    if [[ "$identity_passthrough" -eq 1 ]]; then
        # The session holds the full secrets-manager identity; session tooling
        # should use `infisical` directly, not the manifest broker.
        export AGENT_AUTH_MODE=identity
    else
        export AGENT_AUTH_MODE=brokered
    fi
    export AGENT_AUTH_GRANTED="$AGENT_AUTH_GRANTED_VARS"
    export AGENT_AUTH_ONDEMAND="$AGENT_AUTH_ONDEMAND_VARS"
    # Names a session may *store* through the broker (#598), beside the names it
    # may fetch. A session reads this instead of guessing and being refused.
    export AGENT_AUTH_WRITABLE="$AGENT_AUTH_WRITABLE_VARS"

    # Drop the brokering inputs before the shell/command this helper launches.
    # The manifest, the secret-name breadcrumbs and the passthrough switch itself
    # are engine config the launched shell never needs — dropped in either mode.
    # The brokered agent tokens exported above are kept — that is the whole point.
    unset ENGINE_AGENT_AUTH_SECRETS \
        ENGINE_AGENT_AUTH_GH_TOKEN_FROM ENGINE_AGENT_AUTH_TOKEN_PROJECT_ID \
        ENGINE_AGENT_AUTH_PROBES ENGINE_AGENT_AUTH_IDENTITY_PASSTHROUGH \
        MYDEVENV2_CADASTRE_SECRET_NAME MYDEVENV2_VOGT_SECRET_NAME \
        2>/dev/null || true
    # The machine identity is dropped by default (#511/#566): a normal session
    # must not keep the credential that could read the rest of the vault. In
    # identity-passthrough mode (#637) it is deliberately KEPT so the session can
    # use `infisical` directly against any project.
    if [[ "$identity_passthrough" -eq 0 ]]; then
        unset INFISICAL_CLIENT_ID INFISICAL_CLIENT_SECRET INFISICAL_API_URL \
            ENGINE_INFISICAL_ENV \
            2>/dev/null || true
    fi
}

# The service-probe list is configuration, not a baked-in estate service map.
# Each line is `NAME URL TOKEN_VAR`: a bearer-authenticated GET that must
# return 2xx. An empty/unset manifest runs no service probes — the MCP probes
# below still run.
run_configured_probes() {
    local line name url token_var token
    [[ -n "${ENGINE_AGENT_AUTH_PROBES:-}" ]] || return 0
    while IFS= read -r line; do
        line="${line%%#*}"
        read -r name url token_var <<<"$line"
        [[ -n "${name:-}" ]] || continue
        [[ -n "${url:-}" && -n "${token_var:-}" ]] || die \
            "malformed ENGINE_AGENT_AUTH_PROBES entry (want 'NAME URL TOKEN_VAR'): $line"
        token="${!token_var:-}"
        if curl -fsS -H "Authorization: Bearer $token" "$url" >/dev/null; then
            printf 'ok: %s (%s)\n' "$name" "$url"
        else
            die "$name probe failed at $url"
        fi
    done <<<"${ENGINE_AGENT_AUTH_PROBES}"
}

check_access() {
    local response_file error_file mcp_url vogt_url vogt_mcp_url
    local vogt_failure_hint
    local -a mcp_curl_args
    require_command curl
    load_agent_environment
    response_file="$(mktemp)"
    error_file="$(mktemp)"
    # Defaulted expansions, because this trap fires at *script* exit — by
    # which point `check_access` has returned and its locals are gone. Under
    # `set -u` the bare form aborts with "unbound variable" after every probe
    # has reported ok, so a fully green check would exit 1 and the temp files
    # would never be removed.
    trap 'rm -f "${response_file:-}" "${error_file:-}"' EXIT

    printf 'ok: secrets-manager universal auth\n'
    [[ -z "$AGENT_AUTH_ONDEMAND_VARS" ]] || \
        printf 'ondemand (fetched on request through the engine broker): %s\n' "$AGENT_AUTH_ONDEMAND_VARS"
    [[ -z "$AGENT_AUTH_WRITABLE_VARS" ]] || \
        printf 'writable (stored on request through the engine broker): %s\n' "$AGENT_AUTH_WRITABLE_VARS"
    run_configured_probes

    if [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
        mcp_url="${CADASTRE_MCP_URL:-}"
        [[ -n "$mcp_url" ]] || die \
            "CADASTRE_MCP_ENABLED=1 but CADASTRE_MCP_URL is not set"
        mcp_curl_args=()
        if [[ -n "${MYDEVENV2_CADASTRE_MCP_RESOLVE:-}" ]]; then
            mcp_curl_args+=(--resolve "${MYDEVENV2_CADASTRE_MCP_RESOLVE}")
        fi
        probe_mcp "Cadastre MCP" "$mcp_url" "$CADASTRE_HTTP_TOKEN" \
            "$CADASTRE_SECRET_NAME" "$response_file" "$error_file" "" \
            "${mcp_curl_args[@]}"
    else
        printf 'skip: Cadastre MCP (optional integration disabled)\n'
    fi

    # Vogt, probed the same way Cadastre is — because until this existed, the
    # check reported services green while Vogt was completely unusable from the
    # pod (#30). It is the one place that can catch that class of failure: it
    # runs in the pod holding the same credential a client will use, against
    # the endpoint that client will use.
    #
    # An absent token is not a failure: an instance may legitimately not be
    # deployed yet, and agent auth must keep working for git/gh regardless. So
    # this reports three distinct states and never conflates them — configured
    # and answering, not configured, or configured and refused.
    #
    # `/mcp`, not `/api/vogt/*`: the front door forwards a client's *core*
    # token there untouched and injects its own on the API prefix, so `/mcp` is
    # the one surface this credential is the right kind of token for.
    vogt_url="${VOGT_URL:-$DEFAULT_VOGT_URL}"
    vogt_mcp_url="${vogt_url%/}/mcp"
    if [[ -z "${VOGT_HTTP_TOKEN:-}" ]]; then
        printf 'skip: Vogt MCP (no %s secret; no instance configured)\n' \
            "${VOGT_SECRET_NAME:-Vogt token}"
    else
        # The token is what a client presents; the file is where a client
        # reads it from, and the rejection an agent eventually sees names that
        # file. Absent means every client here is broken, so it is a named
        # failure rather than the `skip` above, which means no instance.
        [[ -s "${VOGT_TOKEN_FILE:-}" ]] || die \
            "Vogt token loaded but VOGT_TOKEN_FILE (${VOGT_TOKEN_FILE:-<unset>}) is missing or empty; every registered client reads the credential from that file"
        # Named, because a Vogt token is minted by one instance and stored
        # hashed there. A token from another instance is refused however fresh
        # it is, and the message an agent sees points at the token file.
        vogt_failure_hint=" — if the token is current, check it was issued by *this* instance; tokens are not shared between Vogt instances"
        probe_mcp "Vogt MCP" "$vogt_mcp_url" "$VOGT_HTTP_TOKEN" \
            "${VOGT_SECRET_NAME:-Vogt token}" "$response_file" "$error_file" \
            "$vogt_failure_hint"
    fi
}

main() {
    case "${1:-}" in
        check)
            check_access
            ;;
        run)
            shift
            [[ "${1:-}" == "--" ]] && shift
            [[ $# -gt 0 ]] || die "run requires a command"
            load_agent_environment
            trap cleanup_auth_artifacts EXIT
            "$@"
            ;;
        shell)
            load_agent_environment
            trap cleanup_auth_artifacts EXIT
            "${SHELL:-/bin/bash}" -l
            ;;
        get)
            shift
            [[ $# -eq 1 && -n "${1:-}" ]] || die "get requires exactly one VAR"
            get_one "$1"
            ;;
        fetch)
            shift
            [[ $# -eq 1 && -n "${1:-}" ]] || die "fetch requires exactly one VAR"
            fetch_one "$1"
            ;;
        set)
            shift
            [[ $# -eq 1 && -n "${1:-}" ]] || die "set requires exactly one VAR"
            set_one "$1"
            ;;
        store)
            shift
            [[ $# -eq 1 && -n "${1:-}" ]] || die "store requires exactly one VAR"
            store_one "$1"
            ;;
        -h|--help|help)
            usage
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
