#!/usr/bin/env bash
# Keep Cargo's download caches on the persisted home volume (WI-1256).
#
# The pod image installs Rust under /opt/rust (CARGO_HOME=/opt/rust/cargo)
# because the runtime mounts a volume over /home/sprooty and would hide a
# toolchain installed there. The price was that `registry/` and `git/` — the
# crate index and downloaded sources — lived in the image layer too, so every
# redeploy reset them to the image's build-time snapshot. Offline gates
# (`cargo clippy --offline`) then failed on any crate published since the image
# was built, and agents reported it as a dependency problem.
#
# This moves just those two caches onto the home volume and leaves a symlink
# in CARGO_HOME, so the toolchain and `cargo install`ed binaries still come
# from the image. The first boot seeds the store from the image's copy; later
# boots reuse it. Nothing happens when CARGO_HOME is already under HOME (it
# persists with it) or cannot be written.
#
# Usage: vogt-cargo-cache [store-dir]   (default: $HOME/.cache/vogt-cargo)
set -euo pipefail

cargo_home="${CARGO_HOME:-}"
home="${HOME:-}"
store="${1:-${ENGINE_CARGO_CACHE_DIR:-${home:+$home/.cache/vogt-cargo}}}"

if [[ -z "$cargo_home" || -z "$home" || -z "$store" ]]; then
    exit 0
fi
if [[ ! -d "$cargo_home" || ! -w "$cargo_home" ]]; then
    exit 0
fi
case "$(realpath -m "$cargo_home")/" in
    "$(realpath -m "$home")"/*) exit 0 ;;
esac

mkdir -p "$store"
for sub in registry git; do
    live="$cargo_home/$sub"
    kept="$store/$sub"
    if [[ -L "$live" ]]; then
        # Already linked: this container restarted without being recreated.
        continue
    fi
    if [[ -d "$live" && ! -e "$kept" ]]; then
        # First boot on this volume: keep what the image shipped.
        cp -a "$live" "$kept.partial"
        mv "$kept.partial" "$kept"
    fi
    mkdir -p "$kept"
    rm -rf "$live"
    ln -s "$kept" "$live"
done
echo "cargo caches persisted under $store"
