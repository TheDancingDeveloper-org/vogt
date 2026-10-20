# Vogt backlog audit — 2026-10-17

A read-only audit of the 283 open, in_progress and blocked items in project `vogt`. No item was changed: everything here is a proposal.

Sources:
- Vogt MCP: `work_list`, `work_get`, `session_list`.
- `origin/main` and `origin/rust-core`, checked with commit greps by WI ref and `git cherry`.
- `gh` on TheDancingDeveloper-org/vogt. It shows **no open PRs**; landing is branch-push only.
- `docs/rust-port/gap-audit-2026-10-17.md`, on branch `docs/rust-port-gap-audit-2026-10-17`.

Live sessions at audit time: WI-1067, 1071, 1072, 1080, 1749 and 1751. Every other linked session is dead; codex work was rescued to `salvage/2026-10-17/*`.

## Summary

| Class | Count (approx.) |
|---|---|
| done-but-open (close) | ~120 |
| stale / duplicate / superseded | ~30 |
| blocked | ~54 |
| in flight (live session or unmerged branch) | ~18 |
| ready | ~61 (about 15 of them ops-only or operator-gated) |

The counts are approximate because some rows are "likely done, verify first". **Two systemic causes:**
1. The forge sync doesn't close a Vogt mirror when its GitHub issue closes. Seventeen or more gh# and WI rows mirror issues that closed on 2026-09-08 or earlier.
2. Landing a branch never transitions its item, so done work stays open or in_progress. That accounts for most of the ~120.

**Both deserve a bug item. Closing items in batches alone will not keep the backlog clean.**

### Proposed batch closes (done-but-open, with evidence in the tables below)

- **Terminal attach series:** WI-121–131.
- **GitHub mirrors closed upstream:** WI-51, 73, 74, 84; gh#540, 467, 545, 613, 616, 620, 622, 626, 632, 639.
- **Call mode:** WI-960 and WI-965–970.
- **Earlier fixes:** WI-171, 177, 642, 643, 698, 774, 891, 952, 987, 1011, 1013, 1053, 1097, 1146, 1149.
- **Context management / H-series:** WI-1243, 1248, 1250, 1251, 1256, 1269, 1270, 1279, 1281, 1289, 1298, 1299, 1302, 1304, 1309, 1310, 1312, 1313, 1316, 1318, 1321, 1323, 1355, 1380, 1411, 1412, 1413, 1518.
- **Codex / CI / rail:** WI-1590, 1594, 1598, 1630, 1632, 1634, 1638–1641, 1645, 1647, 1664, 1667, 1670, 1680–1682, 1687, 1736, 1742, 1746, 1748.
- **Close after verifying:**
  - WI-700, 1059, 1061, 1063, 1066, 1068, 1095, 1224, 1351, 1592, 1612.
  - WI-1635 and WI-1644: close once deployed.
  - WI-1671 and WI-1672: the Python side is done; move the Rust remainder to gap audit WP5–7.

### Proposed stale / duplicate closes

- **Stale:** WI-64, 66, gh#273, WI-181, 818, 852, 861, 995, 1010, 1014, 1093, 1260, 1280, 1529, 1661.
- **Superseded by WI-1151:** WI-1007, 1008, 1009, 1012, 1098.
- **Duplicates:**
  - WI-1030 → WI-1007
  - WI-1018 → WI-1013
  - WI-1371 → WI-1359
  - WI-1621 → WI-1631
  - gh#583 → WI-695
  - WI-1574 → WI-1634
- **Wont-do under the operator's "NO caps" ruling:** WI-894, and WI-895 unless the operator re-opens caps.
- **Umbrellas to re-scope or close:** gh#188, WI-826, WI-1265, WI-1517, WI-1603.
- **Partial, re-scope to the remainder:**
  - gh#538
  - WI-1017
  - WI-1373
  - WI-1381
  - WI-1629: retarget to voice/Cargo.lock, which still has rustls 0.23.45
  - WI-1725: split the read-only onboard, which landed on rust-core, from consolidate

### Ready: priority order

