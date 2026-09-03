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
#                                    VAR PROJECT_ID SECRET_NAME [optional]
#                                  each fetched and exported as VAR; a missing
#                                  required secret fails, naming the secret.
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

Commands:
  check  Fetch credentials and validate secrets-manager access, any configured
         service probes, and the Cadastre and Vogt MCP endpoints.
  run    Execute one command with service credentials on demand in memory.
  shell  Start a login shell with service credentials on demand in memory.
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
            "malformed ENGINE_AGENT_AUTH_SECRETS entry (want 'VAR PROJECT_ID SECRET_NAME [optional]'): $line"
        value="$(get_secret "$access_token" "$project" "$name" || true)"
        if [[ -z "$value" && "${flag:-required}" != "optional" ]]; then
            die "Infisical secret $name is missing or empty"
        fi
        printf -v "$var" '%s' "$value"
        export "${var?}"
        if [[ "$var" == "${ENGINE_AGENT_AUTH_GH_TOKEN_FROM:-}" ]]; then
            export GH_TOKEN="$value"
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

    umask 077
    AUTH_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/mydevenv2-agent-auth.XXXXXXXX")"
    if [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
        printf '%s' "$CADASTRE_HTTP_TOKEN" >"$AUTH_TMP_DIR/cadastre-http-token"
        export CADASTRE_HTTP_TOKEN_FILE="$AUTH_TMP_DIR/cadastre-http-token"
    fi
    if [[ -n "${VOGT_HTTP_TOKEN:-}" ]]; then
        printf '%s' "$VOGT_HTTP_TOKEN" >"$AUTH_TMP_DIR/vogt-http-token"
        export VOGT_TOKEN_FILE="$AUTH_TMP_DIR/vogt-http-token"
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

    # Drop the brokering identity before the shell/command this helper launches.
    # The engine re-grants exactly these to the helper (they are otherwise
    # stripped from every child, #511); every Infisical secret is now fetched,
    # so the launched shell must not keep the machine identity that could read
    # the rest of the vault, nor the manifest that names what to read. The
    # brokered agent tokens exported above are kept — that is the whole point.
    unset INFISICAL_CLIENT_ID INFISICAL_CLIENT_SECRET INFISICAL_API_URL \
        ENGINE_INFISICAL_ENV ENGINE_AGENT_AUTH_SECRETS \
        ENGINE_AGENT_AUTH_GH_TOKEN_FROM ENGINE_AGENT_AUTH_TOKEN_PROJECT_ID \
        ENGINE_AGENT_AUTH_PROBES \
        MYDEVENV2_CADASTRE_SECRET_NAME MYDEVENV2_VOGT_SECRET_NAME \
        2>/dev/null || true
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
