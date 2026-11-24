# docs/ENGINE.md — Rust-only edits

Apply at the swap (WI-2505).

## Image contents

Where the build section says the image carries the Python core (the sentence
naming the Python stage as what a checkout builds), state that the image
carries the Rust core built from `engine/core`, and drop the Python stage
from the stage list.

## Entrypoint selection

Replace:

> starts the core selected by `VOGT_CORE_IMPL` (default the Python core) on
> loopback when `VOGT_CORE_URL` names a loopback address

with:

> starts the Rust core (`vogt`) on loopback when `VOGT_CORE_URL` names a
> loopback address

`VOGT_CORE_IMPL` is no longer read. Delete every other mention of it in this
file.
