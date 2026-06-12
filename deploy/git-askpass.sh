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
        [[ -n "${GH_TOKEN:-}" ]] || exit 1
        printf '%s\n' "$GH_TOKEN"
        ;;
    *Password*)
        [[ -n "${GIT_AUTH_TOKEN:-}" ]] || exit 1
        printf '%s\n' "$GIT_AUTH_TOKEN"
        ;;
    *)
        exit 1
        ;;
esac