1. **WI-1074** S8b sessions II: the Rust critical path, unstaffed. Mark it in_progress and staff it.
2. **WI-1064** P2.6 external adapters: critical path, unstaffed.
3. **WI-1725 remainder** (forge.onboard consolidate): critical path.
4. **WI-939** rotate the old prod agent token (p1 security, operator/Infisical). Run in parallel with **WI-1223** (rotate FORGEJO_TOKEN).
5. **WI-1000** grants security gap (p1).
6. **WI-1001** a blind Enter can approve a prompt (autopilot.rs).
7. **WI-1058** P1.9 application gaps: the branch exists, 5 ahead of rust-core, no session.
8. **WI-1324** workspace record: unblocks WI-1325, 1326, 1340 and 1330.
9. **WI-1341** GitOpReq::Land: the real head of the landing chain (1328 → 1330).
10. **WI-1520** session_report op: unblocks WI-1521, 1523 and 1524.
11. Then:
    - **WI-1151**, p3 → p2: closes six items.
    - **WI-1319**, then 1320.
    - **WI-1744**, which unblocks I4.
    - **WI-1747**, which feeds I5.
    - WI-1331 and 1343, then WI-1327, 1329, 1322, 1334, 1253, 1254, 954, 799, 1658, 1660 and 1576.

**Safe to run in parallel** (no shared files or goldens):
- **Engine:** WI-1324 (sessions.rs + new workspace.rs), WI-1341 (git.rs), WI-1001 (autopilot.rs) and WI-1331/1343 (agent-auth). Watch that WI-1324 and WI-1520 both touch engine sessions.rs.
- **Core:** WI-1319 (core/entities.py) and WI-1000 (grants).
- **Port:** WI-1074 (core services/sessions) and WI-1064 (core adapters), as long as neither re-records goldens (see the golden process below).
- **CI, tests and web:** WI-1658/1660 (workflows), WI-954 (integration.rs) and WI-1254 (web).

**Serialize:**
- WI-1327 and WI-1326 both edit policy.rs.
- WI-1339 can re-cut demo baselines.
- Every parity slice (WI-1067/1069/1070/1071/1072/1073) waits for the golden lane.

### Priority and effort mismatches

- **Should drop:**
  - p1 items blocked behind unbuilt work (WI-1325, 1326, 1328, 1340) while ready work waits.
  - WI-1603: p1 umbrella whose fixes have merged → p2.
  - WI-1661 and WI-852: obsolete p1 items.
  - WI-1659: conditional → p4.
  - Pipeline S4–S6 (WI-1651–1653): p1, but the design WI-1646 is unapproved.
- **Contradicted by rulings:** WI-894 and 895 are p1 against "NO caps". WI-893 is p1 with stale 10-05 data.
- **Under-ranked:** WI-1151 (p3, unblocks six items), WI-1000 and WI-1001 (security), WI-1324 (unblocks four), WI-1341.
- **No effort set:** WI-885–899 and WI-939–954.
- **Wrong state:** WI-1074 is open but has 26 rust-core commits. WI-1053 is DONE per the gap audit but still in_progress. About 15 P-items (WI-1043…1062) sit in `review` with no reviewer.

### Bodies stale against current code

- **WI-1629:** targets the engine, which is fixed; voice still has the vulnerable version.
- **WI-1725:** says it "unblocks consolidate", but a read-only port has landed.
- **WI-1017:** assumes an MSRV bump, against the stable 1.99 / floor 1.88 policy.
- **WI-996:** predates session_report_blocked, decision.ask and AUTOPILOT DONE.
- **WI-1260:** LiteLLM is already the router.
- **WI-1337:** superseded_by is handled in 21 files; it may already be done.
- **WI-1339:** the inbox row exists; only the PWA card is left.
- **WI-1315:** the docs rule landed; only the CI check is left.
- **WI-1317:** the design doc lives only on a branch while its children ship.
- **WI-891:** the ref was reused for terminal-resync commits.
- **WI-66 and WI-818:** name an old branch topology and an old SHA.
- **WI-1595:** PR #1007 was closed and never landed, yet WI-1527 depends on it.

### Branch clutter (prune after verifying)

`wi-973-engine-test-hang`, `fix/wi-995-codeql-ram`, `wi-1068-s3-work-ops-v2`, `wi-1066-s1-instance-auth`, `wi-1314-auto-unblock-b`, and the salvage branches for WI-1270, 1373, 1411, 1413 and 1224.

### Operator decisions surfaced (not asked here)

- **Make the rust-core Playwright step a gate** (I1). WI-1695 is marked done but conflicts with it.
- **WI-885:** decisions D1–D12.
- **WI-1646:** sign off the pipeline design.
- **WI-1637:** the requirements.toml pin.
- **WI-993:** Dependabot policy.
- **WI-1297:** six decisions.
- **WI-941:** prod hibernate policy.
- **WI-1342:** the brokered flip on dev.
- **WI-895:** caps.

