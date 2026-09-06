#!/bin/sh
# A deployment-owned lifecycle hook (#614), mounted read-only at
# /run/vogt/hooks and run by the image's generic runner before the core and
# engine start. It is NOT part of the image — a stranger reads this file to see
# exactly what a hook may assume and must uphold.
#
# The runner provides the phase, the working directory, the hook root, and
# whether this is the first start; a hook must be idempotent (safe to rerun on
# every restart) and exits non-zero to fail the service deliberately. Pass any
# credentials a real hook needs through a Compose secret/config mount, never
# through the image or Git.
set -eu

marker="${HOME:-/home/sprooty}/.vogt-custom-stack-hook-ran"

# Idempotent: record that the deployment's pre-start hook fired. A real hook
# would restore a bundle, warm a cache, or register with an estate service here.
printf 'custom-stack pre-start hook ran at %s (phase=%s)\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${VOGT_HOOK_PHASE:-pre-start}" >>"$marker"

echo "custom-stack: pre-start hook ok"
