# Source providers

Status: design for WI-3392 on `rust-2` (2026-11-06).

This note defines the boundary between a project and the system that checks
out, inspects, saves, and lands its source. It is deliberately a design, not
an implementation claim. The public product keeps Git as its default; the
maintainers' deployment may select Graft as its primary provider. Neither
choice is a property of the engine's session code.

## Goals and boundary

A project has one source descriptor. The descriptor is carried in the shared
`engine/contract` crate so the Rust core, engine, and pod agree on the same
wire shape. The project owns the descriptor; deployment configuration supplies
provider credentials, endpoints, and safe defaults but does not replace the
project's source.

The descriptor contains:

- `kind`: `git` or `graft`;
- `location`: provider endpoint plus repository identifier (for Graft), or
  clone URL plus repository identity (for Git);
- `ref`: branch for Git or line for Graft;
- `landing`: the selected landing mode and its provider-specific destination;
- a stable, non-secret repository identity used for grants, labels, audit, and
  cache keys.

URLs and repository identities are metadata, not credentials. Tokens are
issued at runtime and are never part of the descriptor, session specification,
pod template, or persisted project JSON.

The core's project model should replace the current loose `repo_url` plus
opaque landing value with this typed source (or a compatibility projection
while migration is in progress). Its landing policy remains the product
decision: the source says where and how, while the core says whether the
requested operation is allowed and records the result.

## Provider interface

The engine owns a provider registry keyed by `SourceKind`. The registry is
constructed from deployment capabilities; selecting a provider is data-driven,
not a `match` on `graft` spread through session code. A provider receives a
validated descriptor and a scoped credential handle, and operates only inside
the session's checkout directory.

The interface has these operations:

1. **`checkout(source, destination, credential)`** — create a clean checkout
   at the requested branch or line. It is idempotent only for an empty or
   provider-owned destination; an existing non-empty directory is refused.
2. **`mint_credential(source, session)` / `revoke_credential(credential)`** —
   issue and retire a least-privilege credential scoped to the repository and
   ref. The returned value is a handle or secret-file path, never a value in a
   response DTO or log.
3. **`status(checkout)`**, **`diff(checkout)`**, and **`log(checkout, range)`** —
   return provider-neutral read models containing clean/dirty state, changed
   paths or patch data, and landed/unlanded history with provider identifiers.
   The Git view is a read-only compatibility projection of these operations;
   it is not a second source of truth.
4. **`commit(checkout, message)`** — record a provider-native work unit when
   the provider supports commits. The operation must return its opaque id and
   preserve the dirty-work distinction. A provider that stages or drafts
   instead exposes that fact in the result rather than pretending it made a
   Git commit.
5. **`save_unlanded(checkout, reason)`** — persist recoverable work before
   hibernate/stop and periodically while a pod is alive. The result identifies
   the draft/snapshot and excludes build output. Restore is the inverse of
   this operation during checkout or wake, using the provider's id.
6. **`sync(checkout)`** — refresh the checkout from the selected branch/line
   when the provider supports it, with explicit conflict and dirty-work
   results. It never silently discards local work.
7. **`land(checkout, request)`** — submit the work according to the source's
   landing mode. The request carries message, target, and review metadata; the
   provider owns its transport and returns a pending or landed result.
8. **`report_landed(source, selector)`** — report changesets, branches, pull
   requests, merge state, line, and timestamps associated with a work item.
   It is the common input to delivery evidence (WI-3393), not a Git-only
   collector assumption.

`branch` and `line` are one conceptual field, `ref`, in the descriptor and
read models. Provider-specific terminology remains in the provider adapter.
Errors are typed as unavailable, unauthorized, invalid source, dirty checkout,
conflict, or not landed; raw subprocess output does not cross the API boundary.

## Git provider

Git is the default provider for the public product and for projects without an
explicit source kind. It:

- clones the descriptor's URL at the requested branch using a brokered,
  repository-scoped forge credential;
- stores the credential outside the pod specification and removes it after
  use, with revocation delegated to the broker;
- implements status, diff, log, commit, sync, and branch operations with the
  existing Git view and workspace rules;
- saves unlanded work as a provider-owned work-in-progress ref or archive,
  pending the decision in WI-3370, and restores it on wake;
- lands by the project's Git landing policy (for example push a branch or open
  a pull request); and
- reports branch, pull-request, merge, and commit evidence for WI-3393.

A Git checkout may have a remote for provider operations, but the pod must not
receive a generally reusable forge token. Extra repositories use the same
descriptor and checkout operation, are read-only by default, and receive a
separate repository-scoped grant (WI-3372).

## Graft provider

Graft is a provider, not a session concept. It moves today's entrypoint
behaviour behind the interface:

- `checkout` calls `g clone` for the descriptor's endpoint, repository, and
  line;
- `mint_credential` and `revoke_credential` use the existing graft mint
  service with repository/line scope;
- status, diff, log, commit, sync, save, land, and landed reporting use graft
  changesets, drafts, line history, and metadata;
- `save_unlanded` stores a graft draft and restores it over a fresh clone; and
- `land` and `report_landed` preserve the changeset id, line, work-item
  metadata, and later batch-merge state.

The current read-only Git view remains useful to agents that expect `git
status`, `git diff`, or `git log`. The graft provider may materialize that view
after checkout, as the current entrypoint does, but no caller may infer that a
Graft checkout is a Git repository or land through its synthetic history.