## Rust-port critical path to the swap (I1 → I7)

rust-core tip `cf2d3ca6`. Live claude sessions: WI-1067, 1071, 1072, 1080, 1749, 1751. Codex sessions showing `attention=unknown` are dead; their work is on `salvage/2026-10-17/*`.

**Feeding I1:** I1 (WI-1076) depends on S9 (WI-1075, in_progress). S9 is waiting on:
- **WI-1064** (P2.6 external adapters): in_progress, no live session.
- **WI-1074** (S8b sessions II): open, no live session, salvage branch exists.
- **WI-1725** (forge.onboard): open, not started. It unblocks `project.import consolidate=true`.

**Nobody is working on these three, so they are the current bottleneck.**

### Slices still showing GAPS

| Slice | WI | Status |
|---|---|---|
| S1 | WI-1066 | Open. Branches `wi-1066-s1-instance-auth` and `-s1-report`, plus salvage. Needs review. |
| S2 | WI-1067 | Live. Branch `wi-1067-s2-gap-close` is 6 ahead, 8 behind. Re-records the golden. |
| S3 | WI-1068 | No session. Branches `wi-1068-s3-work-ops` and `-v2`. |
| S4 | WI-1069 | Core work landed (`9007667c`, `cf2d3ca6`). `wi-1069-golden-fix` is 1 ahead, 1 behind. Near review. |
| S5 | WI-1070 | No session. `wi-1070-s5-parity` is 2 ahead, 8 behind. Re-records the golden. |
| S6 | WI-1071 | Live. `wi-1071-s6-gaps` is 7 ahead, 0 behind. Re-records the golden. |
| S7 | WI-1072 | Live. The reviewer found one blocker. `wi-1072-s7-forge-gaps` is 5 ahead, 0 behind. |
| S8a | WI-1073 | No session. 4 ops have no parity step. Drift items WI-1316/1289/1672/1671/1255/1251 are still open. |
| P1.9 | WI-1058 | Open. Branch `wi-1058-application-gaps`. |
| P2.1/2.3/2.5 | WI-1059/1061/1063 | WI-1061's OpenAPI work landed (`54d16659`, `48753304`). WI-1742 is partly landed (`3314e9af`, `8f7c4c2e`, `901c0503`). Audit these: they may be done-but-open. |
| Schemas | WI-1748/1749 | WI-1748 step 1 landed (`360cd679`). WI-1749 is live. |

### Order of the I-items

1. **I1 WI-1076.** Still left:
   - S9 has to close.
   - PR #1045 (base `wi-1443-main-sync`) has to merge.
   - Playwright in `rust-core.yml` is advisory (`|| echo`). Three specs fail against the Rust core: gui login-errors, terminalReplay reattach and demo stale-provenance. **Operator decision, never asked: make it a gate.** WI-1695 is marked done but conflicts with this.
2. **I2 WI-1077, PWA contract:** after I1. Its codex session died; work is on salvage.
3. **I3 WI-1078, stack image:** after I1 and WI-1751 (live). `engine/Dockerfile` still has the uv and CORE_IMAGE stages. Branch `wi-1078-i3-stack-image` exists.
4. **I4 WI-1079, scripts:** salvage only. Can run in parallel after I1. Also needs WI-1744.
5. **I5 WI-1080, test residue:** live, CI running. WI-1745 and WI-1747 roll into it. Can run in parallel with I2–I4.
6. **I6 WI-1081, drift closure and final golden refresh:** needs every S slice, plus I4 and I5.
7. **I7 WI-1082, the swap PR:** after I2, I3 and I6.

Path: (WI-1074 ∥ WI-1064 ∥ WI-1725) → S9 WI-1075 → I1 (+ gate decision) → (I2 ∥ I3 + WI-1751) → I6 → I7. I4 and I5 run alongside until I6.

Bookkeeping: about 15 P-items (WI-1043…1062) sit in `review` with no reviewer, so they need a close-or-review sweep. The gap audit rates WI-1053 DONE, but it is still in_progress.

## Parity-golden contention (`tests/parity/golden/21c95f6/`)

These five branches all rewrite `21c95f6/{cli,http,mcp,meta}.json`, and most also edit `tests/parity/script.json`:

