---
title: "ops + docs: vogt-dev sends workers to pods (host cap, template policy, env renames, worker docs)"
labels: [pods, phase-1, ops, docs, "model:haiku"]
depends_on: [P1.8 deployed, P1.9, P1.6]
vogt: WI-3169 (docs remainder)
lands: ops PR (apps/vogt-dev); workspace docs through agent-workspace-config (exempt from graft)
---

## What to change

1. **ops `apps/vogt-dev/deployment.yaml`:**
   - `ENGINE_HOST_SESSION_CAP` (Q2 default: 4);
   - `ENGINE_HOST_SESSION_SCRATCH_ROOT` on the builds PVC;
   - `ENGINE_EPHEMERAL_LIMIT` from the downward API (P1.9);
   - `ENGINE_AGENT_CLAUDE_ACCOUNT_CAP` (Q3 default: 12);
   - the P1.6 template policy if not already merged.
2. **Rename the deprecated env names** the engine warns about at boot:

   | Old | New |
   |---|---|
   | `ENGINE_POD_GRAFT_ENDPOINT` | `ENGINE_SOURCE_GRAFT_ENDPOINT` |
   | `ENGINE_POD_GRAFT_TOKEN_FILE` | `ENGINE_SOURCE_GRAFT_TOKEN_FILE` |
   | `ENGINE_GRAFT_MINT_TOKEN_FILE` | `ENGINE_SOURCE_GRAFT_MINT_TOKEN_FILE` |
   | `ENGINE_POD_GRAFT_LINE` | `ENGINE_SOURCE_GRAFT_LINE` |
3. **Docs.** Update oversight `worker-setup.md` and the `worker-brief` skill (agent-workspace-config):
   - workers run as pods by default;
   - never pass `placement=host` for a worker or reviewer;
   - the graft token is minted by the engine and written to `~/.config/graft/sessions/$VOGT_SESSION_ID/trial-1.token` in the pod;
   - the clone is at `$VOGT_WORK_DIR` (`/home/vogt/Working/session`);
   - builds run in the pod.

## Done when

1. After the rollout, the engine boot log has no `deprecated source setting` warnings.
2. `session_start` of a worker with `placement=host` on dev is refused, and a 5th concurrent host shell is refused (cap 4).
3. The worker-setup and worker-brief changes are merged in agent-workspace-config, and the hand-off quotes the new lines.