The `ENGINE_POD_GRAFT_*` settings become graft-provider settings. Keep them as
aliases for one release, translate them once at configuration load, and emit a
deprecation warning without exposing the alias in the source descriptor. No
file outside the graft provider and its configuration adapter should name
Graft.

## Selection and configuration

Project configuration is authoritative for source selection:

```text
project.source = {
  kind: "git" | "graft",
  endpoint: "...",
  repo: "...",
  ref: "branch-or-line",
  landing: { mode: "push_branch" | "open_pr" | "graft", ... }
}
```

The exact serialized field names belong in `engine/contract` and the core
model; the example shows ownership, not a frozen wire spelling. `git` is the
compatibility default for existing projects with `repo_url`. A graft source
requires an explicit repository and line, either from the project or the
deployment's migration default. A missing or unsupported provider is an
honest unavailable start error, not an implicit fallback that could check out
the wrong repository.

At `session.start`, the core resolves the project and includes the source
descriptor in `SessionSpec`. The engine validates it, selects a registered
provider, and includes the same descriptor in the bootstrap response sent to
the pod. The pod receives a short-lived scoped credential through the existing
broker path; it does not receive deployment-wide source settings. Labels and
audit records use the stable repository identity and provider kind.

## Pod flow

The provider-neutral boot sequence is:

1. validate the descriptor and provider capability before creating the pod;
2. mint the scoped credential outside the pod spec;
3. start the pod with the descriptor and one-use credential handoff;
4. the session entrypoint invokes the provider's checkout step into `/work`;
5. optionally materialize the read-only Git view for agent compatibility;
6. restore a provider draft if wake requested one; and
7. on stop, hibernate, or periodic checkpoint, call `save_unlanded`, then
   revoke the credential when the session is torn down.

The session agent calls provider verbs, never `git`, `graft`, or a provider
binary by name. The same path therefore starts a Git and a Graft project. A
second repository is another descriptor passed to `checkout`, with a separate
scope and read-only policy by default.

## Migration

Migration is staged so a deployment can move without changing session
semantics:

1. Add the descriptor and provider-neutral result types to `engine/contract`;
   add the Rust core dependency and project mapping, preserving old fields on
   decode.
2. Add the engine provider trait/registry and adapt the existing Graft mint,
   clone, teardown, and git-view code into the Graft provider. Keep the old
   `ENGINE_POD_GRAFT_*` names as one-release aliases.
3. Pass the descriptor through `session.start`, bootstrap, pod labels, and
   grants. Remove deployment-wide repo/line selection from `PodConfig` after
   the compatibility window.
4. Add the Git provider using brokered forge credentials, then make Git the
   default for legacy projects and Graft explicit for maintainer projects.
5. Move save/restore and landed reporting to the provider verbs. WI-3370 and
   WI-3393 must consume those verbs rather than add provider-specific paths.
6. Run the pod smoke once with a Git fixture and once with a Graft fixture,
   asserting descriptor propagation, credential scope, checkout, read-only Git
   view, and teardown. Remove aliases only after one release and a migration
   warning period.

This item must not edit `pod_backend.rs` or `sessions.rs`; those are consumers
of the seam in the implementation items that follow.

## Dependents

- **WI-3369:** use the session project's descriptor for checkout, grant scope,
  bootstrap, and labels; remove the deployment's single-repository assumption.
- **WI-3372:** use the Git provider for Git-only projects and invoke checkout
  again for an explicitly declared second repository, read-only by default.
  The operator still needs to decide whether extra repositories are declared at
  start or requested through the grant flow.
- **WI-3370:** call `save_unlanded` and restore through the selected provider;
  do not make the session agent know whether recovery is a Graft draft or a
  Git work-in-progress ref/archive.
- **WI-3393:** call `report_landed` for provider-neutral delivery evidence;
  distinguish "on a Graft line" from "merged to `main`" and suppress stale
  session-branch drift for Graft projects.

## Open questions and recommendations

These are decisions for the operator, not blockers to this design:

1. **Should the descriptor use one `location` union or endpoint/repo fields?**
   Recommend a typed `SourceLocation` union in the contract, with explicit
   endpoint and repository members for both providers. It prevents a Git URL
   from being mistaken for a Graft repository id while keeping one outer
   descriptor.
2. **Should the engine and Rust core share the provider trait?** Recommend no:
   share only wire DTOs in `engine/contract`; keep provider execution in the
   engine and source/landing decisions in the core. This preserves the current
   process boundary and avoids coupling domain code to subprocesses.
3. **What is Git's durable representation of unlanded work?** Recommend a
   broker-owned, repository-scoped WIP ref or patch archive, chosen by the Git
   provider, with no automatic push to a user's branch. Confirm in WI-3370
   before promising crash recovery semantics.
4. **When may a session add a second repository?** Recommend declaration at
   `session.start` first, read-only by default; add an audited grant flow later
   if WI-3372 demonstrates a need. This keeps pod identity and scopes stable.
5. **Should a Graft checkout always materialize a Git view?** Recommend yes as
   a best-effort compatibility projection, but expose its read-only and
   synthetic nature and never use it for provider operations.
6. **When can the legacy Graft settings be removed?** Recommend after one
   release with startup warnings, migration telemetry, and both-provider smoke
   coverage. Until then aliases must map only at configuration load.
7. **Where should provider landed evidence be collected?** Recommend in the
   core's existing collection/evidence layer through `report_landed`, with the
   provider returning untrusted observations and the core attaching freshness,
   provenance, and work-item metadata.

