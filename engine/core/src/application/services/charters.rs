//! Charters: standing instructions per scope, injected into briefs (WI-2038).
//! Ports `application/services/charters.py`.
//!
//! A charter is a short Markdown text attached to the workspace, a project or
//! an initiative. Every session brief started on that scope carries it, in the
//! order workspace, then project, then initiative, then the brief itself.
//!
//! - **Versioned, never edited in place.** Each set appends the next version,
//!   and the write is audited like any other.
//! - **Empty clears.** An empty body is a new version that says "no charter".
//! - **The workspace charter is a person's.** An agent principal cannot set it;
//!   project and initiative charters stay `work.write`.
//! - **Optimistic.** `expected_version` refuses a write made against a version
//!   that is no longer current, as `preference.set` does.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::application::models::records::{
    CharterGetParams, CharterGetResult, CharterScopeKind, CharterSetParams, CharterSetResult,
    CharterView,
};
use crate::application::resolve;
use crate::application::services::{context_parts, dispatch, now_of, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{ActorKind, Charter, Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

pub const CHARTER_SET: &str = "charter.set";
pub const CHARTER_SET_EVENT: &str = "charter.set";

pub fn charter_get_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, charter_get, params)
}

pub fn charter_set_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, charter_set, params)
}

struct Scope {
    kind: CharterScopeKind,
    id: String,
    reference: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, VogtError> {
    serde_json::from_value(params).map_err(|error| VogtError::InvalidRequest(error.to_string()))
}

fn scope_of(
    view: &impl ReadView,
    kind: CharterScopeKind,
    reference: Option<&str>,
) -> Result<Scope, VogtError> {
    let reference = reference.filter(|text| !text.is_empty());
    match kind {
        CharterScopeKind::Workspace => {
            if reference.is_some() {
                return Err(VogtError::InvalidRequest(
                    "the workspace charter takes no ref".to_string(),
                ));
            }
            Ok(Scope {
                kind,
                id: String::new(),
                reference: None,
            })
        }
        _ => {
            let Some(reference) = reference else {
                let name = kind.as_str();
                return Err(VogtError::InvalidRequest(format!(
                    "a {name} charter needs ref (the {name} slug)"
                )));
            };
            let id = if kind == CharterScopeKind::Project {
                resolve::project(view, reference)?.id
            } else {
                resolve::initiative(view, reference)?.id
            };
            Ok(Scope {
                kind,
                id,
                reference: Some(reference.to_string()),
            })
        }
    }
}

fn view_of(view: &impl ReadView, scope: &Scope, row: &Charter) -> Result<CharterView, VogtError> {
    let author = match view.actor_by_id(&row.author_actor_id)? {
        Some(actor) => actor.identity_ref,
        None => row.author_actor_id.clone(),
    };
    Ok(CharterView {
        scope: scope.kind,
        reference: scope.reference.clone(),
        version: row.version,
        body: row.body.clone(),
        author,
        created_at: row.created_at.to_json(),
        reason: row.reason.clone(),
    })
}

fn charter_get<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params: CharterGetParams = parse(params)?;
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    let scope = scope_of(&view, params.scope, params.reference.as_deref())?;
    let kind = scope.kind.as_str();
    let current = match view.charter(kind, &scope.id)? {
        Some(row) => Some(view_of(&view, &scope, &row)?),
        None => None,
    };
    let history = if params.history {
        view.charter_history(kind, &scope.id, params.limit)?
            .iter()
            .map(|row| view_of(&view, &scope, row))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    Ok(serde_json::to_value(CharterGetResult { current, history }).expect("a result serialises"))
}

fn charter_set<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params: CharterSetParams = parse(params)?;
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    if params.scope == CharterScopeKind::Workspace && matches!(principal.kind, ActorKind::Agent) {
        return Err(VogtError::PersonRequired(format!(
            "the workspace charter reaches every session on every project, so only a person sets it, not an agent ({})",
            principal.identity_ref
        )));
    }
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    set_charter(&mut writing, params, clock)
}

