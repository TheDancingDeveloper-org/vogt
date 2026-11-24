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
        exit 1
      fi
    else
      if [ -z "${GIT_AUTH_TOKEN:-}" ]; then
        exit 1
      fi
      printf '%s\n' "$GIT_AUTH_TOKEN"
    fi
    ;;
  *)
    exit 1
    ;;
esac
