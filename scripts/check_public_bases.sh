#!/usr/bin/env bash
# #620: every base image the *public* Dockerfiles reference must be anonymously
# pullable. "Build from source" (DEPLOYMENT.md §3) is supposed to be a clean,
# estate-independent path; the engine/pod/voice Dockerfiles default their bases
# to this org's GHCR mirror, which works only while those packages stay public.
# If one is flipped private or renamed, a stranger's build breaks at FROM with a
# bare 403/404 and no obvious cause. This asserts anonymous pullability loudly,
# naming the offending ref, so that failure surfaces here instead.
#
# Product images a from-source build produces or overrides locally (the core
# `vogt`, the merged `vogt-stack*`) are excluded — a fork does not pull those.
# A registry/network hiccup is retried and reported distinctly from "private".
set -uo pipefail

attempts="${BASES_ATTEMPTS:-3}"

# The public base refs, read only from real FROM / ARG *IMAGE= lines (never a
# comment): the org GHCR bases the engine/pod/voice Dockerfiles default to, plus
# the root Dockerfile's upstream `python` default (the public path CI overrides
# to the mirror). A `${VAR}` FROM is covered by its own ARG line, so it is
# dropped here. Product images a from-source build makes or overrides locally
# (core `vogt`, the merged `vogt-stack*`) are excluded — a fork never pulls them.
extract() { # $1=file: emit the image token from each FROM/ARG *IMAGE= line
  grep -Eh '^(FROM[[:space:]]|ARG[[:space:]]+[A-Za-z_]*IMAGE=)' "$1" \
    | sed -E 's/^FROM[[:space:]]+//; s/^ARG[[:space:]]+[A-Za-z_]*IMAGE=//' \
    | awk '{print $1}'
}
mapfile -t refs < <(
  {
    extract engine/Dockerfile
    extract engine/Dockerfile.pod
    extract voice/Dockerfile
    extract Dockerfile
  } \
    | grep -E '^(ghcr\.io/thedancingdeveloper-org/|python:|docker\.io/library/python:)' \
    | grep -vE 'thedancingdeveloper-org/(vogt|vogt-stack|vogt-stack-estate|vogt-voice):' \
    | sort -u
)

[ "${#refs[@]}" -gt 0 ] || { echo "::error::no public base refs found — the Dockerfiles or this pattern changed"; exit 1; }

# Split a ref into registry/repository/reference (tag or digest).
# Handles: [registry/]repo[:tag][@sha256:...] — GHCR and docker.io library images.
anon_token() {  # $1=registry_host $2=repository -> prints a bearer token
  case "$1" in
    ghcr.io) curl -fsS "https://ghcr.io/token?scope=repository:$2:pull" 2>/dev/null | sed -nE 's/.*"token":"([^"]+)".*/\1/p' ;;
    docker.io|registry-1.docker.io) curl -fsS "https://auth.docker.io/token?service=registry.docker.io&scope=repository:$2:pull" 2>/dev/null | sed -nE 's/.*"token":"([^"]+)".*/\1/p' ;;
  esac
}

manifest_status() {  # $1=host $2=repo $3=ref $4=token -> prints HTTP status
  local api_host="$1"
  [ "$1" = "docker.io" ] && api_host="registry-1.docker.io"
  curl -o /dev/null -s -w '%{http_code}' \
    -H "Authorization: Bearer $4" \
    -H "Accept: application/vnd.oci.image.index.v1+json" \
    -H "Accept: application/vnd.docker.distribution.manifest.list.v2+json" \
    -H "Accept: application/vnd.oci.image.manifest.v1+json" \
    -H "Accept: application/vnd.docker.distribution.manifest.v2+json" \
    "https://${api_host}/v2/${2}/manifests/${3}" 2>/dev/null
}

bad=0
for ref in "${refs[@]}"; do
  # registry host: first path segment if it contains a dot; else docker.io.
  case "$ref" in
    *.*/*) host="${ref%%/*}"; rest="${ref#*/}" ;;
    *) host="docker.io"; rest="library/$ref" ;;
  esac
  # reference = digest after @, else tag after last :, else 'latest'
  if [[ "$rest" == *@* ]]; then reference="${rest#*@}"; repo="${rest%@*}"; repo="${repo%:*}"
  elif [[ "${rest##*/}" == *:* ]]; then reference="${rest##*:}"; repo="${rest%:*}"
  else reference="latest"; repo="$rest"; fi

  ok=""
  for a in $(seq 1 "$attempts"); do
    token="$(anon_token "$host" "$repo")"
    if [ -z "$token" ]; then
      echo "::warning::could not get an anonymous token for ${host}/${repo} (attempt ${a}/${attempts}) — network/registry hiccup, retrying"
      sleep $((a * 3)); continue
    fi
    code="$(manifest_status "$host" "$repo" "$reference" "$token")"
    case "$code" in
      200) ok=1; echo "  OK (anonymous) ${ref}"; break ;;
      401|403) echo "::error::${ref} is NOT anonymously pullable (HTTP ${code}) — a from-source build would fail at FROM. Make the package public or default the Dockerfile to an upstream image (#620)."; bad=1; break ;;
      404) echo "::error::${ref} does not exist anonymously (HTTP 404) — renamed or removed? A from-source build would fail at FROM (#620)."; bad=1; break ;;
      000|5*) echo "::warning::transient ${code} pulling ${ref} (attempt ${a}/${attempts}); retrying"; sleep $((a * 3)) ;;
      *) echo "::warning::unexpected HTTP ${code} for ${ref} (attempt ${a}/${attempts}); retrying"; sleep $((a * 3)) ;;
    esac
  done
  if [ -z "$ok" ] && [ "$bad" -eq 0 ]; then
    echo "::error::could not confirm ${ref} is anonymously pullable after ${attempts} attempts — registry/network failure, not a private-package finding (#620)."
    bad=1
  fi
done

[ "$bad" -eq 0 ] || exit 1
echo "every public base image the Dockerfiles default to is anonymously pullable (#620)"
