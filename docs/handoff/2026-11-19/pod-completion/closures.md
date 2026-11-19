# Items to close, narrow or supersede (needs decision Q7)

Evidence is from vogt `rust-2` @ `ce761a7` and live vogt-dev on 2026-11-19.

| Item | Action | When | Evidence |
|---|---|---|---|
| WI-3244 write-back auth | close (fixed by WI-3334) | now | `presenting_session` falls back to `agent_auth.session_for_token` (`engine/server/src/secret_broker.rs:1995-2006`, `2026-2028`); test `a_claude_pod_gets_a_signed_in_bundle_and_can_write_it_back`. The live 2xx is observed in P1.12 step 4. |
| WI-3221 smoke-pod harness | close | now | ops `scripts/smoke-pod`: a lost pod is a FAIL (`:207-221,303,577,584-590`); diagnostics (`:260-293,568-573`); unique image tag (`:107-127,164`); the NetworkPolicy allows sessions→engine (`apps/vogt-smoke/networkpolicy.yaml:29-37,85-89`); the entrypoint retries (`session-entrypoint.sh:39-48`). Item 5 (leftover `--keep` objects): open a fresh chore if `kubectl -n vogt-smoke get all` still shows them. |
| WI-3181 pod spec part 2 | close | now | Code done: whole-claim `/cache`, envFrom, `/etc/kache`, nodeName pin via `CACHE_NODE` (`pod_backend.rs:1547-1669,1787`). The per-node writable cache premise is superseded by WI-3242 (no writable shared volume). Remaining config is P1.6. |
| WI-3163 pod boot | close | now | Every entrypoint step is implemented (`engine/deploy/session-entrypoint.sh:13-378`). Verified on dev 2026-11-06: warm boot to a working turn about 17 s, brief seeded. The `g status` check moves to P1.2. |
| WI-3229 pod boot follow-ups | close | after P1.2 | Mint and revoke are done and live-enabled; the only blocker was `landing.source`. Items 2–6 → P1.11. |
| WI-3161 MCP and agent-auth in pod | close | after P1.12 step 4 | Code done (`agent_stream.rs:1182-1210`, `session-entrypoint.sh:92-201`). |
| WI-3160 write-back | close | after P1.3 + P1.12 step 4 | Code and tests done; starved by WI-4418 in practice. |
| WI-3158 hibernate/wake | close | after P1.12 step 5 | Code and integration test done (`integration.rs:23481`). |
| WI-3169 workers and reviews as pods | close | after P1.12 | — |
| WI-3166 per-template resources | narrow → P1.6 | now | `ENGINE_POD_TEMPLATE_POLICIES` exists (`pod_backend.rs:28-48`); only the measurement, defaults and OOM test remain. |
| WI-3162 session images | narrow → P1.4 + P2.3 | now | The stages exist; the toolchain and the project map are missing. |
| WI-895 per-session cgroups | supersede → P1.8 + P2.6 | now | Branch `wi-895-session-cgroups` was never ported to rust-2. With workers in pods, only the capped host exceptions remain, and those go away in P2.6. |
| WI-2698 prod compile load | supersede → P2.9 | after P1.12 | — |
