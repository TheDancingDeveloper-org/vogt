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
# No token in the environment (WI-1331): ask the broker for an approved
# `git:<host>` grant, only at a Password prompt so a `once` grant is spent once
# per push. Prints the token line (the helper's second line); silent exit 1 on
# any refusal. The value is never logged and never on argv.
broker_password() {
  [ -n "${VOGT_ENGINE_BROKER_URL:-}" ] || exit 1
  cred=$(vogt-agent-auth git-credential "$1" 2>/dev/null) || exit 1
  token=$(printf '%s\n' "$cred" | sed -n '2p')
  [ -n "$token" ] || exit 1
  printf '%s\n' "$token"
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
        broker_password "$host"
      fi
    else
      if [ -n "${GIT_AUTH_TOKEN:-}" ]; then
        printf '%s\n' "$GIT_AUTH_TOKEN"
      else
        broker_password "$host"
      fi
    fi
    ;;
  *)
    exit 1
    ;;
esac
