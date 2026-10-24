# Approved actions: engine-executed infrastructure steps (WI-2331)

Status: **design only**. Nothing here is implemented. Where this note and
[`ENGINE.md`](../ENGINE.md) or [`API.md`](../API.md) disagree, those describe
what exists. This note applies the [approved-grants](oversight-grants.md)
pattern (WI-973) to *actions* instead of *credentials*: a person approves one
exact operation, and the engine carries it out.

## Problem

A driven-session worker often reaches a step the operator has already agreed
to: apply a Tailscale ACL change, deploy a Komodo stack, merge a Forgejo pull
request in `indexarr/ops`. The worker can't do it:

- The driven-session classifier (`engine/deploy/driven-session-settings.json`)
  treats Komodo writes, Tailscale policy and Forgejo merges outside the
  "Own Green PR Merge" rule as protected-scope changes, and refuses them. That
  refusal is correct. Nothing in the transcript proves a person approved the
  step, so the classifier can't tell an approved apply from an unapproved one.
- The approval lives in chat. Oversight then runs the step itself with broad
  credentials, such as the operator's personal Forgejo token. The worker that
  holds the context stops, and the action is audited only as "oversight did
  something".
- A credential grant (WI-973) doesn't fit. It hands the worker a secret that
  can do far more than the one approved step, and the classifier still refuses
  the protected use of that secret.

Nothing in the system can express "approved: this exact action, with these
parameters, for this work item, once".

## The shape

```text
worker (driven)            core                         approver              engine
  | vogt-agent-auth request ---- broker token --------------------------------->|
  |                        |<-- POST /api/agent-auth/action-requests (engine relays, adds session) --|
  |                        | action_requests row (pending), audited            |
  |                        |-- Inbox entry ----------->|                        |
  | session_report_blocked |                           |                        |
  |                        |<-- action.decide (approve) |                       |
  |                        |-- POST /api/actions/{id}/execute (stack secret) -->|
  |                        |                           |   preconditions        |
  |                        |                           |   execute (engine cred)|
  |                        |                           |   verify / rollback    |
  |                        |<------------------------ outcome ------------------|
  |                        | row: succeeded | failed | rolled_back, audited     |
  |<-- wake + outcome ----------------------------------------------------------|
```

The split is the same as for grants. The core owns the **decision**: who
asked, the exact parameters, who approved, the item, and the audit trail. The
engine owns the **effect**: it holds the action credentials, runs the
executor, and is the only process that touches the target system. The worker
never holds a credential that can perform the action.

## 1. Data model

### 1.1 Action types (code, not data)

An action type is a Rust module in the engine, registered under a stable dotted
name. It's code, not configuration, because its preconditions, verification
and rollback are code. Each type declares:

| Field | Meaning |
| --- | --- |
| `name` | `forgejo.merge`, `komodo.deploy`, `tailscale.acl.apply`, `tailscale.device.tag`. |
| `params_schema` | A JSON schema. Every field is required or has a stated default, `additionalProperties: false`. Free text is allowed only in `reason`. |
| `risk` | `standard` or `person_only`. A `person_only` type can never be approved by a delegation (§1.4). Every `tailscale.*` type is `person_only`, and so is any request whose target matches the deployment's prod pattern. |
| `credential` | The engine-held credential the executor uses, by name (§3.5). |
| `preconditions(params)` | Read-only checks the engine runs at execute time (§3.1). |
| `execute(params)` | The one change. |
| `verify(params, before)` | A read that proves the change took effect (§3.2). |
| `rollback(params, before)` | Puts the target back to the captured `before` state, or `none` with a stated reason (§3.3). |
| `deadman_seconds` | How long the change may stand unconfirmed before the engine rolls it back automatically. `0` means no dead-man. |

`GET /api/actions/types` lists the registered types with their schemas, risk
and whether this deployment has their credential configured. A type without a
credential is listed as `unavailable`. A request for it is refused when it is
made, not when it is approved.

### 1.2 Action requests

Core table `action_requests`, in the next free migration:

