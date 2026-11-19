#!/usr/bin/env bash
# Create GitHub issues from the drafts in this directory.
# Usage: create-issues.sh <owner/repo> [pod-completion|backlog|all] [--dry-run]
# Each draft has a front-matter block (title, labels). The body is the rest of the file.
# Needs gh with GH_TOKEN. Labels that don't exist are created once.
# The backlog set is large (665): create it in batches, or use --dry-run first.
set -euo pipefail
repo=${1:?owner/repo}; set_=${2:-pod-completion}; dry=${3:-}
here=$(cd "$(dirname "$0")" && pwd)
case $set_ in
  pod-completion) files=$(ls "$here"/pod-completion/p*.md "$here"/pod-completion/phase-2.md) ;;
  backlog) files=$(ls "$here"/backlog/items/*.md) ;;
  all) files=$(ls "$here"/pod-completion/p*.md "$here"/pod-completion/phase-2.md "$here"/backlog/items/*.md) ;;
  *) echo "unknown set $set_" >&2; exit 2 ;;
esac
declare -A made
for f in $files; do
  title=$(awk '/^---$/{n++; next} n==1 && /^title:/{sub(/^title: */,""); gsub(/^"|"$/,""); print; exit}' "$f")
  labels=$(awk '/^---$/{n++; next} n==1 && /^labels:/{sub(/^labels: *\[/,""); sub(/\]$/,""); gsub(/"/,""); print; exit}' "$f")
  body=$(mktemp); awk '/^---$/{n++; next} n>=2' "$f" > "$body"
  args=(--repo "$repo" --title "$title" --body-file "$body")
  IFS=',' read -ra ls <<< "$labels"
  for l in "${ls[@]}"; do
    l=$(echo "$l" | xargs); [ -n "$l" ] || continue
    if [ -z "${made[$l]:-}" ] && [ "$dry" != "--dry-run" ]; then gh label create "$l" --repo "$repo" --force >/dev/null 2>&1 || true; made[$l]=1; fi
    args+=(--label "$l")
  done
  if [ "$dry" = "--dry-run" ]; then echo "would create: $title [$labels]"; else gh issue create "${args[@]}"; fi
  rm -f "$body"
done
