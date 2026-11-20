# Brief goldens

Rust-native snapshots of the brief renderer in `../brief.rs`. Regenerate after an
intended change:

    UPDATE_SNAPSHOTS=1 cargo test -p vogt-core brief

`scripts/gen_brief_golden.py` (the Python recorder) is retired (WI-4467
follow-up); do not run it. The `text/` files are renderer inputs, not goldens.