| Column | Meaning |
| --- | --- |
| `id` | `act_…` ULID. |
| `action` | The type name. |
| `params` | Canonical JSON (keys sorted, no whitespace), validated against the type's schema at request time. |
| `params_digest` | `sha256(action ‖ 0x00 ‖ params)`. The approval binds to this digest (§6.2). |
| `work_item` | `WI-n`. Required. The item must exist and be open. |
| `requester_session` | The `ses_…` id, taken from the engine relay, never from the body. |
| `requester_principal` | The authenticated identity_ref of the relay. |
| `reason` | Shown to the approver verbatim as untrusted text. ≤ 2000 characters. |
| `state` | `pending` → `approved` → `executing` → `succeeded` \| `failed` \| `rolled_back`; or `denied`, `withdrawn`, `expired`. |
| `approver`, `approval_kind` | A person's identity_ref and `person`, or an oversight session and `delegation:<id>`. |
| `decided_at`, `expires_at` | A pending request expires after `ttl_seconds` (default 86 400, at most 604 800). An approval must be executed within 15 minutes, or it expires as well. |
| `outcome` | The executor's structured result: precondition results, `before` snapshot digest, the change's id (merge SHA, Komodo update id, ACL ETag), verify result, rollback result. No secret, ever. |

A request is **single-use**. `approved` moves to `executing` in one
conditional update (`WHERE state = 'approved'`), so a second execute call finds
nothing to run.

### 1.3 Approvals

An approval is the decision on one request, recorded on the row: who, when,
by which path (person, or a named delegation), and the `params_digest` the
approver saw. A person approves in the Inbox, which shows the exact
parameters. They are rendered from the stored canonical JSON, not from the
reason. There is no "approve with edits". To change a parameter, the person
denies and the worker requests again.

### 1.4 Standing delegations

Core table `action_delegations`, managed only by a person (`action.delegation_set`
refuses every agent principal):

| Column | Meaning |
| --- | --- |
| `id` | `dlg_…`. |
| `project` | The Vogt project the delegation applies to. A request is in scope when its `work_item` belongs to this project. |
| `actions` | The action types oversight may approve, each `standard` risk. Naming a `person_only` type is refused. |
| `constraints` | Per type, a narrowing of the parameters, e.g. `forgejo.merge: {repo: ["indexarr/ops"], style: ["rebase"]}`, `komodo.deploy: {stack: ["vogt-dev"]}`. A constraint can only narrow the schema. |
| `approvers` | Which oversight sessions may approve: `any_oversight` (the WI-957 role, which only a person can assign) or listed `ses_…` ids. |
| `max_per_day` | A rate cap on delegated approvals. Default 20. |
| `expires_at` | At most 30 days. No delegation is permanent. |
| `created_by`, `reason` | The person who set it, and why. |

An oversight session approves a request through a delegation with
`action.decide` like a person would. The core checks four things: the approver
is an oversight session listed by the delegation, the request is in the
delegation's project, its type and parameters are within the constraints, and
the type is not `person_only`. It also refuses three cases:

- **Self-approval.** The approver is the requester.
- **Approving the approver's own work.** The requester was started by the
  approving oversight session for this item, and the delegation does not set
  `allow_own_workers: true`. The default is `true`, because "oversight
  approves its workers' steps" is the use case. The flag exists so a project
  can require a *different* overseer.
- **The prod pattern.** Any target matching the deployment's prod pattern
  (`*prod*`, the same rule the Komodo MCP uses) is person-only, whatever the
  delegation says.

## 2. CLI and API surface

### 2.1 Session side: `vogt-agent-auth`

```text
vogt-agent-auth actions                             # list types, schemas, availability
vogt-agent-auth request <action> --item WI-n \
    [--param key=value ...] [--params-file f.json] \
    --reason "…"                                    # prints act_… and the params digest
vogt-agent-auth request-status <act_…>              # state and outcome
vogt-agent-auth request-withdraw <act_…>            # requester only, while pending
```

`request` calls `POST /api/agent-auth/action-requests` on loopback
(`VOGT_ENGINE_BROKER_URL`) with the session's broker token, which is the same
token `fetch` and `store` use. The engine validates the parameters against the
type's schema, adds the session's identity from the token, and relays the
request to the core with its own identity. The worker then calls
`session_report_blocked` naming the `act_…` id and stops. The engine wakes it
when the request reaches a final state (§3.4).

`request` never takes a credential, never prints one, and never runs the
action. It's safe to allow in the classifier (§5).

### 2.2 Core operations (registry → CLI, REST, MCP)

