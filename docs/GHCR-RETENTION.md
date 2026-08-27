# GHCR retention

Container retention for this repository is governed by the organization-wide
policy in [github-policy](https://github.com/TheDancingDeveloper-org/github-policy/blob/194ee9a0f4fad7131c4f5050d8b17af6f485d895/docs/GHCR-RETENTION.md).
The local workflow pins a reviewed `github-policy` revision and declares this
repository's package names; it does not define separate retention numbers or
cleanup rules. The pin is executable supply-chain input and is updated with
the organization policy, never replaced with a moving branch reference.

The scheduled run is a dry run and uploads before/after counts and a deletion
plan. Applying a plan requires an explicit workflow dispatch after review.