| Branch | Ahead/behind rust-core | Golden churn | Also touches |
|---|---|---|---|
| wi-1067-s2-gap-close | 6/8 | +1663/−46 | script.json, parity.py |
| wi-1069-golden-fix | 1/1 | +1847/−1 | — |
| wi-1070-s5-parity | 2/8 | +2058/−130 | script.json, parity.py, normalise.toml |
| wi-1071-s6-gaps | 7/0 | +1698/−219 | script.json, parity.py, normalise.toml |
| wi-1072-s7-forge-gaps | 5/0 | +254/−124 | script.json |

rust-core re-recorded the same directory twice in a row (`43b227a2`, `cf2d3ca6`).

Every re-record regenerates the whole transcript, because inserting a step shifts step order and sequential ids. So any two branches conflict on everything after their first inserted step. Re-recording on rebase fixes the conflict but quietly drops the other branch's steps whenever it ran against a stale script. The golden set is also fragmenting: `21c95f6-s4`, `21c95f6-s9`, `fe9d4db` (cli only), and `39b89c8`/`b108fa4`/`bd7bbb8` (no mcp).

**Proposed process**
1. **Slice branches don't commit goldens.** A slice commits only its `script.json` steps and its normaliser/driver changes. CI records the Python implementation on the fly and compares against that. A `rust-core.yml` guard fails any non-lane branch whose diff touches `tests/parity/golden/**`.
2. **One serialized re-record lane.** Oversight or a `golden-refresh` job owns it. After each slice lands on rust-core, run `scripts/parity.py record --impl python` on all three transports into `golden/<rust-core-sha>`. Commit that alone as `test(parity): golden @<sha>`. Merges are one at a time, so re-records never race.
3. **Shard the script by surface** (`script.d/<surface>.json`), each shard starting from its own fresh `init`, with goldens per shard. Then two slices only conflict when they touch the same surface. CI checks that a slice's golden diff stays inside its own shard.
4. **Keep one golden set.** Retire `-s4`, `-s9`, `fe9d4db`, `39b89c8`, `b108fa4` and `bd7bbb8` at the next lane re-record. `meta.json` must list cli, http and mcp, or CI fails. CI should also fail on MCP steps without params, which `parity.py:168` silently skips today.
5. **Now:** freeze golden commits on the five branches. Land their `script.json` changes least-behind first: WI-1069 → WI-1072 → WI-1071 → WI-1067 → WI-1070. Re-record in the lane after each merge, or once after all five.

## Per-item classification

Key: rc = origin/rust-core. "golden" = re-records tests/parity/golden/21c95f6.

### WI-51 … WI-968 and GitHub mirrors

| ref | class | evidence / notes | files (ready) |
|---|---|---|---|
| WI-51 | done-but-open | GH #491 closed; PRs #492/#493/#495 | — |
| WI-64 | stale | triage tracker, children mostly closed | — |
| WI-66 | superseded | gh#545 closed 09-08; the decision moved to WI-70/#564 | — |
| WI-73 / WI-74 / WI-84 | done-but-open | PRs #567 / #569 / #593 #594 #597 | — |
| gh#273 | stale tracking | its children are closed | — |
| gh#538 | partial | 17afc0ca (editor drafts); re-scope the rest | web |
| gh#540, #467, #545, #613, #616, #620, #622, #626, #632, #639 | done-but-open | closed on GH 2026-09-08; fixes 00a891cc, b747f1e7, cf69a33b, 16b6774f; Vogt mirrors did not sync | — |
| gh#188 | stale umbrella | voice epic; call mode landed; re-scope | — |
| gh#191 | ready (operator) | FCM client entry; needs Firebase access | android google-services.json |
| gh#583 | duplicate | merge into WI-695 | — |
| WI-121–131 | done-but-open | aeb7f86b, 144e3dd6, b046a06b, 12041472, 82bb8d1c, 3a5a9170, a31ce16f, d9d009b1, 141ea250, 303bd527 | — |
| WI-171 | done-but-open | c57e408f | — |
| WI-177 | done-but-open | PR #683 | — |
| WI-181 | stale (verify) | identity mode is live | — |
| WI-642 / 643 | done-but-open | 7ed38e05 | — |
| WI-695 | ready (operator, ops) | mint FCM key; absorb gh#583 | ops / Infisical |
| WI-698 | done-but-open | 93cb0fc2 | — |
| WI-700 | done-but-open (re-measure) | e6c386ae | — |
| WI-701 | ready (low; re-check logs first) | WS attach auth | engine auth |
| WI-774 | done-but-open | PRs #772, #829 | — |
| WI-799 | ready | deskd write-back gaps | services/forge*.py, adapters/github |
| WI-818 | stale | deploy of an old SHA | — |
| WI-826 | umbrella | check its children, then close | — |
| WI-852 | stale | merge-queue proposal obsolete under the push-branch policy | — |
| WI-859 | ready p4 (ops) | Cloudflare DNS via OpenTofu | ops |
| WI-861 | stale in_progress | no session or commit since 10-04 | — |
| WI-885 | blocked (operator D1–D12) | design 198e3c7d is on main | — |
| WI-886→887→888→889→890 | blocked | wait on WI-885 | — |
| WI-891 | done-but-open | push_api.rs:388 handles Lagged | — |
| WI-893 | blocked (operator); likely stale | — | — |
| WI-894 | conflicts with the "NO caps" ruling → wont_do | — | — |
| WI-895 | needs an operator call (NO caps) | — | — |
| WI-896 / 897 / 899 | ready (ops, node-b) | — | ops / grafana |
| WI-898 | ready (image) | gradle hygiene | pod image |
| WI-939 | ready p1 (security) | rotate the old prod agent token | Infisical |
| WI-940 | ready (ops) | prod /metrics scrape | ops prod compose |
| WI-941 | blocked (operator policy) | — | — |
| WI-952 | done-but-open | themes in styles.css | — |
| WI-954 | ready | integration.rs ~8433 sleep → poll | engine/server/tests/integration.rs |
| WI-960, 965–968 | done-but-open | 55b233f, dfa706ed, 0fb2edf5, 489fdb4f, db3f99fe | — |

