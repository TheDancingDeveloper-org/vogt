#!/bin/sh
# Generic deployment-owned example. Mount this script and its source bundle
# under /run/vogt/hooks; it is never copied into a Vogt image.
#
# Required environment:
#   VOGT_HOOK_SOURCE_DIR  read-only restore source
#   VOGT_HOOK_TARGET_DIR  writable target directory
# Optional:
#   VOGT_HOOK_REQUIRED_PATH  a path relative to target to verify

set -eu

source_dir=${VOGT_HOOK_SOURCE_DIR:?set VOGT_HOOK_SOURCE_DIR}
target_dir=${VOGT_HOOK_TARGET_DIR:?set VOGT_HOOK_TARGET_DIR}
required_path=${VOGT_HOOK_REQUIRED_PATH:-}

[ -d "$source_dir" ] || {
    echo "lifecycle restore: source bundle is missing: $source_dir" >&2
    exit 78
}

if [ -d "$target_dir/.git" ]; then
    dirty=$(git -C "$target_dir" status --porcelain)
    [ -z "$dirty" ] || {
        echo "lifecycle restore: refusing to overwrite dirty checkout: $target_dir" >&2
        exit 73
    }
fi

mkdir -p "$target_dir"
cp -R "$source_dir"/. "$target_dir"/

if [ -n "$required_path" ]; then
    [ -e "$target_dir/$required_path" ] || {
        echo "lifecycle restore: required asset is missing: $target_dir/$required_path" >&2
        exit 78
    }
fi

echo "lifecycle restore: restored and verified $target_dir" >&2