/// Split out against the concrete store, as `preferences::set_preference` is.
fn set_charter<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut crate::application::writes::WriteContext<
        '_,
        C,
        I,
        crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>,
    >,
    params: CharterSetParams,
    clock: Arc<std::sync::Mutex<C>>,
) -> Result<Value, VogtError> {
    let body_text = params.body.trim().to_string();
    let reason = params.reason.clone();
    audited_write(writing, CHARTER_SET, &reason, |txn, actor| {
        let scope = scope_of(&*txn, params.scope, params.reference.as_deref())?;
        let kind = scope.kind.as_str();
        let current = txn
            .charter(kind, &scope.id)?
            .map(|row| row.version)
            .unwrap_or(0);
        if let Some(expected) = params.expected_version {
            if expected != current {
                return Err(VogtError::CharterVersionConflict(format!(
                    "the {kind} charter is at version {current}, not {expected}; re-read it and apply the change on top of the current text"
                )));
            }
        }
        let charter = Charter {
            scope_kind: kind.to_string(),
            scope_id: scope.id.clone(),
            version: current + 1,
            body: body_text.clone(),
            author_actor_id: actor.id.clone(),
            created_at: now_of(&clock),
            reason: params.reason.clone(),
        };
        txn.insert_charter(&charter)?;
        let payload = json!({
            "scope_kind": charter.scope_kind,
            "scope_id": charter.scope_id,
            "version": charter.version,
            "body": charter.body,
            "author_actor_id": charter.author_actor_id,
            "created_at": charter.created_at.to_json(),
            "reason": charter.reason,
        });
        let summary = json!({
            "scope": kind,
            "ref": scope.reference,
            "version": charter.version,
            "cleared": body_text.is_empty(),
        });
        let result = CharterSetResult {
            charter: view_of(&*txn, &scope, &charter)?,
        };
        Ok(WriteOutcome::new(
            serde_json::to_value(result).expect("a result serialises"),
            "charter",
            &format!("{kind}:{id}", id = scope.id),
            payload,
            CHARTER_SET_EVENT,
            summary,
        ))
    })
}

