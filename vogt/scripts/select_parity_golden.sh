#!/bin/sh
# Pick the committed parity golden a checkout should be checked against.
#
# Prefer the directory named for the first 7 hex chars of the merge-base of
# HEAD and origin/main. When that recording is not in the tree, fall back to
# the newest golden whose introducing commit is an ancestor of HEAD,
# ordered by that commit's committer time. The introducing commit is the
# one that added the directory: a graft land squashes, so the commit the
# directory is named for is not in the history CI checks out. File mtime is not a commit order: a fresh checkout
# gives every directory the same stamp (WI-2662).
#
# Prints the golden directory on stdout. A fallback is announced on stderr.
# Exits 1 when nothing qualifies.
set -eu

golden_root=${1:-tests/parity/golden}

base=$(git merge-base HEAD origin/main)
short=$(printf '%s' "$base" | cut -c1-7)
preferred="$golden_root/$short"
if [ -d "$preferred" ]; then
  printf '%s\n' "$preferred"
  exit 0
fi

best_time=0
best_sha=""
best_name=""
for dir in "$golden_root"/*/; do
  [ -d "$dir" ] || continue
  name=$(basename "$dir")
  case "$name" in
    [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) ;;
    *) continue ;;
  esac
  # The directory is named for the commit the golden was recorded against.
  # A graft land squashes, so that commit is not in the GitHub history the
  # job checks out. Date the golden by the commit that added its directory,
  # which is in this history, and fall back to the recorded commit when it
  # is (a plain git checkout, or the selection test).
  sha=$(git log -1 --format=%H -- "$dir" 2>/dev/null || true)
  if [ -z "$sha" ]; then
    sha=$(git rev-parse --verify "${name}^{commit}" 2>/dev/null || true)
  fi
  [ -n "$sha" ] || continue
  git merge-base --is-ancestor "$sha" HEAD || continue
  stamp=$(git log -1 --format=%ct "$sha")
  # A graft batch stamps every land with one committer time, so time alone
  # ties. The later introducing commit is the newer recording.
  if [ "$stamp" -gt "$best_time" ] || {
    [ "$stamp" -eq "$best_time" ] && [ -n "$best_sha" ] &&
      git merge-base --is-ancestor "$best_sha" "$sha"
  }; then
    best_time=$stamp
    best_sha=$sha
    best_name=$name
  fi
done

if [ -z "$best_name" ]; then
  echo "no golden for $base" >&2
  exit 1
fi
echo "warning: no golden for merge-base ${short}; using ancestor golden ${best_name}" >&2
printf '%s\n' "$golden_root/$best_name"
