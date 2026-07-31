#!/usr/bin/env bash
set -euo pipefail

# Resolves the latest sccache release via the GitHub API and verifies the
# downloaded archive against the sha256 digest that same API response
# publishes for the asset — no version/checksum to keep in sync by hand.
command -v jq >/dev/null 2>&1 || (apt-get update -qq && apt-get install -y -qq jq >/dev/null)

api="https://api.github.com/repos/mozilla/sccache/releases/latest"
curl -fsSL "$api" -o /tmp/sccache-release.json
sccache_version=$(jq -r '.tag_name | ltrimstr("v")' /tmp/sccache-release.json)
archive="sccache-v${sccache_version}-x86_64-unknown-linux-musl.tar.gz"
asset_url=$(jq -r --arg n "$archive" '.assets[] | select(.name == $n) | .browser_download_url' /tmp/sccache-release.json)
asset_digest=$(jq -r --arg n "$archive" '.assets[] | select(.name == $n) | .digest | ltrimstr("sha256:")' /tmp/sccache-release.json)

curl -fsSL "$asset_url" -o /tmp/sccache.tar.gz
echo "${asset_digest}  /tmp/sccache.tar.gz" | sha256sum -c -
tar -xzf /tmp/sccache.tar.gz -C /tmp
mv "/tmp/sccache-v${sccache_version}-x86_64-unknown-linux-musl/sccache" /usr/local/bin/sccache
chmod +x /usr/local/bin/sccache
rm -rf /tmp/sccache.tar.gz "/tmp/sccache-v${sccache_version}-x86_64-unknown-linux-musl" /tmp/sccache-release.json
