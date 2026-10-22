#!/usr/bin/env bash
# Render every screen of the mockup, light and dark, into docs/screenshots/ (WI-2162).
#
#   scripts/screenshots.sh
#
# The images come from tests/screenshots.rs through a wgpu adapter. The script runs it in a
# pinned Rust image with Mesa lavapipe (software Vulkan), the way fluent2-egui renders its
# gallery, so it needs no GPU. Needs Docker unless already inside that image.
set -euo pipefail

IMAGE="rust:1.95-trixie"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${MOCKUP_SHOTS_IN_CONTAINER:-}" != 1 ]]; then
  # Stream the crate in and the images back out as tar, so it also works against a Docker
  # daemon that cannot see this filesystem. The container's stdout carries only the tar.
  cd "$root"
  tar --exclude=./target --exclude=./dist -cf - . \
    | docker run --rm -i \
        -e MOCKUP_SHOTS_IN_CONTAINER=1 \
        -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
        -v vogt-gui-mockup-cargo-registry:/usr/local/cargo/registry \
        -v vogt-gui-mockup-cargo-git:/usr/local/cargo/git \
        -v vogt-gui-mockup-target:/target \
        -e CARGO_TARGET_DIR=/target \
        "$IMAGE" bash -c "mkdir -p /work && tar -xf - -C /work && cd /work \
          && scripts/screenshots.sh >&2; s=\$?; mkdir -p docs/screenshots; tar -cf - docs/screenshots; exit \$s" \
    | tar -xf - -C "$root"
  exit "${PIPESTATUS[2]}"
fi

export DEBIAN_FRONTEND=noninteractive
if ! [[ -e /usr/share/vulkan/icd.d/lvp_icd.json ]]; then
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends mesa-vulkan-drivers >/dev/null
fi
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
export WGPU_BACKEND=vulkan

rm -f docs/screenshots/*.png
UPDATE_SNAPSHOTS=1 cargo test --features snapshots --test screenshots
# Keep only the images; a failed comparison leaves .new/.diff files beside them.
rm -f docs/screenshots/*.new.png docs/screenshots/*.diff.png docs/screenshots/*.old.png
