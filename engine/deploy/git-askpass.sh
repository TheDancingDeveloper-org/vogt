#!/bin/sh
set -eu
# Host is the exact hostname, not a substring.
# github.com.example and notgithub.com do not receive the GitHub token.
prompt=${1:-}
case "$prompt" in
  *://*) host=${prompt##*://} ;;
  *) host=$prompt ;;
esac
host=${host%%/*}
host=${host##*@}
host=${host%%:*}
host=${host%\'}
host=${host#\'}
# No token for the host in the environment: ask the engine's broker for one
# from an approved `git:<host>` grant (WI-1331), at prompt time, so a grant
# approved after the session started still reaches git. Only the password
# prompt asks — the username follows from the host — so a `once` grant is
# spent by one prompt, not two. The value goes straight to stdout.
brokered_password() {
  [ -n "${VOGT_ENGINE_BROKER_URL:-}" ] || exit 1
  vogt-agent-auth git-credential "$host" 2>/dev/null | sed -n 2p | grep . || exit 1
}
case "$prompt" in
  *[Uu]sername*)
    if [ "$host" = "github.com" ]; then
      printf '%s\n' x-access-token
    else
      printf '%s\n' git
    fi
    ;;
  *[Pp]assword*)
    if [ "$host" = "github.com" ]; then
      if [ -n "${ENGINE_WORKTREE_FETCH_TOKEN:-}" ]; then
        printf '%s\n' "$ENGINE_WORKTREE_FETCH_TOKEN"
      elif [ -n "${GH_TOKEN:-}" ]; then
        printf '%s\n' "$GH_TOKEN"
      else
        brokered_password
      fi
    else
      if [ -n "${GIT_AUTH_TOKEN:-}" ]; then
        printf '%s\n' "$GIT_AUTH_TOKEN"
      else
        brokered_password
      fi
    fi
    ;;
  *)
    exit 1
    ;;
esac
