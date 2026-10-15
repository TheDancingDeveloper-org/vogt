#!/usr/bin/env bash
set -euo pipefail

prompt="${1:-}"
case "$prompt" in
    *Username*github.com*)
        printf '%s\n' 'x-access-token'
        ;;
    *Username*)
        printf '%s\n' 'git'
        ;;
    *Password*github.com*)
        # The engine's worktree fetch sets ENGINE_WORKTREE_FETCH_TOKEN for
        # this one command and clears GH_TOKEN, so the two never compete.
        # Sessions still answer from GH_TOKEN.
        if [[ -n "${ENGINE_WORKTREE_FETCH_TOKEN:-}" ]]; then
            printf '%s\n' "$ENGINE_WORKTREE_FETCH_TOKEN"
        elif [[ -n "${GH_TOKEN:-}" ]]; then
            printf '%s\n' "$GH_TOKEN"
        else
            exit 1
        fi
        ;;
    *Password*)
        [[ -n "${GIT_AUTH_TOKEN:-}" ]] || exit 1
        printf '%s\n' "$GIT_AUTH_TOKEN"
        ;;
    *)
        exit 1
        ;;
esac
