---
title: "Pod image rust stage: clippy, rustfmt, rust-src and a read-only toolchain that resolves `stable`"
labels: [pods, phase-1, feature, "model:sonnet"]
depends_on: []
vogt: WI-3162
lands: graft vogt/rust-2 (image built by the rust2-images workflow; the dev pod digest bump is an ops PR the operator approves)
---

## Why

Workers in pods cannot run the gates (`cargo fmt --check`, `cargo clippy`):

- `engine/Dockerfile.pod:279` installs `rustup-init … --default-toolchain ${RUST_VERSION} --profile minimal`. After that it adds only the targets (`:281`) and `rust-analyzer` (`:286`), so there is **no clippy, rustfmt or rust-src**. `cargo-nextest` (`:285`) is present.
- Session pods run with `readOnlyRootFilesystem: true` (`engine/server/src/pod_backend.rs:1760`). `RUSTUP_HOME=/opt/rust/rustup` is in the image, so `rustup component add` fails, and so does any `rust-toolchain.toml` that asks for components or another channel. graft's file asks for `channel = "stable"` with rustfmt and clippy; the image's toolchain is named `1.94.0`.
- On the engine, where `/opt` is writable, the same mismatch silently installed a 1.4G `1.95.0` toolchain into the 8Gi layer (see P0.1).
- The dev pod image is built with no `target:` (`.github/workflows/rust2-images.yml:129`), so Docker builds the last stage, **mobile** (`Dockerfile.pod:303`), not rust. The comment at `Dockerfile.pod:2` ("default target is base") is wrong. The headers of `pod-base.yml:6-9` and `engine/Dockerfile:34-37` still list sway, selkies and Gradle, which are no longer installed.

## What to change

Files: `engine/Dockerfile.pod`, `.github/workflows/rust2-images.yml`, `.github/workflows/pod-base.yml` (comments), `engine/Dockerfile` (comments), plus a new image test script (e.g. `engine/deploy/tests/pod-image-rust.sh`) wired into the image workflow.

1. In the rust stage install `--profile default` and add `rust-src`.
2. Make the pinned toolchain also answer to `stable`: install `stable` at the pinned version, or `rustup toolchain link stable /opt/rust/rustup/toolchains/<pinned>`. A `rust-toolchain.toml` with `channel = "stable"` and components rustfmt/clippy must then resolve without writing.
3. `ENV RUSTUP_AUTO_INSTALL=0` in the rust stage.
4. Publish the rust stage as its own tag, and point the vogt-dev pod image at the **rust** target. The mobile stage stays a separate tag for mobile projects.
5. Fix the stale comments.

## Done when

1. The image test runs as uid 1000 with `/opt/rust` mounted read-only (`docker run --read-only --tmpfs /tmp --tmpfs /home/vogt …`). It runs `cargo fmt --version`, `cargo clippy --version` and `cargo nextest --version`, then `cargo clippy` in a scratch crate whose `rust-toolchain.toml` says `channel = "stable"`. All pass. The test fails against the current image digest `vogt-dev-pod@sha256:4be1b2f6…`.
2. `rust2-images.yml` builds and pushes the rust target; the workflow log shows the image size. Record it on WI-3162.
3. The comments in `Dockerfile.pod:2`, `pod-base.yml:6-9` and `engine/Dockerfile:34-37` match what is installed.
4. With `RUSTUP_AUTO_INSTALL=0`, a toolchain-file mismatch fails with a clear error (covered by the image test).