### WI-969 … WI-1253

| ref | class | evidence / notes | files (ready) |
|---|---|---|---|
| WI-969 | done-but-open | 01746e16 | — |
| WI-970 | done-but-open (verify) | 55b233fe, 7e1f8ebc, cff4ea3a | — |
| WI-973 | done-but-open (M1) | 91bcf41a, ae28883c; M2/M3 are tracked in WI-979/980; branch wi-973-engine-test-hang is stale | — |
| WI-977 | ready | — | opencode config |
| WI-979 | blocked | waits on WI-982 | — |
| WI-980 | blocked | waits on WI-979 | — |
| WI-982 | ready (L, p1) | separate uid | engine pty.rs, deploy |
| WI-987 | done-but-open | 5475453b; the remainder is WI-990 | — |
| WI-991 | ready | — | web terminal replay |
| WI-992 | ready p3 | cosign 3 | build/release workflows |
| WI-993 | ready (operator decision) | — | .github/dependabot.yml |
| WI-994 | blocked | waits on WI-995 | — |
| WI-995 | stale | fix/wi-995-codeql-ram 10-07, no PR; likely solved by runner placement | — |
| WI-996 | ready (p1, L, design) | body stale vs session_report_blocked / decision.ask / WI-1518 | engine, alertmanager |
| WI-1000 | ready p1 (security) | — | grants in services/sessions |
| WI-1001 | ready | blind Enter can approve a prompt | engine autopilot.rs |
| WI-1003 | ready p3 | L3 linkage | web board, core |
| WI-1007/1008/1009/1012/1098 | superseded by WI-1151 | Klaudia driven-agent gaps | — |
| WI-1030 | duplicate of WI-1007 | grok 400 | — |
| WI-1010 | stale (re-test) | wait_any with cursor (fe64958f) | — |
| WI-1011 | done-but-open | #901 | — |
| WI-1013 | done-but-open | Klaudia pin 3e03a65b | — |
| WI-1014 | stale (re-check) | likely fixed by WI-1094 parity | — |
| WI-1016 | ready p3 | egressy lane | — |
| WI-1017 | partial | 6193cab2; body assumes an MSRV bump (policy is stable 1.99 / floor 1.88) | engine Cargo.toml |
| WI-1018 | duplicate of WI-1013 | — | — |
| WI-1019 | blocked | waits on WI-990 | — |
| WI-1053 | done-but-open | gap audit says DONE; cmt_01M544C4965HEAQ36BDWGVWX93 | — |
| WI-1058 | ready / stalled | wi-1058-application-gaps 5 ahead of rc, no session | core brief.rs, services/sessions.rs, resolve.rs |
| WI-1059 | likely done | 8be93a4b (rc) | — |
| WI-1061 | likely done | 54d16659; HTTP gaps → WI-1742 | — |
| WI-1063 | likely done | c9aab9a2 (rc) | — |
| WI-1064 | in progress, unstaffed | cb976e35 (rc); **critical path** | engine/core adapters |
| WI-1065 | near done | 620d6aae, 0b8f65f2; consolidate waits on WI-1725 | — |
| WI-1066 | likely done | 341b1be0; check the s1-report branch | — |
| WI-1067 | in flight | live session; wi-1067-s2-gap-close | **golden** |
| WI-1068 | likely done | 3d15ee8f; the -v2 branch is stale | — |
| WI-1069 | in flight, no session | wi-1069-golden-fix 1 ahead | **golden** |
| WI-1070 | in flight, no session | wi-1070-s5-parity; 15 ops have no parity step | **golden** |
| WI-1071 | in flight | live session running | **golden** |
| WI-1072 | in flight | live session; review blocker | **golden** |
| WI-1073 | in progress, no session | 4 ops have no parity step | **golden** |
| WI-1074 | ready, unstaffed | 26 rc commits, salvage only; **critical path** | services/sessions*, grants |
| WI-1075 | in progress | S9; waits on 1064/1074/1725 | — |
| WI-1076–1082 | blocked chain | see the Rust-port section | — |
| WI-1079 | in progress, no session | 6ad7ce89 | scripts/ |
| WI-1080 | in flight | live session, CI | tests |
| WI-1093 | stale (verify) | 10-08 estate tooling | workspace |
| WI-1095 | done-but-open or needs a decision | 9cf4799e | — |
| WI-1097 | done-but-open | v0.7.8 #905/#907; ops env owed elsewhere | — |
| WI-1146 | done-but-open | c382cd3a, 573d874a | — |
| WI-1149 | done-but-open | 69693697, d7f70864 | — |
| WI-1150 | ready / stalled | needs a person-run on dev | docs/parity |
| WI-1151 | ready (p3 → p2) | closes 6 items | engine agent_cli.rs |
| WI-1223 | ready p1 (ops) | rotate FORGEJO_TOKEN | — |
| WI-1224 | likely done | e6c386ae, 77361235 (v0.7.10); verify the Caddy half | — |
| WI-1243 | done-but-open | 6bf47108, a3d2df95; the Rust remainder is WI-1058 | — |
| WI-1248 | done-but-open | 70bc3e0a, a49bac80, ba27a5d3 | — |
| WI-1250 | done-but-open | e19fe6c3, 637d6b51, dd6ea745 | — |
| WI-1251 | done-but-open | ca2c69dc | — |
| WI-1253 | ready (p2, S) | — | core events / sweep |

