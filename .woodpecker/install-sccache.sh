#!/usr/bin/env bash
set -euo pipefail

SCCACHE_VERSION="${SCCACHE_VERSION:-0.10.0}"
SCCACHE_SHA256="${SCCACHE_SHA256:-1fbb35e135660d04a2d5e42b59c7874d39b3deb17de56330b25b713ec59f849b}"
ARCHIVE="sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz"
URL="https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${ARCHIVE}"

curl -fsSL "$URL" -o /tmp/sccache.tar.gz
echo "${SCCACHE_SHA256}  /tmp/sccache.tar.gz" | sha256sum -c -
tar -xzf /tmp/sccache.tar.gz -C /tmp
mv "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" /usr/local/bin/sccache
chmod +x /usr/local/bin/sccache
rm -rf /tmp/sccache.tar.gz "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl"
