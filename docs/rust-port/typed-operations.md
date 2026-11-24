# Typed operations (WI-4616)

Status: pilot landed on the `label.*` family. Follow-ups: WI-4617 (`work.*`),
WI-4618 (`session.*`), then the rest.

## Problem

Every operation is `fn(&Built, Value) -> Result<Value, VogtError>`
(`registry::ServiceFn`). Services re-parse the JSON by hand, so defaults live
twice and a mistyped key compiles. The `*Params` models in
`application/models` only generate schemas.

## Signature

A typed handler is

```rust
fn handler<C, I>(ctx: &AppContext<C, I>, params: P) -> Result<R, VogtError>
where P: DeserializeOwned (+ JsonSchema), R: Serialize
```

`application/typed.rs` provides `decode`, `decode_or_empty` and `encode`, and
the `typed_op!(name, P, handler[, null_is_empty])` macro. The macro expands to
an ordinary `ServiceFn` (`fn(&Built, Value)`) that opens the context with
`with_ctx!`, decodes, calls the handler and encodes. The registry arm is
unchanged, so typed and Value operations coexist and families migrate one at a
time. The handler stays generic over the clock and id factory, which is why
the adapter is a macro and not a generic function: the four `Built` variants
need one monomorphisation each.

## Edges

- HTTP and MCP still parse the body to `serde_json::Value` and call the
  registry; the wire is unchanged. Schemas still come from the models in
  `application/models` (`registry.json`).
- Params: `decode` is `serde_json::from_value`; failures map to
  `VogtError::InvalidRequest(serde message)`, the mapping the hand-typed
  services already used. `null_is_empty` reads a null body as `{}` for
  all-default parameter types (list calls).
- Results: `encode` is `serde_json::to_value`. Result structs must serialize to
  exactly the keys the Value form built with `json!`.

## Validation, aliases, defaults

- Defaults: `#[serde(default)]` / `default = "fn"` on the parameter type, once.
  Target state is one type serving both schema and decode (the models'
  `*Params`); the pilot keeps the existing local decode structs because the
  models use `deny_unknown_fields`, which would reject extra keys the services
  accept today. Unifying them is a deliberate, snapshot-visible decision for a
  follow-up, per family.
- Aliases (`project`, `id` for `slug`, etc.): `#[serde(alias = "...")]` on the
  field; they stay invisible to the schema as today.
- Range and length limits are schema-level; semantic validation (slug, state
  parsing) stays in the handler and returns the same errors.

## Error mapping

Handlers return `VogtError` directly, unchanged; the transports' existing
`VogtError` to status/code mapping applies. Decode errors are
`InvalidRequest`. No new variants.

## Auth context

Unchanged. The hooks build `Built` (principal, clock, ids, store) before the
registry call; `typed_op!` passes the `AppContext` to the handler, which takes
the principal through `context_parts` / `write_of` as before. Scope and
`reason` checks stay in the registry and `audited_write`.

## Proof of no change

Existing parity tests, `registry.json` and the HTTP/MCP wire snapshots must
pass unchanged; a migration PR touches no snapshot file. For any new family,
add a test that decodes the same JSON through the old and new paths when the
old path is being replaced.

## Pilot: label.create, label.list

Both now take typed params and return typed results through `typed_op!`; no
`.get("...")` reads remain. Initiatives (`initiative.*`) were deferred: they
use hand-rolled coercion with bespoke error text (`require_string`,
`optional_i64`, `params.get("body")` null-vs-absent semantics in update), so
migrating them needs explicit decisions on those error messages and is the
next step before the other families.