| Operation | Scope | Who |
| --- | --- | --- |
| `action.request` | `work.write` | Only through the engine relay. A direct agent call is refused (`relay_required`), so the requester session is always the engine's word. A person may also request from the GUI or CLI. |
| `action.decide` (`approve` \| `deny`, `params_digest`, `reason`) | `work.write` | A person, or an oversight session within a delegation (§1.4). Every other agent principal is refused with `403 person_required`. |
| `action.withdraw` | `work.write` | The requester, or a person. |
| `action.list` / `action.get` | `read` | Filters: `state`, `work_item`, `action`, `requester`. |
| `action.delegation_set` / `_revoke` / `_list` | `admin` | A person only. |

`action.decide` must carry the `params_digest` the approver saw. If it doesn't
match the row, the call is refused (`params_changed`).

### 2.3 Engine routes

| Route | Caller | Purpose |
| --- | --- | --- |
| `GET /api/actions/types` | any authenticated caller | The registry (§1.1). |
| `POST /api/agent-auth/action-requests` | a session broker token | Validate and relay a request (§2.1). Rate-limited per session: 10 an hour. |
| `GET /api/agent-auth/action-requests/{id}` | a session broker token | The requester's own requests only. |
| `POST /api/actions/{id}/execute` `{action, params, params_digest, approver, approval_kind}` | **the stack secret only** (`vogt-core`) | Run the executor. The engine recomputes the digest from the body and refuses a mismatch. |
| `POST /api/actions/{id}/confirm` | the stack secret | Disarm a dead-man (§3.3). |

The engine half of "it was approved" is the same as for grants. Only the
core's stack-secret identity can make the engine execute, and the core sends
an execute request only after recording an approval.

## 3. Executor contract

Every executor runs in this order. Each step writes a `vogt::audit` line
(`event=action`, with `phase=`).

### 3.1 Preconditions

These are read-only checks, run with the engine credential at execute time,
not at request time. Things change between the request and the approval, so
the checks must run against the state at execution. Each type lists its own
checks (§4). Every type also checks two things: the work item is still open,
and the request is still `executing`. If any check fails, the request ends
`failed`, with the failing check named, and the target is untouched.

### 3.2 Verify

After `execute`, the engine reads the target back and checks that the
intended state now holds: a merge commit is on the base branch, a container is
healthy at the new image, an ACL ETag matches the applied policy. If
verification fails, the engine runs `rollback`.

### 3.3 Rollback and the dead-man

`execute` is preceded by a **before snapshot**, stored with the outcome by
digest and kept by the engine for rollback. A type's rollback restores that
snapshot. Where a rollback isn't possible (a merge is public history), the type
says so and its preconditions must carry more of the weight.

A type with `deadman_seconds > 0` arms a timer once `verify` passes. If no
`confirm` arrives before the timer runs out, the engine rolls the change back
and the request ends `rolled_back`. The confirm comes from the core, on behalf
of a person or the requester's follow-up check. The dead-man exists for changes
that can cut off the path used to confirm them. The main case is a Tailscale
ACL that locks the tailnet out of the engine. The timer runs inside the engine
and doesn't need the network path the change might break. An engine restart
while the timer is armed rolls the change back at startup (fail closed). For
that, the snapshot is persisted under the engine's state directory, mode 0600.

### 3.4 Audit and wake

When the request reaches a final state, the engine returns the outcome to the
core. The core records it on the row and emits `action.completed`, carrying
the requester, approver, approval kind, action, params digest, item, outcome
and change id. It posts a `work_comment` on the item (kind `action`), and wakes
the requester with `session_input`, a single line: `ACTION act_… <state>: <summary>`.
The engine audits the same facts on `vogt::audit`. Neither side ever records a
credential.

### 3.5 Engine-held credentials

The engine resolves an action's credential from the stack's agent-auth helper
(`vogt-agent-auth get VAR`) with a manifest entry flagged **`action`**, a new
flag beside `optional`, `ondemand` and `writable`. An entry flagged `action` is
**never** exported to a session, and it is never fetchable through
`/api/agent-auth/fetch` or a grant. The engine's broker refuses it the way it
refuses an undeclared name. The executor resolves the credential when it runs,
holds it in memory for that one call, and drops it afterwards.

Suggested manifest lines (the ops overlay owns the real ones):

```text
ACTION_FORGEJO_TOKEN    <cicd project>  FORGEJO_ACTIONS_BOT_TOKEN  action
ACTION_KOMODO_KEY       <infra project> KOMODO_ACTIONS_API_KEY     action
ACTION_KOMODO_SECRET    <infra project> KOMODO_ACTIONS_API_SECRET  action
ACTION_TAILSCALE_OAUTH  <infra project> TAILSCALE_ACL_OAUTH_CLIENT action
```

