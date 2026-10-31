#!/usr/bin/env bash
# `g`: the graft client with this session's endpoint and token (WI-2755).
#
# Workers land through graft with a per-session token that oversight mints at
# ~/.config/graft/sessions/<session id>/<name>.token. The worker setup used to
# define `g` as a shell function, which survives only in the shell that ran it:
# a Codex session runs every command in a fresh shell, so it never had `g`.
# This executable on PATH gives every engine-launched session (Claude, Codex,
# OpenCode) the same `g`, resolved fresh on each call.
#
# Endpoint: $GRAFT_ENDPOINT, else $VOGT_GRAFT_ENDPOINT (a deployment setting),
# else graft's own config. Token: $GRAFT_TOKEN or an explicit --token-file
# wins; else $GRAFT_TOKEN_FILE; else, in the session's token directory (keyed
# by $VOGT_SESSION_ID, then $VOGT_ENGINE_SESSION_ID), trial-1.token, else the
# only *.token there. The token is passed as a file path, never in argv.
set -euo pipefail

self="$(readlink -f "${BASH_SOURCE[0]}" 2>/dev/null || echo "${BASH_SOURCE[0]}")"

# The real client: $VOGT_GRAFT_BIN, else ~/.local/bin/graft, else `graft` on
# PATH (never this wrapper, should it be installed under that name too).
graft_bin="${VOGT_GRAFT_BIN:-}"
if [[ -z "$graft_bin" && -x "$HOME/.local/bin/graft" ]]; then
    graft_bin="$HOME/.local/bin/graft"
fi
if [[ -z "$graft_bin" ]]; then
    while IFS= read -r candidate; do
        if [[ "$(readlink -f "$candidate" 2>/dev/null || echo "$candidate")" != "$self" ]]; then
            graft_bin="$candidate"
            break
        fi
    done < <(type -aP graft 2>/dev/null || true)
fi
if [[ -z "$graft_bin" ]]; then
    echo "g: no graft client found (install it at ~/.local/bin/graft or set VOGT_GRAFT_BIN)" >&2
    exit 127
fi

if [[ -z "${GRAFT_ENDPOINT:-}" && -n "${VOGT_GRAFT_ENDPOINT:-}" ]]; then
    export GRAFT_ENDPOINT="$VOGT_GRAFT_ENDPOINT"
fi

explicit=0
for arg in "$@"; do
    case "$arg" in
        --token-file | --token-file=*) explicit=1 ;;
        --) break ;;
    esac
done

token_file=""
if [[ $explicit -eq 0 && -z "${GRAFT_TOKEN:-}" ]]; then
    if [[ -n "${GRAFT_TOKEN_FILE:-}" ]]; then
        token_file="$GRAFT_TOKEN_FILE"
    else
        root="${XDG_CONFIG_HOME:-$HOME/.config}/graft/sessions"
        for id in "${VOGT_SESSION_ID:-}" "${VOGT_ENGINE_SESSION_ID:-}"; do
            [[ -n "$id" && -d "$root/$id" ]] || continue
            if [[ -f "$root/$id/trial-1.token" ]]; then
                token_file="$root/$id/trial-1.token"
                break
            fi
            shopt -s nullglob
            tokens=("$root/$id"/*.token)
            shopt -u nullglob
            if [[ ${#tokens[@]} -eq 1 ]]; then
                token_file="${tokens[0]}"
                break
            fi
            if [[ ${#tokens[@]} -gt 1 ]]; then
                echo "g: several tokens in $root/$id; set GRAFT_TOKEN_FILE to pick one" >&2
                exit 64
            fi
        done
    fi
fi

if [[ -n "$token_file" ]]; then
    if [[ ! -r "$token_file" ]]; then
        echo "g: token file $token_file is not readable (oversight mints it shortly after the session starts)" >&2
        exit 66
    fi
    exec "$graft_bin" --token-file "$token_file" "$@"
fi
exec "$graft_bin" "$@"
