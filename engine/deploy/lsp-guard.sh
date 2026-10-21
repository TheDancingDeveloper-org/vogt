#!/usr/bin/env bash
# Trust gate and env sanitizer for language servers (WI-889, design
# docs/design/shared-language-servers.md §7, D4).
#
# A language server that runs build.rs, proc macros or a Gradle/Maven import
# executes project code. That is only allowed for a trusted or opted-in root;
# every other root gets a "static" mode. Whatever the mode, the server never
# inherits engine or vault credentials: this deployment runs agent-auth in
# identity mode, so an inherited INFISICAL_CLIENT_SECRET would hand the whole
# vault to any build script.
#
# A root is trusted when VOGT_LSP_TRUST is `trusted` or `opt-in`, or when it is
# inside one of the colon-separated VOGT_LSP_TRUSTED_ROOTS. Anything else is
# static.
#
# Usage:
#   vogt-lsp-guard [--root DIR] -- COMMAND [ARG]...
#       Run COMMAND with a sanitized env, VOGT_LSP_MODE=trusted|static, and,
#       when static, XDG_CONFIG_HOME pointing at a rust-analyzer.toml that
#       turns off build scripts, proc macros and check-on-save. A jdtls
#       launcher reads VOGT_LSP_MODE to leave Gradle and Maven import off.
#   vogt-lsp-guard --check [DIR]
#       Print the mode; exit 0 when trusted, 3 when static. Prewarmers call
#       this and never prewarm a static root.
set -euo pipefail

root="${PWD}"
check=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --root) root="$2"; shift 2 ;;
        --check) check=1; shift; [[ $# -gt 0 && "$1" != --* ]] && { root="$1"; shift; } ;;
        --) shift; break ;;
        *) echo "vogt-lsp-guard: unknown argument: $1" >&2; exit 2 ;;
    esac
done

root="$(realpath -m "$root")"

mode=static
case "${VOGT_LSP_TRUST:-}" in
    trusted|opt-in) mode=trusted ;;
esac
if [[ "$mode" == static && -n "${VOGT_LSP_TRUSTED_ROOTS:-}" ]]; then
    IFS=: read -r -a trusted_roots <<<"${VOGT_LSP_TRUSTED_ROOTS}"
    for t in "${trusted_roots[@]}"; do
        [[ -z "$t" ]] && continue
        t="$(realpath -m "$t")"
        if [[ "$root" == "$t" || "$root" == "$t"/* ]]; then
            mode=trusted
            break
        fi
    done
fi

if [[ "$check" -eq 1 ]]; then
    echo "$mode"
    [[ "$mode" == trusted ]] && exit 0 || exit 3
fi

if [[ $# -eq 0 ]]; then
    echo "vogt-lsp-guard: no command given" >&2
    exit 2
fi

# The PTY sanitizer's strip list (pty.rs), plus agent-auth's own state.
unset_args=()
while IFS= read -r name; do
    case "$name" in
        ENGINE_*|INFISICAL_*|AGENT_AUTH_*|*TOKEN*|*SECRET*|*PASSWORD*|*API_KEY*)
            unset_args+=(-u "$name") ;;
    esac
done < <(compgen -e)

if [[ "$mode" == static ]]; then
    state="${VOGT_LSP_STATE_DIR:-${HOME:-/tmp}/.cache/vogt-lsp}"
    conf="$state/static-config"
    mkdir -p "$conf/rust-analyzer"
    cat >"$conf/rust-analyzer/rust-analyzer.toml" <<'EOF'
# Written by vogt-lsp-guard: this root is not trusted, so nothing in it runs.
checkOnSave = false

[cargo.buildScripts]
enable = false

[procMacro]
enable = false
EOF
    set -- env "${unset_args[@]}" VOGT_LSP_MODE=static XDG_CONFIG_HOME="$conf" "$@"
else
    set -- env "${unset_args[@]}" VOGT_LSP_MODE=trusted "$@"
fi

cd "$root"
exec "$@"
