#!/usr/bin/env bash
# WI-3162 / P1.4: the Rust toolchain in a dev pod image, as a session sees it.
#
# A session pod runs as uid 1000 on a read-only root filesystem, so the
# toolchain must work without writing anywhere under /opt/rust. This runs the
# checks that way and fails on the first one that cannot pass:
#
#   - cargo fmt, clippy and nextest are installed and report a version
#   - a scratch crate whose rust-toolchain.toml says `channel = "stable"` with
#     rustfmt and clippy resolves to the installed toolchain and passes clippy
#   - a toolchain file that names a version the image does not have fails with
#     rustup's "not installed" error instead of trying to install it
#
# Usage: pod-image-rust.sh <image-ref>
#   image-ref is a tag or, in CI, the digest reference the image was pushed as.
set -euo pipefail

image="${1:?usage: pod-image-rust.sh <image-ref>}"

# The checks run as the session user (uid 1000) in a container with a read-only
# root filesystem, as a session pod runs. Scratch space is tmpfs. CARGO_HOME
# moves to tmpfs because this test fetches no crates; the image's /cache/cargo
# belongs to the runtime.
echo "pod image rust check: ${image}"
docker run --rm -i \
    --pull=missing \
    --read-only \
    --tmpfs /tmp \
    --tmpfs /home/vogt \
    --user 1000:1000 \
    --env CARGO_HOME=/tmp/cargo-home \
    --env CARGO_TARGET_DIR=/tmp/target \
    --entrypoint /bin/sh \
    "$image" -s <<'SH'
set -eu

# The image sets RUSTUP_AUTO_INSTALL=0 (engine/Dockerfile.pod, rust stage).
[ "${RUSTUP_AUTO_INSTALL:-}" = 0 ] || { echo "FAIL: RUSTUP_AUTO_INSTALL is '${RUSTUP_AUTO_INSTALL:-unset}', want 0" >&2; exit 1; }

# /opt/rust must not be writable from inside the pod.
if touch /opt/rust/.write-probe 2>/dev/null; then
    echo "FAIL: /opt/rust is writable; the pod must mount it read-only" >&2
    exit 1
fi

cargo fmt --version
cargo clippy --version
cargo nextest --version

work=/tmp/scratch
mkdir -p "$work/src"
cd "$work"
printf '[package]\nname = "scratch"\nversion = "0.1.0"\nedition = "2021"\n' > Cargo.toml
printf 'fn main() {\n    println!("ok");\n}\n' > src/main.rs

printf '[toolchain]\nchannel = "stable"\ncomponents = ["rustfmt", "clippy"]\n' > rust-toolchain.toml
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# A file that names a toolchain the image does not carry must fail, and say why.
printf '[toolchain]\nchannel = "1.0.0"\n' > rust-toolchain.toml
if out="$(cargo --version 2>&1)"; then
    echo "FAIL: a toolchain file for 1.0.0 resolved with auto-install off: ${out}" >&2
    exit 1
fi
case "$out" in
    *"not installed"*) echo "mismatch refused as expected" ;;
    *) echo "FAIL: mismatch failed without the 'not installed' error: ${out}" >&2; exit 1 ;;
esac

echo "pod image rust checks passed"
SH
