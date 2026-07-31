#!/usr/bin/env bash
# Load service credentials on demand from Infisical for agent commands.
# The base container intentionally does not install or authenticate Codex/Claude.

set -euo pipefail

readonly DEFAULT_INFISICAL_API_URL="http://100.92.54.45:8400"
readonly CICD_PROJECT_ID="6d6caff5-7aaf-42f8-a135-2455d7629af8"
readonly INFRASTRUCTURE_PROJECT_ID="5b7e75de-e874-484d-9595-873acd6bfd07"
readonly APPS_PROJECT_ID="76b1ebe1-3656-4cef-952c-30d5d489c6e7"
readonly INFISICAL_ENV="prod"

usage() {
    cat <<'EOF'
Usage:
  mydevenv2-agent-auth check
  mydevenv2-agent-auth run -- <command> [args...]
  mydevenv2-agent-auth shell

Commands:
  check  Fetch credentials and validate Infisical, Forgejo, Woodpecker,
         GitHub, and Komodo access.
  run    Execute one command with service credentials on demand in memory.
  shell  Start a login shell with service credentials on demand in memory.
EOF
}

die() {
    printf 'mydevenv2-agent-auth: %s\n' "$*" >&2
    exit 1
}

warn() {
    printf 'mydevenv2-agent-auth: warning: %s\n' "$*" >&2
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
    local access_token github_destination_token

    require_identity
    require_command infisical
    access_token="$(mint_access_token)" || die "Infisical universal-auth login failed"

    export GIT_AUTH_TOKEN="$(get_secret "$access_token" "$CICD_PROJECT_ID" GIT_AUTH_TOKEN)"
    export FORGEJO_TOKEN="$(get_secret "$access_token" "$CICD_PROJECT_ID" FORGEJO_TOKEN)"
    export WOODPECKER_TOKEN="$(get_secret "$access_token" "$INFRASTRUCTURE_PROJECT_ID" WOODPECKER_TOKEN)"
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
    export GH_TOKEN="$github_destination_token"
    export HOMELAB_KOMODO_API_KEY="$(get_secret "$access_token" "$APPS_PROJECT_ID" HOMELAB_KOMODO_API_KEY)"
    export HOMELAB_KOMODO_API_SECRET="$(get_secret "$access_token" "$APPS_PROJECT_ID" HOMELAB_KOMODO_API_SECRET)"

    export INFISICAL_API_URL="${INFISICAL_API_URL:-$DEFAULT_INFISICAL_API_URL}"
    export GIT_ASKPASS=/usr/local/bin/mydevenv2-git-askpass
    export GIT_TERMINAL_PROMPT=0
}

check_access() {
    local response_file gh_login
    require_command curl
    require_command git
    require_command gh
    load_agent_environment
    response_file="$(mktemp)"
    trap "rm -f '$response_file'" EXIT

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
    printf 'ok: GitHub destination (TheDancingDeveloper-org admin)\n'
    # The source-org credential is a local `gh auth login` session, which only
    # exists on the primary dev machine — MyDevEnv2 pods have no gh hosts.yml.
    # Advisory, not fatal: destination access is what agents actually need, and
    # a hard failure here would leave `check` red for every containerised agent.
    if [[ "$(env -u GH_TOKEN -u GITHUB_PAT gh api user/memberships/orgs/AusAgentSmith-org --jq '.state + ":" + .role' 2>/dev/null)" == "active:admin" ]]; then
        printf 'ok: GitHub source (AusAgentSmith-org admin)\n'
    else
        warn "no local AusAgentSmith GitHub CLI session; source-org operations are unavailable here. Run 'gh auth login' if you need them."
    fi
    curl -fsS http://100.92.54.45:3011/read \
        -H "X-Api-Key: $HOMELAB_KOMODO_API_KEY" \
        -H "X-Api-Secret: $HOMELAB_KOMODO_API_SECRET" \
        -H 'Content-Type: application/json' \
        --data '{"type":"ListServers","params":{}}' >"$response_file"
    printf 'ok: Komodo API\n'
}

case "${1:-}" in
    check)
        check_access
        ;;
    run)
        shift
        [[ "${1:-}" == "--" ]] && shift
        [[ $# -gt 0 ]] || die "run requires a command"
        load_agent_environment
        exec "$@"
        ;;
    shell)
        load_agent_environment
        exec "${SHELL:-/bin/bash}" -l
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