### WI-1254 … WI-1524

| ref | class | evidence / notes | files (ready) |
|---|---|---|---|
| WI-1254 | ready | GUI refresh debounce | web session store / SSE |
| WI-1256 | done-but-open | e1201c76, 3199b57a | — |
| WI-1260 | stale | LiteLLM is already the Codex default (WI-1147); re-scope or close | — |
| WI-1263 | ready p3 | envsubst in runner images | runner Dockerfiles |
| WI-1265 | blocked umbrella | close after WI-1266–1268 | — |
| WI-1269 | done-but-open | 9cf4e91b, 6720984d | — |
| WI-1270 | done-but-open | 4fb96621, c1248915, 89e4c05a | — |
| WI-1273 | ready | ENGINE.md:1599 accept-edits | agent_cli.rs, docs/ENGINE.md |
| WI-1279 | done-but-open | 712b790b | — |
| WI-1280 | stale | review whose output became 1281/1284/1297 | — |
| WI-1281 | done-but-open | 96560109, 0b96dfb2, 923d4a64 | — |
| WI-1284 | ready | — | engine transcripts index, sessions.py |
| WI-1289 | done-but-open | 1986e3a6, 645b2819 | — |
| WI-1291 | ready (ops) | GCP runner TMPDIR | runner config |
| WI-1297 | review | report uncommitted; 6 operator decisions pending | — |
| WI-1298 | done-but-open | 69d744a6, e087db20, 2c6e83d1 | — |
| WI-1299 | done-but-open | 68385649, ccf818f5 | — |
| WI-1302 | done-but-open | 24c5be31 | — |
| WI-1304 | done-but-open | 2c5d3602, 885ccaa0 | — |
| WI-1309/1310/1312/1313 | done-but-open | 1e5d06c3 / eda031ba / 62bf6abb / 58399ccd | — |
| WI-1314 | stalled in flight | wi-1314-auto-unblock-c has 1 commit off main; no session or PR | sessions.rs, autopilot.rs |
| WI-1315 | ready | docs rule landed; CI check not built | .github/workflows |
| WI-1316 | done-but-open | 9d2232cf, bd83b433 | — |
| WI-1317 | in flight | design doc only on wi-1317-harness-design; land it or close | docs/design |
| WI-1318 | done-but-open | 8b822484 | — |
| WI-1319 | ready | verdict comment kinds | core/entities.py, work comments |
| WI-1320 | blocked | waits on WI-1319 | — |
| WI-1321 | done-but-open | 1bbe3771, 6fdb80f6 | — |
| WI-1322 | ready | decision change kind | engine sessions changes |
| WI-1323 | done-but-open | 6f5526e2 | — |
| WI-1324 | ready (unblocks 4 items) | workspace record | engine sessions.rs, new workspace.rs |
| WI-1325/1326/1340 | blocked | wait on WI-1324 (1325 overlaps 1304, so re-scope) | — |
| WI-1327 | ready | push/PR-open rule | engine policy.rs |
| WI-1328 | blocked | waits on WI-1341, WI-1319 | — |
| WI-1329 | ready | dispatch enforcement | services/sessions.py |
| WI-1330 | blocked | waits on WI-1328 | — |
| WI-1331 | ready | askpass token | engine agent-auth broker |
| WI-1333 | ready (docs) | — | docs/ENGINE.md |
| WI-1334 | ready | ci.run_hung | CI collector, inbox.py |
| WI-1335 | blocked | waits on WI-1334 | — |
| WI-1336 | ready p3 (docs) | — | docs |
| WI-1337 | ready (verify; may be done) | superseded_by already in 21 files | sessions.py |
| WI-1338 | ready | local_only auto-chore | sessions.py |
| WI-1339 | ready | inbox row exists; PWA card only | web inbox (demo baselines) |
| WI-1341 | ready | GitOpReq::Land | engine git.rs |
| WI-1342 | ready (operator-gated) | brokered flip | ops |
| WI-1343 | ready | `vogt-agent-auth with` | engine/deploy/agent-auth.sh |
| WI-1344 | blocked | waits on 1342, 1331, 1343 + one week on dev | — |
| WI-1351 | done-but-open (verify) | 80df588b, 115a170f | — |
| WI-1355 | done-but-open | 994b8031, d734c08b, 761e0453 | — |
| WI-1359 | blocked (infra) | pod userns | — |
| WI-1371 | duplicate of WI-1359 | — | — |
| WI-1373 | partial | 0365057f, b108fa4b; re-scope the analysis remainder | — |
| WI-1380 | done-but-open | 319bb466, 56001df2, a15d48c8 | — |
| WI-1381 | partial | 73ad343f; STT/TTS latency metrics remain | voice |
| WI-1411 | done-but-open | 64466141, 04e316a7 | — |
| WI-1412 | done-but-open | b0bffb4d | — |
| WI-1413 | done-but-open | 9f5b5d34, a66205b8, b69f3bde; check wi-1413b (+3) | — |
| WI-1517 | blocked umbrella | children open | — |
| WI-1518 | done-but-open | 7f4a4d11 | — |
| WI-1520 | ready | session_report op | engine sessions.rs, core, MCP parity |
| WI-1521 | blocked | waits on WI-1520 | — |
| WI-1523 | blocked | waits on WI-1520 | — |
| WI-1524 | blocked | waits on WI-1520, WI-1521 | — |