Each is a **dedicated bot credential** with only the rights its executor needs:
a Forgejo bot user with write on the named repos, a Komodo API key limited to
deploy on non-prod stacks where Komodo allows it, and a Tailscale OAuth client
with the `policy_file` and `devices` scopes. None is the operator's personal
token.

## 4. The first action types

### 4.1 `forgejo.merge`

| | |
| --- | --- |
| Params | `repo` (`owner/name`, matched against `ENGINE_ACTION_FORGEJO_REPOS`), `pr` (integer), `head_sha` (40 hex), `style` (`rebase` \| `squash` \| `merge`, default `rebase`), `delete_branch` (bool, default `false`) |
| Risk | `standard` |
| Preconditions | The PR is open. Its head is exactly `head_sha`, so a push after approval voids it. It is mergeable, with no conflict. Every required status check on the base branch's protection is `success` at `head_sha`. The base branch is the repo default or one named in the protection rule. |
| Execute | `POST /repos/{repo}/pulls/{pr}/merge` with `Do = style` and `head_commit_id = head_sha`. **Never `force_merge`**: the executor has no code path that sets it. |
| Verify | The PR reports `merged`, and the base branch now contains `head_sha`'s tree (for `rebase`, the base tip's tree equals the merged result's). The merge commit SHA is recorded. |
| Rollback | `none`. A merge is public history. A follow-up revert is a separate, newly requested change. This is why the precondition pins `head_sha`. |
| Dead-man | `0` |

This replaces oversight's personal-token merges in `indexarr/ops` (rollout
phase 3). Merging `indexarr/ops` main is not a deploy, so this type stays
`standard`.

### 4.2 `komodo.deploy`

| | |
| --- | --- |
| Params | `stack` (a Komodo stack name), `expected_commit` (optional: the ops repo SHA the stack should be at after `PullStack`), `services` (optional list) |
| Risk | `standard`, and `person_only` when `stack` matches the prod pattern. Prod also requires `ENGINE_ACTION_KOMODO_ALLOW_PROD=1`, which is off by default, so a deployment opts in to engine-run prod deploys at all. |
| Preconditions | The stack exists and is not mid-deploy. If `expected_commit` is set, the stack's repo resolves to it after a refresh. The stack's `env_diff` against the last successful deploy is recorded in the outcome as names and fingerprints only, never values. |
| Before snapshot | The current image digests per service and the last successful deploy's update id. |
| Execute | `DeployStack` (with `services` if given). |
| Verify | Every service is running and healthy within `verify_timeout` (default 300 s), at a digest that is not the before digest unless nothing changed. |
| Rollback | Redeploy the before snapshot's digests (pinned), then verify again. A failed rollback ends `failed`, raises an Inbox entry of severity `high`, and leaves the stack as found. |
| Dead-man | `0` by default. `verify` covers health. A stack may set a dead-man in its delegation constraint where an external smoke test must confirm. |

### 4.3 `tailscale.acl.apply`