/// The charters a brief carries, in inheritance order: workspace, then
/// project, then initiative. Each is `(heading, charter)`; a scope with no
/// charter, or a cleared one, is left out.
pub fn charters_for(
    view: &impl ReadView,
    project_id: Option<&str>,
    initiative_id: Option<&str>,
) -> Result<Vec<(String, Charter)>, VogtError> {
    let mut scopes: Vec<(String, &str, String)> =
        vec![("Workspace".to_string(), "workspace", String::new())];
    if let Some(project_id) = project_id {
        let label = view
            .project_by_id(project_id)?
            .map(|project| project.slug)
            .unwrap_or_else(|| project_id.to_string());
        scopes.push((
            format!("Project \u{2014} {label}"),
            "project",
            project_id.to_string(),
        ));
    }
    if let Some(initiative_id) = initiative_id {
        let label = view
            .initiative_by_id(initiative_id)?
            .map(|initiative| initiative.slug)
            .unwrap_or_else(|| initiative_id.to_string());
        scopes.push((
            format!("Initiative \u{2014} {label}"),
            "initiative",
            initiative_id.to_string(),
        ));
    }
    let mut found = Vec::new();
    for (heading, kind, scope_id) in scopes {
        if let Some(row) = view.charter(kind, &scope_id)? {
            if !row.body.trim().is_empty() {
                found.push((heading, row));
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    //! Ports `tests/test_charters.py`, the cases that stand on the service and
    //! the brief section. Session start carrying the section is covered by
    //! the engine body test in `adapters::engine`.
    use serde_json::{json, Value};

    use super::{charter_get_op, charter_set_op, charters_for};
    use crate::application::brief::charter_section;
    use crate::application::context::{build_context, Built};
    use crate::application::services::initiatives;
    use crate::application::services::services_test::{opened, principal};
    use crate::core::{ActorKind, Principal};
    use crate::errors::VogtError;
    use crate::storage::interface::DeclaredStore;

    const WHY: &str = "standing rule";

    fn set(built: &Built, params: Value) -> Result<Value, VogtError> {
        let mut params = params;
        params["reason"] = json!(WHY);
        charter_set_op(built, params)
    }

    fn initiative(built: &Built) {
        initiatives::initiative_create_op(built, json!({"title": "Port", "reason": WHY})).unwrap();
    }

    #[test]
    fn each_set_is_a_new_version_with_history() {
        let built = opened();
        initiative(&built);
        let first = set(
            &built,
            json!({"scope": "initiative", "ref": "port", "body": "Commit as the bot."}),
        )
        .unwrap();
        let second = set(
            &built,
            json!({"scope": "initiative", "ref": "port", "body": "Commit as the bot. Bump crates."}),
        )
        .unwrap();
        assert_eq!(first["charter"]["version"], 1);
        assert_eq!(second["charter"]["version"], 2);
        assert_eq!(second["charter"]["author"], principal().identity_ref);
        assert_eq!(second["charter"]["ref"], "port");

        let got = charter_get_op(
            &built,
            json!({"scope": "initiative", "ref": "port", "history": true}),
        )
        .unwrap();
        assert_eq!(got["current"]["body"], "Commit as the bot. Bump crates.");
        let versions: Vec<i64> = got["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["version"].as_i64().unwrap())
            .collect();
        assert_eq!(versions, vec![2, 1]);
        let plain = charter_get_op(&built, json!({"scope": "initiative", "ref": "port"})).unwrap();
        assert_eq!(plain["history"], json!([]));
    }

    #[test]
    fn expected_version_refuses_a_stale_write() {
        let built = opened();
        set(
            &built,
            json!({"scope": "workspace", "body": "Stop at push.", "expected_version": 0}),
        )
        .unwrap();
        let stale = set(
            &built,
            json!({"scope": "workspace", "body": "Other.", "expected_version": 0}),
        );
        assert!(
            matches!(stale, Err(VogtError::CharterVersionConflict(ref message)) if message.contains("at version 1, not 0")),
            "{stale:?}"
        );
        let again = set(
            &built,
            json!({"scope": "workspace", "body": "Other.", "expected_version": 1}),
        )
        .unwrap();
        assert_eq!(again["charter"]["version"], 2);
    }

    #[test]
    fn scope_refs_are_checked() {
        let built = opened();
        assert!(matches!(
            set(
                &built,
                json!({"scope": "workspace", "body": "x", "ref": "vogt"})
            ),
            Err(VogtError::InvalidRequest(_))
        ));
        assert!(matches!(
            set(&built, json!({"scope": "project", "body": "x"})),
            Err(VogtError::InvalidRequest(_))
        ));
        let missing = set(
            &built,
            json!({"scope": "initiative", "body": "x", "ref": "nope"}),
        );
        assert!(missing.is_err_and(|error| error.code().contains("not_found")));
        let got = charter_get_op(&built, json!({"scope": "workspace"})).unwrap();
        assert_eq!(got["current"], Value::Null);
    }

    #[test]
    fn the_brief_section_is_broadest_first_and_skips_a_cleared_charter() {
        let built = opened();
        initiative(&built);
        set(
            &built,
            json!({"scope": "workspace", "body": "WORKSPACE RULE"}),
        )
        .unwrap();
        set(
            &built,
            json!({"scope": "initiative", "ref": "port", "body": "INITIATIVE RULE"}),
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        let initiative_id = {
            let view = ctx.declared.read().unwrap();
            crate::application::resolve::initiative(&view, "port")
                .unwrap()
                .id
        };
        let found = {
            let view = ctx.declared.read().unwrap();
            charters_for(&view, None, Some(&initiative_id)).unwrap()
        };
        let text = charter_section(&found).join("\n");
        assert!(text.starts_with("## Charters"));
        assert!(text.contains("### Workspace (v1)"));
        assert!(text.contains("### Initiative \u{2014} port (v1)"));
        assert!(text.find("WORKSPACE RULE") < text.find("INITIATIVE RULE"));

        set(&built, json!({"scope": "workspace", "body": "  "})).unwrap();
        set(
            &built,
            json!({"scope": "initiative", "ref": "port", "body": ""}),
        )
        .unwrap();
        let found = {
            let view = ctx.declared.read().unwrap();
            charters_for(&view, None, Some(&initiative_id)).unwrap()
        };
        assert!(charter_section(&found).is_empty());
    }

    #[test]
    fn only_a_person_sets_the_workspace_charter() {
        let built = opened();
        initiative(&built);
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        let agent = Principal::new("agent:session:ses_x", ActorKind::Agent, "x").unwrap();
        let as_agent = build_context(
            ctx.config.clone(),
            Some(agent),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let refused = set(
            &as_agent,
            json!({"scope": "workspace", "body": "Rewrite everything."}),
        );
        assert!(
            matches!(refused, Err(VogtError::PersonRequired(_))),
            "{refused:?}"
        );
        let allowed = set(
            &as_agent,
            json!({"scope": "initiative", "ref": "port", "body": "Initiative rule."}),
        )
        .unwrap();
        assert_eq!(allowed["charter"]["version"], 1);
    }
}
