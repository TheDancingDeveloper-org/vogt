#!/usr/bin/env bash
# Load service credentials on demand from Infisical for agent commands.
# The base container intentionally does not install or authenticate Codex/Claude.

set -euo pipefail

readonly DEFAULT_INFISICAL_API_URL="http://100.92.54.45:8400"
readonly CICD_PROJECT_ID="6d6caff5-7aaf-42f8-a135-2455d7629af8"
readonly INFRASTRUCTURE_PROJECT_ID="5b7e75de-e874-484d-9595-873acd6bfd07"
readonly APPS_PROJECT_ID="76b1ebe1-3656-4cef-952c-30d5d489c6e7"
readonly INFISICAL_ENV="prod"
# Cadastre-scoped like the other Cadastre values in this file
# (CADASTRE_MCP_URL, CADASTRE_HTTP_TOKEN), not prefixed with a product name.
readonly CADASTRE_MCP_ENABLED="${CADASTRE_MCP_ENABLED:-0}"
readonly CADASTRE_SECRET_NAME="${MYDEVENV2_CADASTRE_SECRET_NAME:-HOMELAB_CADASTRE_HTTP_TOKEN}"
readonly VOGT_SECRET_NAME="${MYDEVENV2_VOGT_SECRET_NAME:-HOMELAB_VOGT_AGENT_TOKEN}"
readonly DEFAULT_CADASTRE_MCP_URL="https://winrarhost.tailc7d3c.ts.net:18092/mcp"
readonly DEFAULT_CADASTRE_MCP_RESOLVE="${MYDEVENV2_CADASTRE_MCP_RESOLVE:-}"
# The same default `vogt-mcp-auth.sh` uses, and for the reason it gives: in
# the merged stack the engine is the only published port (NFR-D11) and this
# runs inside that container, so loopback needs no DNS and no certificate. A
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
  check  Fetch credentials and validate Infisical, Forgejo, Woodpecker,
         GitHub, Komodo, Cadastre MCP and Vogt MCP access.
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
    [[ -n "${INFISICAL_CLIENT_ID:-}" ]] || die \
        "INFISICAL_CLIENT_ID is not configured; add the MyDevEnv2 machine identity to the Komodo stack"
    [[ -n "${INFISICAL_CLIENT_SECRET:-}" ]] || die \
        "INFISICAL_CLIENT_SECRET is not configured; add the MyDevEnv2 machine identity to the Komodo stack"
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
        --domain "${INFISICAL_API_URL:-$DEFAULT_INFISICAL_API_URL}" \
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
        --domain "${INFISICAL_API_URL:-$DEFAULT_INFISICAL_API_URL}" \
        --projectId "$project_id" \
        --env "$INFISICAL_ENV" \
        --token "$access_token" \
        --plain --silent
}

