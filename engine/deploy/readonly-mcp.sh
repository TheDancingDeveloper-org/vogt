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
#            read Secrets; when the manifest declares it `ondemand` instead
#            of exporting it, the value is fetched from the engine's secret
#            broker at spawn. Written to a 0600 file for the run, never argv.)
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

# A pod whose tailscaled runs in userspace mode (vogt-dev, WI-4307) has no
# TUN device: a direct dial to a tailnet address is refused, and only the
# sidecar's loopback SOCKS5 reaches it (gh#1442). When the server's URL is a
# tailnet address (100.64.0.0/10 or *.ts.net), route this one process through
# that proxy: VOGT_TAILNET_SOCKS5 (host:port) if set, else the sidecar's
# default 127.0.0.1:1055 when its LocalAPI socket is present. Nothing else in
# the session is proxied.
tailnet_proxy() {
    local url="$1" host socks
    host="${url#*://}"
    host="${host%%/*}"
    host="${host##*@}"
    host="${host%:*}"
    if [[ ! "$host" =~ ^100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]+\.[0-9]+$ \
        && "$host" != *.ts.net ]]; then
        return 0
    fi
    socks="${VOGT_TAILNET_SOCKS5:-}"
    if [[ -z "$socks" && -S "${TS_SOCKET:-/run/tailscale/tailscaled.sock}" ]]; then
        socks="127.0.0.1:1055"
    fi
    [[ -n "$socks" ]] || return 0
    export HTTP_PROXY="socks5://$socks" HTTPS_PROXY="socks5://$socks"
    unset http_proxy https_proxy ALL_PROXY all_proxy
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
        tailnet_proxy "$GRAFANA_URL"
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
        value="${K8S_AGENT_READONLY_KUBECONFIG:-}"
        if [[ -z "$value" ]]; then
            value="$("${VOGT_AGENT_AUTH:-/usr/local/bin/vogt-agent-auth}" fetch K8S_AGENT_READONLY_KUBECONFIG)" \
                || die "K8S_AGENT_READONLY_KUBECONFIG is not set and could not be fetched from the secret broker"
        fi
        [[ -n "$value" ]] || die "K8S_AGENT_READONLY_KUBECONFIG is empty"
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
        if [[ "$value" == *clusters:* ]]; then
            printf '%s\n' "$value" >"$kubeconfig"
        else
            printf '%s' "$value" | base64 -d >"$kubeconfig" 2>/dev/null \
                || die "K8S_AGENT_READONLY_KUBECONFIG is neither a kubeconfig nor base64 of one"
        fi
        # Only this file may configure the client: drop the alternatives.
        unset K8S_AGENT_READONLY_KUBECONFIG KUBECONFIG KUBERNETES_MASTER
        # --toolsets core: the default (core,config) adds configuration_view,
        # which prints the service-account token unredacted.
        kubernetes-mcp-server --read-only --toolsets core --kubeconfig "$kubeconfig"
        ;;
    *)
        die "usage: vogt-readonly-mcp github|grafana|gitea|k8s"
        ;;
esac
