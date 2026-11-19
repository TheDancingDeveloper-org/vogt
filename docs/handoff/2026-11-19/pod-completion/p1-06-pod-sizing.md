---
title: "Rust worker pod sizing from measurement (per-template policy on vogt-dev)"
labels: [pods, phase-1, ops, "model:sonnet"]
depends_on: [P1.4 deployed to vogt-dev, P1.2]
vogt: WI-3166 (narrowed), WI-3181 (remainder), WI-3164 (numbers)
lands: measurements as Vogt comments; config by ops PR (apps/vogt-dev, apps/vogt-sessions)
---

## Why

Today one global policy covers every session pod: `ENGINE_POD_RESOURCE_REQUESTS=cpu=250m,memory=512Mi,ephemeral-storage=2Gi` and `ENGINE_POD_RESOURCE_LIMITS=cpu=4,memory=6Gi,ephemeral-storage=20Gi`. In a Rust worker pod, that 20Gi ephemeral limit is shared by:
- `/work/target` (pods set no `CARGO_TARGET_DIR`; `/work` is an emptyDir);
- kache's local cache (`local_max_size=10GiB` in ConfigMap `kache-config`);
- a cold cargo registry (`CARGO_HOME=/cache/cargo`, an emptyDir; `pod_backend.rs:1065-1074`).

The per-template override already exists: `ENGINE_POD_TEMPLATE_POLICIES` (JSON keyed by exact template name, covering requests, limits, nodeSelector, affinity and deadline; `engine/server/src/pod_backend.rs:28-48,754-757,1544,1747-1756`). It is not set on dev, and the emptyDirs have no `sizeLimit`.

`vogt-sessions` quota (used/hard on 2026-11-19): limits.memory 12Gi/48Gi, limits.ephemeral 40Gi/160Gi, limits.cpu 8/32, pods 2/12.

## What to do

1. **Measure.** In one vogt worker pod on dev (rust image from P1.4, graft token from P1.2), run cold:
   - `cargo clippy --all-targets -- -D warnings` in `engine/`;
   - `cargo nextest run -p vogt-engine-server`.

   Record peak memory (`kubectl top pod` or cgroup `memory.peak`), peak ephemeral use (`du` of `/work/target`, `/cache`, `/tmp`) and wall time. Repeat once warm (same pod, after `cargo clean -p vogt-engine-server`) to show kache's effect.
2. **Set** `ENGINE_POD_TEMPLATE_POLICIES` for the worker and review templates from those numbers. The default the plan assumes (Q4) is limits 4 CPU / 8Gi / 30Gi, requests 1 CPU / 2Gi / 10Gi.
3. **Raise** the `vogt-sessions` ResourceQuota so 5 such pods plus 3 others fit (assumed: limits.memory 64Gi, limits.ephemeral 240Gi).

## Done when

1. Measurements are posted on WI-3166 and WI-3164 (cold and warm: memory, ephemeral, wall time, node).
2. The dev Deployment sets `ENGINE_POD_TEMPLATE_POLICIES`, and a new worker pod's spec shows the template values.
3. The quota admits 8 pods at the new size (checked with `kubectl describe quota`).
4. A deliberate over-limit build in a pod (e.g. `CARGO_BUILD_JOBS=16` against a low test limit) is OOM-killed in that pod only. The session shows the reason, and other pods are unaffected.