### WI-1525 … WI-1751

| ref | class | evidence / notes | files (ready) |
|---|---|---|---|
| WI-1525 H8 compact sweep | ready* | no commits | services/sessions.py sweep |
| WI-1526 H9 oversight model trial | ready (operator decision) | an experiment | brief/profile config |
| WI-1527 H10 ACP pilot | blocked | waits on WI-1595 codex-acp | engine driver |
| WI-1528 H11 brief cleanup | ready* | — | engine prompt templates |
| WI-1529 H12 tracker hygiene | stale | branch from 10-14, no PR, session dead; this audit overlaps it | — |
| WI-1530 H13 aidevenv docs | ready | lives in the aidevenv repo | aidevenv TOOLING.md |
| WI-1574 | superseded by WI-1634 | b3962fed; per-tool annotations may remain | — |
| WI-1575 | ready* | Codex economics | agent_cli.rs, session_economics.py |
| WI-1576 | ready* | Codex ready/done from rollout | autopilot.rs, pty.rs |
| WI-1577 | blocked | waits on WI-1576 | — |
| WI-1578 | ready* | Codex hibernate/wake | engine hibernate |
| WI-1579 | ready (tiny) | AGENTS.md 32 KiB budget | AGENTS.md |
| WI-1590 | done-but-open | f837017e, 943d4c7d | — |
| WI-1592 | done-but-open (verify on dev) | 8772a3a5 | — |
| WI-1593 | ready* (ops) | bwrap userns | pod securityContext |
| WI-1594 | done-but-open | c3225e4c | — |
| WI-1595 | stalled | PR #1007 closed, not on main; verify | agent-versions.env, Dockerfile |
| WI-1597 | blocked | waits on the WI-1603 fixes being deployed to dev | — |
| WI-1598 | done-but-open | 7f3a8cdf | — |
| WI-1599 | blocked | waits on the WI-1635 deploy to dev | — |
| WI-1602 | blocked | operator grant on dev | — |
| WI-1603 | blocked umbrella | fixes merged (#1012–#1015); re-run the bar on dev | — |
| WI-1612 | likely done | #1013 Klaudia pin; verify | — |
| WI-1621 | duplicate | merge into WI-1631 | — |
| WI-1629 | done for engine (8771d424) | voice/Cargo.lock still has rustls 0.23.45; retarget to voice | voice/Cargo.lock |
| WI-1630 | done-but-open | ed24df39, 5941d8ab | — |
| WI-1631 | ready (ops) | LiteLLM groups | litellm config |
| WI-1632 | done-but-open | e9202450 | — |
| WI-1634 | done-but-open | b3962fed | — |
| WI-1635 | done-but-open (deploy pending) | 194f1719 | — |
| WI-1637 | ready (operator decision) | requirements.toml pin | — |
| WI-1638–1641 | done-but-open | #1012–#1015 | — |
| WI-1644 | done-but-open | #1019 (bd7bbb8e); close once it is on prod | — |
| WI-1645 | done-but-open | #1020 + fe9d4db9 | — |
| WI-1646 | in review | branch design/build-deploy-pipeline-wi1646, no PR; needs operator sign-off | docs/design |
| WI-1648 | blocked / ready | waits on WI-1646 sign-off | vogt-estate |
| WI-1651 | stale in flight | session dead, no branch | vogt-estate |
| WI-1652/1653/1656/1657 | blocked chain | 1651 → 1652 → (1653, 1656) → 1657 | — |
| WI-1654 | blocked | operator: Infisical/OIDC | — |
| WI-1655 | blocked | waits on WI-1654 | — |
| WI-1647 | done-but-open | #1021 | — |
| WI-1658 | ready | CodeQL off bigmem | .github/workflows/codeql.yml |
| WI-1659 | blocked | conditional on WI-1658 → p4 | — |
| WI-1660 | ready | publish-runner trim | .github/workflows/build.yml |
| WI-1661 | stale | 10-15 plan; close | — |
| WI-1663 | ready* | GUI dispatch settings | web settings |
| WI-1664 | done-but-open | #1027 | — |
| WI-1667 | done-but-open | #1034 | — |
| WI-1670 | done-but-open | #1039, #1041 | — |
| WI-1671 | done-but-open (Py, #1036) | Rust remainder is gap audit WP7 | — |
| WI-1672 | done-but-open (Py, #1035) | Rust remainder is WP5/6 | — |
| WI-1680, 1681 | done-but-open | #1046 | — |
| WI-1682 | done-but-open | #1048 | — |
| WI-1687 | done-but-open | #1050 (rc) | — |
| WI-1690 | blocked | waits on I7 + soak | — |
| WI-1725 | partial | 54be0a78 read-only onboard on rc; consolidate is not done; split the item | engine/core forge |
| WI-1736 | done-but-open | #1068 | — |
| WI-1742 | done-but-open | #1070 (rc) | — |
| WI-1744 | ready | in-process load driver | engine/server/tests/load.rs, scripts/load.py |
| WI-1745 | blocked | waits on service ports (WI-1725 and others) | — |
| WI-1746 | done-but-open | #1071 | — |
| WI-1747 | ready | front-door audit rows | engine/server front-door tests |
| WI-1748 | done-but-open | #1072 (rc) | — |
| WI-1749 | in flight | live session, gates running; branch wi-1749-schemas-from-models; retires the recorded schema table | registry, models.rs |
| WI-1751 | in flight | live session running; gates I3 | engine/core CLI |

\* no commit or PR matched by WI ref; I did not read the body.