| | |
| --- | --- |
| Params | `policy_sha256` (the digest of the exact HuJSON to apply), `source` (`{repo, path, commit}` in the ops repo, which the engine reads itself), `expected_etag` (the live policy's ETag when the request was made) |
| Risk | `person_only`, always |
| Preconditions | The engine fetches `source` at `commit` and checks that its SHA-256 is `policy_sha256`. The live policy's ETag is still `expected_etag`, so nobody changed it since. `POST /api/v2/tailnet/-/acl/validate` accepts it. The policy's own `tests` block passes. A built-in **lockout check** confirms the engine's own node can still reach the Vogt front door and the Komodo core under the new policy, through a `tests` entry the executor adds if it is missing. |
| Before snapshot | The live policy body and ETag. |
| Execute | `POST /api/v2/tailnet/-/acl` with `If-Match: expected_etag`. |
| Verify | The live ETag changed, and the body's digest equals `policy_sha256`. |
| Rollback | Re-post the before body with `If-Match` on the new ETag. |
| Dead-man | **600 s.** The person who approved, or the requester's check over the tailnet, must `confirm` within ten minutes. Otherwise the engine restores the old policy. This is the guard against the change cutting off the path the confirmation would travel. |

The Inbox entry for an ACL apply shows the **diff** between the live policy
and the proposed one, rendered by the engine from the two bodies. A digest
alone isn't reviewable.

### 4.4 `tailscale.device.tag`

| | |
| --- | --- |
| Params | `device` (stable node id, not hostname), `add` (tags), `remove` (tags), `expected_tags` (current set) |
| Risk | `person_only`, always |
| Preconditions | The device exists, and its current tags equal `expected_tags`. Every tag in `add` is defined in `tagOwners`. The change doesn't remove the last tag from a device that has no user owner, which would orphan the node. |
| Execute | `POST /api/v2/device/{id}/tags` with the resulting set. |
| Verify | The device's tags read back as the resulting set. |
| Rollback | Re-post `expected_tags`. |
| Dead-man | 600 s, as for `acl.apply`, because a tag change moves the node across ACL rules. |

### 4.5 Later types

`github.merge` follows `forgejo.merge`, with a GitHub App installation token
and the repository's required checks or merge queue. `infisical.set` overlaps
with the existing `writable` store broker. It is worth adding only for secrets
whose *value* the engine generates itself (rotation), so no session ever sees
it.

## 5. The classifier allow rule

The rule goes in `engine/deploy/driven-session-settings.json` under
`autoMode.allow`, beside "Approved Vogt Grant". It also adds `Bash(vogt-agent-auth request *)`,
`Bash(vogt-agent-auth request-status *)` and `Bash(vogt-agent-auth actions)` to
`permissions.allow`.

> **Approved Action Request**: running `vogt-agent-auth request <action> --item WI-<n> …`,
> `vogt-agent-auth request-status <id>`, `vogt-agent-auth request-withdraw <id>`
> or `vogt-agent-auth actions`. These commands ask the Vogt engine to carry out
> a named infrastructure action after a person, or a delegation a person set,
> approves it. They never perform the action, hold a credential, or change
> anything outside Vogt's request record. The engine runs the action only after
> that approval, with its own credential. This clears Modify Shared Resources,
> Production Changes and Merge Without Review for **making the request**
> alone. It never covers performing the action by any other means (Komodo,
> Tailscale, Forgejo or GitHub API calls, `curl`, the `komodo`, `tailscale` or
> `gh` CLIs), retrying an action the engine refused or failed by hand,
> fetching or using an action credential, or treating a request's approval as
> permission for anything other than the engine executing it. A denied,
> failed or rolled-back request is reported with `session_report_blocked`,
> not worked around.

The protected-scope defaults stay as they are for any action done by hand.

**Oversight sessions keep bypass**, as the operator decided. This design
doesn't change their posture. Oversight *should* use the executors for an
infrastructure change where one exists, so the change carries a request, an
approval and an outcome record. A Vogt charter can say so. Nothing enforces
it.

## 6. Security analysis

### 6.1 Request spoofing

*Threat:* a request claims to come from a different session or item.
*Mitigation:* the core accepts `action.request` only through the engine relay
(`relay_required`). The relay takes the requester from the broker token, which
the engine minted for that one session and revokes when the session is
forgotten. It is never a body field. A person's request comes from the GUI
under their own principal. The work item must exist and be open. *Test:* a
session token calling `action.request` directly is refused, and a body
`requester` field is rejected by the schema.

### 6.2 Parameter binding

*Threat:* a person approves one thing and something else runs. This covers
parameters edited after approval, a PR head moved, a different policy file,
or a reason that misdescribes the change.
*Mitigation:* the parameters are canonical JSON, validated and frozen at
request time. The approval carries the `params_digest` the approver saw, and
the core refuses a mismatch. The engine recomputes the digest from the execute
body and refuses a mismatch again. Every mutable target is pinned by content
inside the parameters: `head_sha` for merges, `policy_sha256` and
`expected_etag` for ACLs, `expected_tags` for devices, optionally
`expected_commit` for deploys. The preconditions re-check those pins at
execute time. The Inbox renders the parameters (and, for ACLs, the diff), not
the reason. The reason is shown as untrusted text.

### 6.3 Replay

*Threat:* one approval is used twice, or an old approval is used late.
*Mitigation:* a request is single-use. `approved → executing` is a conditional
update, and the engine keeps the ids of executed requests and refuses a second
execute for the same `act_…` id (`409 already_executed`). An approval expires
if not executed within 15 minutes. Pins like `expected_etag` and `head_sha`
make a stale replay fail its preconditions even if everything else were
bypassed.

### 6.4 Approver identity

*Threat:* an agent approves its own request, or launders an approval through
another agent.
*Mitigation:* `action.decide` refuses every agent principal unless it is an
oversight session acting within an active delegation that a person set
(§1.4). The oversight role can only be assigned by a person (WI-957,
`role_refused`). Self-approval is refused. `person_only` types and prod targets
can't be delegated. Delegations expire, are rate-capped, and are listed with
their use counts. Every approval records `approval_kind`, so a reader can tell
a person's approval from a delegated one.

### 6.5 The engine as executor

*Threat:* something other than the core makes the engine execute.
*Mitigation:* the execute and confirm routes accept only the stack secret,
checked by the credential compared, as for grants. Action credentials carry
the `action` manifest flag and are never exported or fetchable.
**The same caveat as grants applies (oversight-grants invariants 2 and 10):**
sessions run as the engine's uid, and this deployment runs `identity` mode.
A session that reads `/proc/1/environ` or the core token file holds the stack
secret. In identity mode a session can read the bot credentials from the
vault directly. Approved actions therefore give the operator a **governed,
audited path the classifier can clear**, not a hard boundary, until sessions
run as a separate uid (WI-982) and the deployment runs brokered. The design
doesn't change when that lands.

### 6.6 Executor faults

*Threat:* an executor bug or a half-applied change.
*Mitigation:* the before snapshot, verify, rollback, the dead-man for
lockout-capable changes, and an Inbox entry of severity `high` on a failed
rollback. Each executor gets an integration test against a fake of its API,
including the refusal paths. `force_merge` and `--admin` have no code path.

### 6.7 Denial of service through the Inbox

*Threat:* a worker floods the Inbox with requests.
*Mitigation:* a per-session rate limit of 10 an hour at the relay, at most 3
pending requests per session, and the parameters are schema-validated before
any row is written.

## 7. Phased rollout (operator accepted 1–4)

| Phase | Content | Layers |
| --- | --- | --- |
| **1. Engine** | The action-type registry and `GET /api/actions/types`. The request relay. The `action_requests` table and migration. `action.request` / `_decide` (person only) / `_withdraw` / `_list` / `_get`. Inbox entry with parameters and Approve/Deny. Stack-secret `execute` and `confirm`. The `action` manifest flag. Executors for `forgejo.merge`, `komodo.deploy` (non-prod), `tailscale.acl.apply` and `tailscale.device.tag`. `vogt-agent-auth actions\|request\|request-status\|request-withdraw`. Wake on completion. | core, engine, PWA, deploy |
| **2. Policy** | `action_delegations` and `action.delegation_*` (person only). Delegated approval by oversight. The "Approved Action Request" classifier rule and the `permissions.allow` lines in the driven-session settings. Ops overlay: bot credentials and `action` manifest lines. | core, deploy, ops |
| **3. Retire personal-token merges** | Oversight's `indexarr/ops` merges move to `forgejo.merge` under a delegation for `indexarr/ops`, `style: rebase`. The operator's personal Forgejo token is removed from oversight's reach. | ops, process |
| **4. Expand** | `github.merge`, `infisical.set` (engine-generated values only), the prod `komodo.deploy` opt-in, and further types on demand. Each new type is a code change with its own review. | engine |

The classifier rule ships in phase 2, not phase 1. Phase 1 is exercised by
people and oversight sessions, which have bypass, so the executors are proven
on dev before driven workers can reach them.

## 8. Decisions for the operator

1. **Engine-run prod deploys.** Should `komodo.deploy` ever run against prod
   stacks, as person-only with `ENGINE_ACTION_KOMODO_ALLOW_PROD=1`? Or does
   prod stay on the Woodpecker `vogt-estate` lanes with no engine path? The
   recommendation is to keep prod out until phase 4.
2. **Bot credentials.** Phase 2 needs a Forgejo bot user, a scoped Komodo API
   key and a Tailscale OAuth client minted and stored in Infisical. This is
   operator work in the ops overlay.
3. **`allow_own_workers` default.** Should oversight be able to approve
   requests from workers it started itself (recommended: yes, since that is the
   use case), or should a *different* overseer be required by default?
4. **Identity mode.** As with grants (oversight-grants question 6), the
   guarantee here is real only in brokered mode with a separate session uid.
   Does adopting approved actions change the case for leaving identity mode?
