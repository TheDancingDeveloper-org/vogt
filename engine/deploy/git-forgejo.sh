#!/usr/bin/env bash
# git, authenticated to a Gitea/Forgejo host with the session's token.
#
#   git-forgejo clone https://forge.example/org/repo.git
#   git forgejo fetch origin          # installed as git-forgejo, so git finds it
#
# The obvious one-liner,
#
#   git -c http.extraheader="Authorization: token $FORGEJO_TOKEN" ...
#
# breaks as soon as it passes through one more layer of shell without its
# quotes: the header is split at its spaces, git receives `Authorization:` as
# the whole header value and a stray word as a command, and the push fails
# with an auth error that names neither. This wrapper builds the header once,
# as one value, and hands it to git through GIT_CONFIG_COUNT/KEY/VALUE — so
# no shell ever re-splits it, and the token is not on git's command line,
# where any process that can read /proc/<pid>/cmdline would see it.
#
# The header is scoped to the forge's URL (`http.<url>.extraheader`), so a
# submodule or redirect to another host is not handed the token.
#
#   FORGEJO_TOKEN   the token (GITEA_MCP_TOKEN is not used: that one is
#                   read-only, and this wrapper is for pushes too)
#   FORGEJO_URL     the forge's base URL, e.g. https://forge.example;
#                   falls back to GITEA_HOST
#
# Nothing here prints the token. Every other argument goes to git unchanged.
set -euo pipefail

die() {
    printf 'git-forgejo: %s\n' "$*" >&2
    exit 64
}

[[ -n "${FORGEJO_TOKEN:-}" ]] || die "FORGEJO_TOKEN is not set in this session"
base="${FORGEJO_URL:-${GITEA_HOST:-}}"
[[ -n "$base" ]] || die "FORGEJO_URL (or GITEA_HOST) is not set; it names the host the token is for"
case "$base" in
    https://*|http://*) ;;
    *) die "FORGEJO_URL must be an http(s) URL, got '$base'" ;;
esac
base="${base%/}/"

# Append to any GIT_CONFIG_COUNT entries the caller already set, rather than
# replacing them.
count="${GIT_CONFIG_COUNT:-0}"
[[ "$count" =~ ^[0-9]+$ ]] || die "GIT_CONFIG_COUNT is not a number"
export "GIT_CONFIG_KEY_${count}=http.${base}.extraheader"
export "GIT_CONFIG_VALUE_${count}=Authorization: token ${FORGEJO_TOKEN}"
export GIT_CONFIG_COUNT=$((count + 1))
# Never fall back to a credential prompt: a wrong token should fail, not hang.
export GIT_TERMINAL_PROMPT=0

exec git "$@"