load_agent_environment() {
    local access_token github_destination_token github_source_token

    require_identity
    require_command infisical
    access_token="$(mint_access_token)" || die "Infisical universal-auth login failed"

    # Assigned, checked, then exported — never `export X="$(...)"`.
    #
    # `export` is a command with its own exit status, and it succeeds. Under
    # `set -e` that status is the one tested, so a `get_secret` that fails
    # inside the substitution does not stop the script: the variable is left
    # empty and everything downstream runs with a credential that is the empty
    # string. The failure then surfaces as a 401 from whichever service is
    # asked first — a long way from the Infisical call that actually failed,
    # and looking like a revoked token rather than an unavailable secret store.
    #
    # The two GitHub secrets below already did it this way. These five did
    # not, and had no emptiness guard either.
    GIT_AUTH_TOKEN="$(get_secret "$access_token" "$CICD_PROJECT_ID" GIT_AUTH_TOKEN || true)"
    [[ -n "${GIT_AUTH_TOKEN}" ]] || die "Infisical secret GIT_AUTH_TOKEN is missing or empty"
    export GIT_AUTH_TOKEN
    FORGEJO_TOKEN="$(get_secret "$access_token" "$CICD_PROJECT_ID" FORGEJO_TOKEN || true)"
    [[ -n "${FORGEJO_TOKEN}" ]] || die "Infisical secret FORGEJO_TOKEN is missing or empty"
    export FORGEJO_TOKEN
    WOODPECKER_TOKEN="$(get_secret "$access_token" "$INFRASTRUCTURE_PROJECT_ID" WOODPECKER_TOKEN || true)"
    [[ -n "${WOODPECKER_TOKEN}" ]] || die "Infisical secret WOODPECKER_TOKEN is missing or empty"
    export WOODPECKER_TOKEN
    # GITHUB_PAT / GH_RELEASE_TOKEN are retired release-automation names whose
    # values are revoked. They used to be exported here as GH_TOKEN, so every
    # `gh` call in an auto-agent-auth shell failed with "Bad credentials" and
    # agents concluded GitHub auth was unavailable. Clear anything inherited
    # so a stale value cannot outlive this fix.
    unset GITHUB_PAT GH_RELEASE_TOKEN
    github_destination_token="$(get_secret "$access_token" "$CICD_PROJECT_ID" GITHUB_DANCINGDEVELOPER_PAT 2>/dev/null || true)"
    [[ -n "$github_destination_token" ]] || die \
        "Infisical secret GITHUB_DANCINGDEVELOPER_PAT is missing or empty; refusing ambiguous GitHub credential fallback"
    export GITHUB_DANCINGDEVELOPER_PAT="$github_destination_token"
    # TheDancingDeveloper-org is the main org, so it owns the default GH_TOKEN.
    export GH_TOKEN="$github_destination_token"

    # AusAgentSmith-org is still live and holds its own distinct repo set
    # (AiFw, lindirstat-rs, email-rs, fluent-gpui, ...), so the pod needs both
    # identities. Source-org work runs as:
    #   GH_TOKEN="$GITHUB_AUSAGENTSMITH_PAT" gh ...
    github_source_token="$(get_secret "$access_token" "$CICD_PROJECT_ID" GITHUB_AUSAGENTSMITH_PAT 2>/dev/null || true)"
    [[ -n "$github_source_token" ]] || die \
        "Infisical secret GITHUB_AUSAGENTSMITH_PAT is missing or empty"
    export GITHUB_AUSAGENTSMITH_PAT="$github_source_token"
    HOMELAB_KOMODO_API_KEY="$(get_secret "$access_token" "$APPS_PROJECT_ID" HOMELAB_KOMODO_API_KEY || true)"
    [[ -n "${HOMELAB_KOMODO_API_KEY}" ]] || die "Infisical secret HOMELAB_KOMODO_API_KEY is missing or empty"
    export HOMELAB_KOMODO_API_KEY
    HOMELAB_KOMODO_API_SECRET="$(get_secret "$access_token" "$APPS_PROJECT_ID" HOMELAB_KOMODO_API_SECRET || true)"
    [[ -n "${HOMELAB_KOMODO_API_SECRET}" ]] || die "Infisical secret HOMELAB_KOMODO_API_SECRET is missing or empty"
    export HOMELAB_KOMODO_API_SECRET
    # Cadastre is an explicit private-stack integration, not a prerequisite
    # for an authenticated shell or for Vogt. Only fetch its credential when
    # that stack has opted in.
    if [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
        CADASTRE_HTTP_TOKEN="$(get_secret "$access_token" "$APPS_PROJECT_ID" "$CADASTRE_SECRET_NAME" || true)"
        [[ -n "$CADASTRE_HTTP_TOKEN" ]] || die \
            "Infisical secret $CADASTRE_SECRET_NAME is missing or empty"
        export CADASTRE_HTTP_TOKEN
    else
        unset CADASTRE_HTTP_TOKEN CADASTRE_HTTP_TOKEN_FILE
    fi
    # Vogt is the estate's backlog/project tracker, reached the same way for
    # the same reasons. Absent secret is not fatal, unlike cadastre's: an
    # instance may legitimately not be deployed yet, and agent auth must keep
    # working for git/gh regardless.
    #
    # Inside a coding session, do not: the session already holds a token Vogt
    # minted for its own actor (FR-S10), and this helper is what launches the
    # session's shell when MYDEVENV2_AUTO_AGENT_AUTH is on — which is the
    # deployed configuration. Fetching here would replace that credential
    # with the pod's before the agent ever ran, and nothing would look
    # wrong: the writes still land, the audit log just says `agent:mydevenv2`
    # for work that belongs to a session.
    if [[ -n "${VOGT_SESSION_ID:-}" && -n "${VOGT_HTTP_TOKEN:-}" ]]; then
        printf 'agent-auth: keeping the session token for %s\n' \
            "$VOGT_SESSION_ID" >&2
    else
        # Deliberately unguarded: absent is a supported state here — an
        # instance may not be deployed yet — and `check` reports that as
        # `skip`, never as success and never as failure.
        VOGT_HTTP_TOKEN="$(get_secret "$access_token" "$APPS_PROJECT_ID" "$VOGT_SECRET_NAME" || true)"
        export VOGT_HTTP_TOKEN
    fi

    umask 077
    AUTH_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/mydevenv2-agent-auth.XXXXXXXX")"
    if [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
        printf '%s' "$CADASTRE_HTTP_TOKEN" >"$AUTH_TMP_DIR/cadastre-http-token"
        export CADASTRE_HTTP_TOKEN_FILE="$AUTH_TMP_DIR/cadastre-http-token"
    fi
    if [[ -n "$VOGT_HTTP_TOKEN" ]]; then
        printf '%s' "$VOGT_HTTP_TOKEN" >"$AUTH_TMP_DIR/vogt-http-token"
        export VOGT_TOKEN_FILE="$AUTH_TMP_DIR/vogt-http-token"
    fi

    # Not gated on Cadastre. The bootstrap registers Vogt unconditionally and
    # Cadastre only when it is enabled; gating the call itself would take
    # Vogt's own registrations with it whenever Cadastre is off — which is
    # every generic deployment, and the opposite of making an optional
    # integration optional. (The variable's name is broader than Cadastre
    # now; renaming it is tracked with the rest of the MYDEVENV2_* window.)
    if [[ "${MYDEVENV2_AUTO_CADASTRE_MCP:-1}" == "1" ]]; then
        /usr/local/bin/mydevenv2-mcp-bootstrap
    fi

    export INFISICAL_API_URL="${INFISICAL_API_URL:-$DEFAULT_INFISICAL_API_URL}"
    export GIT_ASKPASS=/usr/local/bin/mydevenv2-git-askpass
    export GIT_TERMINAL_PROMPT=0
}

check_access() {
    local response_file error_file gh_login mcp_url vogt_url vogt_mcp_url
    local vogt_failure_hint
    local -a mcp_curl_args
    require_command curl
    require_command git
    require_command gh
    load_agent_environment
    response_file="$(mktemp)"
    error_file="$(mktemp)"
    # Defaulted expansions, because this trap fires at *script* exit — by
    # which point `check_access` has returned and its locals are gone. Under
    # `set -u` the bare form aborted with "response_file: unbound variable"
    # after every probe had reported ok, so a fully green check exited 1 and
    # the temp files were never removed.
    trap 'rm -f "${response_file:-}" "${error_file:-}"' EXIT

    printf 'ok: Infisical universal auth\n'
    curl -fsS -H "Authorization: token $FORGEJO_TOKEN" \
        https://repo.indexarr.net/api/v1/user >"$response_file"
    printf 'ok: Forgejo API\n'
    git ls-remote https://repo.indexarr.net/indexarr/ops.git HEAD >/dev/null
    printf 'ok: Forgejo git\n'
    curl -fsS -H "Authorization: Bearer $WOODPECKER_TOKEN" \
        https://ci.indexarr.net/api/user >"$response_file"
    printf 'ok: Woodpecker API\n'
    # GitHub logins are case-insensitive; the API returns canonical casing.
    gh_login="$(gh api user --jq .login 2>/dev/null || true)"
    [[ "${gh_login,,}" == "thedancingdeveloper" ]] || die \
        "GitHub destination token is not authenticated as TheDancingDeveloper (got: ${gh_login:-<none>})"
    [[ "$(gh api user/memberships/orgs/TheDancingDeveloper-org --jq '.state + ":" + .role')" == "active:admin" ]] || die \
        "GitHub destination token is not an active TheDancingDeveloper-org admin"
    printf 'ok: GitHub main org (TheDancingDeveloper-org admin)\n'
    # Source org is validated with its own PAT from Infisical, not a local
    # `gh auth login` session — pods have no gh hosts.yml, so a session-based
    # check could never pass there.
    gh_login="$(GH_TOKEN="$GITHUB_AUSAGENTSMITH_PAT" gh api user --jq .login 2>/dev/null || true)"
    [[ "${gh_login,,}" == "ausagentsmith" ]] || die \
        "GITHUB_AUSAGENTSMITH_PAT is not authenticated as AusAgentSmith (got: ${gh_login:-<none>})"
    [[ "$(GH_TOKEN="$GITHUB_AUSAGENTSMITH_PAT" gh api user/memberships/orgs/AusAgentSmith-org --jq '.state + ":" + .role' 2>/dev/null)" == "active:admin" ]] || die \
        "GITHUB_AUSAGENTSMITH_PAT is not an active AusAgentSmith-org admin"
    printf 'ok: GitHub source org (AusAgentSmith-org admin)\n'
    curl -fsS http://100.92.54.45:3011/read \
        -H "X-Api-Key: $HOMELAB_KOMODO_API_KEY" \
        -H "X-Api-Secret: $HOMELAB_KOMODO_API_SECRET" \
        -H 'Content-Type: application/json' \
        --data '{"type":"ListServers","params":{}}' >"$response_file"
    printf 'ok: Komodo API\n'
    if [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
        mcp_url="${CADASTRE_MCP_URL:-$DEFAULT_CADASTRE_MCP_URL}"
        mcp_curl_args=()
        if [[ -n "${MYDEVENV2_CADASTRE_MCP_RESOLVE:-$DEFAULT_CADASTRE_MCP_RESOLVE}" ]]; then
            mcp_curl_args+=(--resolve "${MYDEVENV2_CADASTRE_MCP_RESOLVE}")
        fi
        probe_mcp "Cadastre MCP" "$mcp_url" "$CADASTRE_HTTP_TOKEN" \
            "$CADASTRE_SECRET_NAME" "$response_file" "$error_file" "" \
            "${mcp_curl_args[@]}"
    else
        printf 'skip: Cadastre MCP (optional integration disabled)\n'
    fi

    # Vogt, probed the same way Cadastre is — because until this existed, the
    # check reported seven services green while Vogt was completely unusable
    # from the pod (#30). Vogt appeared in the bootstrap's banner, which says
    # registrations were *written*, and nowhere in the probes. The first
    # evidence of the outage arrived later, from a client, as a rejected token
    # naming VOGT_TOKEN_FILE — a file that was correct throughout (#29).
    #
    # This is the one place that can catch that class of failure, because it
    # is the only thing that runs in the pod holding the same credential a
    # client will use, against the endpoint that client will use.
    #
    # An absent token is not a failure, for the reason `load_agent_environment`
    # gives: an instance may legitimately not be deployed yet, and agent auth
    # must keep working for git/gh regardless. So this reports three distinct
    # states and never conflates them — configured and answering, not
    # configured, or configured and refused.
    #
    # `/mcp`, not `/api/vogt/*`: the front door forwards a client's *core*
    # token there untouched and injects its own on the API prefix, so `/mcp`
    # is the one surface this credential is the right kind of token for. It is
    # also the surface every registered client uses.
    vogt_url="${VOGT_URL:-$DEFAULT_VOGT_URL}"
    vogt_mcp_url="${vogt_url%/}/mcp"
    if [[ -z "${VOGT_HTTP_TOKEN:-}" ]]; then
        printf 'skip: Vogt MCP (no %s secret; no instance configured)\n' \
            "$VOGT_SECRET_NAME"
    else
        # The token is what a client presents; the file is where a client
        # reads it from, and the rejection an agent eventually sees names
        # that file. Absent means every client here is broken, so it is a
        # named failure rather than the `skip` above, which means no instance.
        [[ -s "${VOGT_TOKEN_FILE:-}" ]] || die \
            "Vogt token loaded but VOGT_TOKEN_FILE (${VOGT_TOKEN_FILE:-<unset>}) is missing or empty; every registered client reads the credential from that file"
        # Named, because a Vogt token is minted by one instance and stored
        # hashed there. A token from another instance is refused however
        # fresh it is, and the message an agent sees points at the token file.
        vogt_failure_hint=" — if the token is current, check it was issued by *this* instance; tokens are not shared between Vogt instances"
        probe_mcp "Vogt MCP" "$vogt_mcp_url" "$VOGT_HTTP_TOKEN" \
            "$VOGT_SECRET_NAME" "$response_file" "$error_file" \
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
