# Approved grants to a session (WI-973)

Status: **design, with milestone 1 implemented**: named-credential grants,
from request to Inbox approval to engine apply and broker fetch. Milestones 2
and 3 are proposals. Where this note and [`ENGINE.md`](../ENGINE.md) or
[`API.md`](../API.md) disagree, those describe what exists. This note
generalises slice 6a of [oversight sessions](oversight-sessions.md#8-the-permission-gap-slice-6)
("pending bypass starts approved in the Inbox") from *bypass at start* to
*any one scoped grant to a live session*.

## Problem

A session gets its credentials and its permission posture when it starts,
and nothing can change either afterwards:

- **Credentials** come from the stack's `ENGINE_AGENT_AUTH_SECRETS` manifest
  (ENGINE.md §9). A secret that is not in the manifest is unreachable from a
  brokered session. Adding a line is a stack-environment edit, and it takes
  effect only on a redeploy, which cycles the pod and kills every live session.
- **Posture** (`permission_mode`) is fixed at launch. `bypass` can be granted
  only by a person (WI-926), at start or with shift+tab at the terminal.
- In **identity** mode the session holds the machine identity and could
  `infisical secrets get` anything. But the driven-session permission policy
  (`engine/deploy/driven-session-settings.json`, Claude Code's `autoMode`
  defaults) denies credential materialisation, which is a correct guardrail.

On 2026-10-07 (WI-970) the call-mode worker needed the Windows emulator's
SSH key, `100.109.218.11_SSH`. The overseer could not get it to the worker.
The only paths were a manifest line plus a redeploy (which was held behind a
prod rollout), or a person switching the worker into bypass by hand. The
overseer must not be able to grant this itself. That would be permission
laundering: an agent widening another agent's power, or its own, with no
person deciding.

## The shape

```text
overseer (agent)            core                     person (Inbox)          engine
     | session_grant_request  |                           |                       |
     |----------------------->| pending row, audited      |                       |
     |                        |-- Inbox entry ----------->|                       |
     |                        |<---- session_grant_decide (approve, a person) ----|
     |                        |-- POST /api/sessions/{id}/grants (stack secret) ->|
     |                        |<----------------------------------- 200 applied ---|
     |                        | approved row, audited     |                       |
worker (target)               |                           |                       |
     | vogt-agent-auth fetch VAR ------------------- broker token ---------------->|
     |<------------------------------------ one value; audited by grant, never value
```

The core owns the **decision**: who asked, who approved, why, until when, and
the audit trail. The engine owns the **effect**: it holds the machine
identity, it knows the live session, and it is the only process that ever sees
a secret value. Neither can do the other's half. The core never sees a value,
and the engine accepts a grant only from the core's own identity.

## Request

MCP `session_grant_request`, operation `session.grant_request`, scope
`work.write`:

| Field | Meaning |
| --- | --- |
| `target` | The session the grant is for: a `ses_…` id or an engine UUID. |
| `kind` | `credential` (milestone 1) or `capability` (milestone 2). |
| `secret_name` | For a credential: the secret's name in the secrets manager, e.g. `100.109.218.11_SSH`. |
| `project_id` | For a credential: the secrets-manager project that holds it. Cadastre `lookup` answers which. |
| `var` | Optional. The name the session fetches it under. Defaults to the secret name made into an environment-variable name, e.g. `GRANT_100_109_218_11_SSH`. |
| `capability` | For a capability: `bypass`, `accept-edits`, … (milestone 2). |
| `uses` | `once` (the first successful fetch consumes the grant) or `ttl` (any number of fetches until it expires). The default is `once`. |
| `ttl_seconds` | Lifetime once approved. Default 3600, at most 86 400. |
| `reason` | Why. Shown to the approver verbatim, as untrusted text. |

The result is the grant row in state `pending`. The requester is the
authenticated principal. It is never a parameter.

**Who may request.** A session may request a grant for itself. Requesting for
*another* session requires the requester to be an oversight session (the
WI-957 role, read from the engine). A person may request for any session.
Every request still needs a person's approval, so this rule does not protect
the boundary. Its job is to keep the Inbox about overseers and their workers,
not any agent asking for anything. For the rule to mean that, the role has
to be a person's nomination: `session.set_role` refuses agent principals
(`role_refused`), so a session cannot make itself an overseer first.
Overseers are started as one (`session.start` with `role`) or nominated
from the GUI.

## Decision

The Inbox shows each pending grant as an `agent`-source entry of kind
`session.grant_request`, with **Approve** and **Deny**. The title names the
item and the target session *as the person knows it* (its title; the summary
and `evidence_snapshot` add its role, agent CLI, permission mode, project and
`ses_…` id, read from the engine and the core, never from the request). The
summary says who is asking, and whether the requester is the target itself
("for itself") or another session — an overseer asking for a credential
*for itself* while the reason talks about its worker is the thing the
approver must be able to see. The requester is the entry's actor. The
action is `{kind: "grant", grant_id}`.

`session.grant_decide` (`decision`: `approve` | `deny`, plus a reason):

- **Refused for any agent principal** (`principal.kind == "agent"`). That
  includes the brokered pod token, which is an agent's since WI-926, and every
  session token. Only a person decides. The error says so and names the GUI.
- **Approve** applies the grant at the engine *first*, and records `approved`
  with `expires_at = now + ttl` only once the engine has said yes. This
  follows the repository rule: resolve the external dependency, then write. If
  the engine refuses (the session is gone, the project is not grantable, no
  broker), the row stays `pending`, the decision fails with the engine's
  reason, and the person can deny it instead. If the *record* fails after
  the engine said yes — any failure, not only a conflict — the grant is
  taken back from the engine, so the engine never holds a grant the record
  does not stand behind; the one exception is another person's approval of
  the same grant having landed first, which stands (the engine holds the
  same `grant_id`). The approved `reason` travels to the engine with the
  grant, so the session sees what it was approved for.
- **Deny** records `denied`. Nothing reaches the engine.
- **Approve-once vs standing.** `uses: once` is the approve-once case. A
  `ttl` grant stands until it expires or is revoked. A standing *rule* that
  auto-approves future requests is the "standing delegation" of
  oversight-sessions slice 6b. It is out of scope here and needs its own
  operator decision.

## How the engine applies a grant

### A named credential (milestone 1)

`POST /api/sessions/{id}/grants` `{grant_id, var, project_id, secret_name,
uses, expires_at}`:

- **Only the stack secret may call it** (the `vogt-core` identity). A
  break-glass token, a person's token and every session token are refused
  with 403, even though they hold `sessions`. This is the engine half of "a
  person approved it": the only way in is through the core's decision.
- It refuses a session it does not know (404) and one with no broker grant
  (409: brokering is not configured, so nothing can reach the session).
- It refuses a `var` that names a manifest entry (409), so a grant can never
  shadow what the deployment declared; and a *different* grant for a `var`
  the session can already fetch (409), so one approval never silently ends
  another. The same `grant_id` sent again replaces the earlier copy, which
  is what a retried approval is.
- It refuses a `project_id` that is not grantable (403). Grantable projects
  are those the manifest already names, plus `ENGINE_AGENT_GRANT_PROJECTS`
  (space- or comma-separated project ids). An approval cannot reach into a
  project the operator never opened to sessions.
- It refuses an `expires_at` in the past or more than 24 h ahead (400).
- Otherwise it records the grant in memory against that one session and
  answers with the grant as the engine holds it. Nothing is fetched yet, and
  no value exists anywhere.

The session fetches it with the command it already has: `vogt-agent-auth
fetch VAR`. The broker's fetch route (`/api/agent-auth/fetch/{var}`, the
session's own broker token) checks the manifest first. For a name that is not
in the manifest, it looks for an active grant for *that* session and *that*
`var`. If one exists, the engine runs the helper's `get VAR` with
`ENGINE_AGENT_AUTH_SECRETS` replaced by exactly the granted line (`VAR
PROJECT_ID SECRET_NAME ondemand`). The helper's own manifest check stays
meaningful (it sees the effective manifest for this one call) and needs no
change. A `once` grant is spent by its first fetch, successful or not: it is
taken out of the table before the helper runs and never put back, so a
revoke or a session exit that lands while the helper runs cannot be undone
by the fetch failing. Each fetch is audited with the grant id.

`vogt-agent-auth grants` (session side, `GET /api/agent-auth/grants` with the
broker token) lists the session's active grants: `var`, secret name, `uses`,
expiry, grant id and the `reason` it was approved for. Values are never
listed. This lets the agent see what it was granted and for what. It also
lets Claude Code's permission classifier see it: the driven-session policy
gains an allow rule, **Approved Vogt Grant**, which clears fetching a
credential a `vogt-agent-auth grants` command *run in the session* lists, and
using it only for what that line's `reason=` says. The classifier cannot
verify a listing (an agent could quote one), which is why the rule is worded
around a command actually run and why the engine, not the rule, is the gate:
the rule can clear no VAR the engine will not serve. Everything else about
credential materialisation stays denied by the defaults. *Unverified:* how
the classifier weighs that rule against the default. Check it on dev with a
real grant before relying on it. `GET /api/sessions/{id}/grants` is an
operator's read (stack secret, break-glass token or `admin`), and
`session.grant_list` shows an agent only its own session's grants.

### A capability (milestone 2)

Claude Code cannot have its permission mode raised from outside the process,
and typing shift+tab into the TUI would be fragile, unaudited and reversible
by the agent. The engine applies a capability grant by **hibernating the
target and waking it** with the granted `permission_mode`. The conversation
is resumed (WI-912), the session keeps its id, and the restart is that one
session's process only, not the pod. The record keeps the base posture. At
expiry or revocation the engine does the same in reverse and returns the
session to its base posture. A session mid-turn is not restarted: the grant
waits (state `approved`, `applied: false`) until the session is idle, or until
the person cancels it.

## Revocation and expiry

- **Expiry** is enforced by the engine on every fetch: an expired grant is
  never honoured. It is dropped from memory when found expired, and by a
  periodic sweep. The core reports `expired` for an approved row past
  `expires_at`, computed on read, so no timer is needed to keep it honest.
- **Revoke**: `session.grant_revoke`, by a person or by the session that
  requested the grant. Narrowing is always safe, so the requester may give a
  grant up early. The core calls `DELETE /api/sessions/{id}/grants/{grant_id}`
  (stack secret only) and records `revoked`. If the engine has already lost
  the grant (restart, session gone), the revoke still records it: absence at
  the engine is the revoked state.
- **The session ends**: its grants go with its registry entry. **The engine
  restarts**: grants are memory-only in milestone 1, so they are gone. That
  fails closed, and the overseer re-requests. Milestone 3 keeps them in the
  session record (metadata only) so a wake keeps a live grant.

## Audit

| Where | What | Never |
| --- | --- | --- |
| core `audit` + `events` (`session.grant_requested`, `session.grant_decided`, `session.grant_revoked`) | grant id, target session, kind, `var`, project, secret *name*, `uses`, TTL, requester, approver, reasons | a value; the core never has one |
| engine `vogt::audit` `event=session.grant` | applied / revoked / expired / consumed: session, grant id, `var`, project, secret name | a value |
| engine `vogt::audit` broker fetch | session, `var`, `grant_id` (or the manifest), outcome | a value |

## Security invariants and how each is enforced

1. **A person decides; an agent cannot.** `session.grant_decide` refuses
   every agent principal before doing anything. Session tokens, `agent:engine:`
   tokens and the brokered pod token all count as agents (WI-926). *Test:*
   an agent-principal decide raises `GrantRefused`, and the row stays
   `pending`.
2. **Nothing reaches the engine except through that decision.** The engine's
   grant routes accept only the stack secret (`vogt-core`), decided by the
   credential compared (`AuthorizedIdentity.stack_secret`), not by its name,
   which a core actor's `identity_ref` could also spell. A session holds a
   core token and a broker token, and neither opens them. *Test:* a person's
   token, the break-glass token and a session token all get 403 on `POST
   …/grants`. **Caveat, stated plainly: this is not a uid boundary.** The
   stack secret is in the engine's environment and its token file, and
   sessions run as the engine's uid; a session that reads `/proc/1/environ`
   or that file holds it and can apply a grant to itself with no person
   involved. (From a session on this deployment, `/proc/1/environ` is
   readable and names `VOGT_CORE_TOKEN_FILE`; the 2026-10-07 review did not
   verify the file's own mode.) The engine warns at start-up when a token
   file is group- or world-readable. Until sessions run as a separate uid,
   invariant 2 narrows ambient exposure and adds an audited, person-approved
   path; calling grants a *boundary* waits on that uid line, which is the
   prerequisite work this design depends on (WI-982).
3. **No laundering.** The requester is the authenticated principal, never a
   parameter. An agent cannot approve (1), cannot apply (2), and cannot
   request for another session unless it is an overseer. A grant is bound to
   one engine session id, and it is used with that session's broker token,
   which another session does not hold. *Test:* a second session's broker
   token cannot fetch the first session's grant.
4. **Least privilege.** One grant is one secret, under one `var`, in one
   grantable project. It is refused if it names a manifest `var` (no
   shadowing) or a project the operator has not opened. Deny by default: no
   grant, no fetch. *Test:* an ungrantable project and a manifest `var` are
   both refused.
5. **Time-boxed.** At most 24 h. Expiry is enforced by the engine at use, not
   only displayed. A `once` grant is taken out of the table *before* its fetch
   runs, so of two racing fetches only one is answered, and it is never put
   back: a failed fetch has spent it, because restoring it would also undo a
   revoke that landed while the helper ran. *Test:* an expired grant is
   refused at fetch, a `once` grant answers only the first fetch, and a
   `once` grant whose helper fails is gone afterwards.
6. **Revocable at once.** A revoke deletes the grant at the engine before it
   is recorded. The session ending or the engine restarting drops every grant
   (fails closed).
7. **No value at rest, no value in transit through the core.** The value
   exists only in the engine's helper call and in the fetching session's
   stdout, at the moment of the fetch. Nothing writes it to a record, a log, an
   audit row or the core.
8. **Audited by name.** Every step above writes an audit line naming the
   session, the grant and the secret *name*.
9. **Does not weaken what exists.** The manifest path is unchanged and
   checked first. The driven-session policy keeps every default, and its one
   new allow rule is scoped to items `vogt-agent-auth grants` lists. Bypass
   stays a person's grant (WI-926). Milestone 2 only lets a person grant it
   to a running session too.
10. **The guarantee holds in brokered mode, not in identity mode.** In
    `identity` mode (`ENGINE_AGENT_AUTH_IDENTITY_PASSTHROUGH=1`) every session
    holds the machine identity. It can read any secret directly, including a
    person's front-door token, and use that token to approve its own grant.
    Grants then add an approved, audited, classifier-visible path, but not a
    boundary: the vault is already in the session's hands. The human-in-the-loop
    property is real only where sessions are brokered, and it is the reason to
    run brokered. The operator questions below ask whether to keep identity
    mode once grants exist.

## Milestones

| | Content | Layers |
| --- | --- | --- |
| **1 (WI-973, this change)** | `session_grants` table (migration 0019); `session.grant_request` / `_decide` / `_revoke` / `_list` (CLI, REST, MCP); Inbox entry with Approve/Deny; engine `POST`/`DELETE`/`GET /api/sessions/{id}/grants` (stack secret only for writes), grant-aware broker fetch, `GET /api/agent-auth/grants`, `vogt-agent-auth grants`; driven-session allow rule | core, engine, PWA, deploy |
| 2 | Capability grants applied by hibernate → wake with the granted posture, reverted at expiry or revoke | engine, core |
| 3 | Grants kept in the session record (metadata) across a wake; the PWA lists live grants per session with Revoke; the sweep turns expiry into an event | engine, core, PWA |

## Open questions for the operator

1. **Grantable projects.** Is "the projects the manifest already names" the
   right default, and should `ENGINE_AGENT_GRANT_PROJECTS` add more? Or should
   grants be limited to an explicit list from the start?
2. **Who may approve.** Any person, or `admin` only? Milestone 1 takes any
   person, matching who may grant bypass today.
3. **Defaults.** `uses: once` with a 1 h TTL by default and 24 h at most. Are
   those right?
4. **The classifier rule.** Is an allow rule in the driven-session policy
   acceptable for approved grants, or should a granted credential be handed to
   the session some other way (a file the engine writes)? A file exists at
   rest, which this design avoids.
5. **Requests from non-overseers.** A session may request a grant for itself.
   Should only oversight sessions be able to request at all?
6. **Identity mode.** This deployment runs `identity` mode, in which a session
   can read any secret, including a person's token, so grants are not a
   boundary there (invariant 10). Should a deployment that adopts grants move
   to `brokered`, and make grants the way a session gets what the manifest
   does not give it?
