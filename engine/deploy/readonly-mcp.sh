#!/usr/bin/env bash
# Start one of the image's third-party MCP servers in read-only mode.
#
#   vogt-readonly-mcp github|grafana|gitea|k8s
#
# This is the command `vogt-mcp-bootstrap` registers with the agent CLIs, so
# the stored registration holds no token and no flag a session could edit:
# the token is read from this process's environment at spawn, renamed to the
# variable the upstream server expects, and the read-only switches are set
# here on every start. Each server is enabled only by its own variable:
#
#   github   GITHUB_MCP_TOKEN                       (fine-grained, read-only)
#            VOGT_GITHUB_MCP_TOOLSETS               optional, default below
#   grafana  GRAFANA_URL + GRAFANA_SERVICE_ACCOUNT_TOKEN  (Viewer role)
#   gitea    GITEA_HOST + GITEA_MCP_TOKEN            (read-scoped token;
#            Gitea or Forgejo)
#   k8s      K8S_AGENT_READONLY_KUBECONFIG          (the kubeconfig itself,
#            raw or base64, for a read-only service account that cannot
#            read Secrets; written to a 0600 file for the run, never argv)
#
# Read-only is enforced twice: by the server's own mode below and by the
# token's scope, which is the one that holds if a server's mode has a gap.
#
# stdout is the MCP transport. Nothing but the server may write to it; every
# message here goes to stderr.
set -euo pipefail

die() {
    printf 'vogt-readonly-mcp: %s\n' "$*" >&2
    exit 64
}

need() {
    local var
    for var in "$@"; do
        [[ -n "${!var:-}" ]] || die "$var is not set in this session; the $server MCP server is not available"
    done
}

server="${1:-}"
case "$server" in
    github)
        need GITHUB_MCP_TOKEN
        export GITHUB_PERSONAL_ACCESS_TOKEN="$GITHUB_MCP_TOKEN"
        # GITHUB_TOOLSETS beats --toolsets upstream, so it is set rather
        # than trusting the flag; GITHUB_READ_ONLY likewise.
        export GITHUB_TOOLSETS="${VOGT_GITHUB_MCP_TOOLSETS:-actions,pull_requests,repos,code_security,dependabot}"
        export GITHUB_READ_ONLY=1
        unset GITHUB_TOOLS GITHUB_DYNAMIC_TOOLSETS
        exec github-mcp-server stdio --read-only
        ;;
    grafana)
        need GRAFANA_URL GRAFANA_SERVICE_ACCOUNT_TOKEN
        # The service-account token is the only credential: drop the
        # alternatives the server would otherwise prefer or combine.
        unset GRAFANA_API_KEY GRAFANA_USERNAME GRAFANA_PASSWORD GRAFANA_EXTRA_HEADERS
        exec mcp-grafana -t stdio --disable-write --usage-stats=disabled
        ;;
    gitea)
        need GITEA_HOST GITEA_MCP_TOKEN
        export GITEA_ACCESS_TOKEN="$GITEA_MCP_TOKEN"
        export GITEA_READONLY=true
        unset GITEA_ACCESS_TOKEN_FILE GITEA_MCP_OAUTH
        exec gitea-mcp -t stdio -r
        ;;
    k8s)
        need K8S_AGENT_READONLY_KUBECONFIG
        # The kubeconfig is a credential: it stays out of argv and the repo.
        # The server reads a file, so write one (0600, in a 0700 directory)
        # and remove it when the server exits. The server runs as a child,
        # not by exec, so the cleanup trap survives; stdin and stdout are
        # inherited, so the MCP transport is untouched.
        umask 077
        kdir="$(mktemp -d "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/vogt-k8s-mcp.XXXXXX")"
        trap 'rm -rf "$kdir"' EXIT
        trap 'exit 143' TERM
        trap 'exit 130' INT
        kubeconfig="$kdir/kubeconfig"
        if [[ "$K8S_AGENT_READONLY_KUBECONFIG" == *clusters:* ]]; then
            printf '%s\n' "$K8S_AGENT_READONLY_KUBECONFIG" >"$kubeconfig"
        else
            printf '%s' "$K8S_AGENT_READONLY_KUBECONFIG" | base64 -d >"$kubeconfig" 2>/dev/null \
                || die "K8S_AGENT_READONLY_KUBECONFIG is neither a kubeconfig nor base64 of one"
        fi
        # Only this file may configure the client: drop the alternatives.
        unset K8S_AGENT_READONLY_KUBECONFIG KUBECONFIG KUBERNETES_MASTER
        kubernetes-mcp-server --read-only --kubeconfig "$kubeconfig"
        ;;
    *)
        die "usage: vogt-readonly-mcp github|grafana|gitea|k8s"
        ;;
esac
