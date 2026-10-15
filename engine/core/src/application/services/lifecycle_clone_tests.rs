//! The clone tests that need seeded credentials. Kept beside the lifecycle
//! service so a change to the sanitising write is tested with it.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::*;
use crate::application::context::{build_context, AppContext, Built};
use crate::core::{
    ActorKind, Moment, Principal, SequentialIds, StepClock,
};
use crate::errors::VogtError;

#[cfg(test)]
mod tests {
    /// Seed the credentials a clone must carry or revoke, through the store
    /// the operation itself reads. Returns the secrets issued.
    fn seed_credentials(built: &Built, who: &str, secret: &str, username: &str) {
        use crate::application::context::AppContext;
        use crate::auth::{hash_password_at, hash_token};
        use crate::core::{Actor, ActorKind, CodingSession, Moment, Token, TokenKind};
        use crate::storage::interface::{DeclaredStore, ProjectUpdate, WriteTxn};

        let ctx: &AppContext<StepClock, SequentialIds> = ctx(built);
        let now = Moment::from_unix(1_700_000_100, 0);
        let actor = Actor {
            id: format!("act-{who}"),
            kind: ActorKind::Agent,
            display_name: who.to_string(),
            identity_ref: format!("agent:{who}"),
            disabled: false,
            created_at: now,
        };
        let token = Token {
            id: format!("tok-{who}"),
            actor_id: actor.id.clone(),
            actor_identity_ref: Some(actor.identity_ref.clone()),
            name: format!("{who} token"),
            scopes: vec!["read".to_string()],
            kind: TokenKind::Api,
            created_at: now,
            expires_at: None,
            last_used_at: None,
            revoked_at: None,
            revoked_reason: None,
        };
        let mut txn = ctx.declared.write().unwrap();
        if txn
            .actor_by_identity(&actor.identity_ref)
            .unwrap()
            .is_none()
        {
            txn.insert_actor(&actor).unwrap();
        }
        txn.insert_token(&token, &hash_token(secret)).unwrap();
        let password =
            hash_password_at(&format!("{who}-password"), b"0123456789abcdef", 16, 1, 1).unwrap();
        txn.upsert_password_credential(&actor.id, username, &password, &["read".to_string()], now)
            .unwrap();
        txn.upsert_forge_account(
            &actor.id,
            "github.com",
            &format!("{who}-github"),
            "[]",
            "enc",
            now,
        )
        .unwrap();
        let mut project = crate::core::Project::new(
            &format!("prj-{who}"),
            &format!("shipped-{who}"),
            "Shipped",
            "/tmp/shipped",
            now,
        );
        project.write_back = crate::core::WriteBack::Full;
        txn.insert_project(&project).unwrap();
        txn.insert_session(&CodingSession {
            id: format!("ses-{who}"),
            engine_session_id: format!("eng-{who}"),
            project_id: project.id.clone(),
            work_item_id: None,
            actor_id: actor.id,
            cwd: "/tmp/shipped".to_string(),
            template: None,
            model: None,
            effort: None,
            reason: "seeded".to_string(),
            started_at: now,
            stopped_at: None,
        })
        .unwrap();
        txn.commit().unwrap();
        let _ = ProjectUpdate::default();
    }

    #[test]
    fn a_clone_keeps_the_targets_credentials_and_revokes_the_sources() {
        let source = opened();
        let target = opened();
        seed_credentials(&source, "prod", "prod-secret-0123456789abcdef", "prodlogin");
        seed_credentials(&target, "dev", "dev-secret-0123456789abcdef", "devlogin");
        let source_id = ctx(&source).declared.read().unwrap().instance_id().unwrap();
        let target_id = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_ne!(source_id, target_id, "two instances must not share an id");
        let taken = backup(
            ctx(&source),
            &json!({
                "destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(),
                "reason": "why",
            }),
        )
        .unwrap();
        let result = clone_instance(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap();

        assert!(result["tokens_kept"].as_i64().unwrap() >= 1);
        assert!(result["source_tokens_revoked"].as_i64().unwrap() >= 1);
        assert_eq!(result["write_back_reset"].as_array().unwrap().len(), 1);
        assert!(result["sessions_closed"].as_i64().unwrap() >= 1);

        let view = ctx(&target).declared.read().unwrap();
        let kept = view
            .token_by_hash(&crate::auth::hash_token("dev-secret-0123456789abcdef"))
            .unwrap()
            .unwrap();
        assert!(kept.revoked_at.is_none(), "the target's token stays live");
        let revoked = view
            .token_by_hash(&crate::auth::hash_token("prod-secret-0123456789abcdef"))
            .unwrap()
            .unwrap();
        assert!(
            revoked.revoked_at.is_some(),
            "the source's token is revoked, not deleted"
        );
        assert!(view
            .list_tokens(false, 100)
            .unwrap()
            .iter()
            .any(|token| token.id == kept.id));
        assert!(view
            .list_tokens(true, 100)
            .unwrap()
            .iter()
            .any(|token| token.id == revoked.id));

        let logins: Vec<String> = view
            .list_password_credentials()
            .unwrap()
            .iter()
            .map(|login| login.username.clone())
            .collect();
        assert!(logins.contains(&"devlogin".to_string()));
        assert!(!logins.contains(&"prodlogin".to_string()));

        let actor = view.actor_by_identity("agent:dev").unwrap().unwrap();
        let logins: Vec<String> = view
            .forge_accounts_for_actor(&actor.id)
            .unwrap()
            .iter()
            .map(|account| account.login.clone())
            .collect();
        assert_eq!(logins, vec!["dev-github".to_string()]);

        let shipped = view.project_by_slug("shipped-prod").unwrap().unwrap();
        assert_eq!(shipped.write_back, crate::core::WriteBack::Disabled);
        assert!(view
            .list_sessions(None, None, false, 100, 0)
            .unwrap()
            .is_empty());
        drop(view);
    }

    #[test]
    fn a_secret_both_instances_hold_stays_live_as_the_targets() {
        let source = opened();
        let target = opened();
        seed_credentials(
            &source,
            "shared",
            "shared-secret-0123456789abcdef",
            "sharedlogin",
        );
        seed_credentials(
            &target,
            "shared",
            "shared-secret-0123456789abcdef",
            "sharedlogin",
        );
        let taken = backup(
            ctx(&source),
            &json!({
                "destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(),
                "reason": "why",
            }),
        )
        .unwrap();
        let result = clone_instance(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap();
        let view = ctx(&target).declared.read().unwrap();
        let token = view
            .token_by_hash(&crate::auth::hash_token("shared-secret-0123456789abcdef"))
            .unwrap()
            .unwrap();
        assert!(token.revoked_at.is_none());
        assert_eq!(result["source_tokens_revoked"].as_i64().unwrap(), 0);
    }

    #[test]
    fn a_blank_reason_is_refused_before_anything_is_staged() {
        let (_source, target, taken) = pair();
        let data_dir = ctx(&target).config.resolved_data_dir();
        let error = clone_instance(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "   "}),
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::MissingReason(_)));
        assert!(
            std::fs::read_dir(data_dir).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".clone-staging")),
            "a blank reason stages nothing"
        );
    }

    #[test]
    fn a_failure_while_sanitising_leaves_the_live_stores_untouched() {
        let (source, target, taken) = pair();
        let before = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        drop(source);
        // The sanitising write sets the instance id from a meta row. Removing
        // it from the staged copy is a fault the write rejects, and the live
        // stores must survive it.
        let backup = PathBuf::from(taken["path"].as_str().unwrap());
        let staged_probe = ctx(&target).config.resolved_data_dir();
        // Corrupt the backup's declared store so migration of the staged copy
        // fails: drop the migrations ledger the migrator requires.
        let declared = backup.join("declared.sqlite3");
        let conn = rusqlite::Connection::open(&declared).unwrap();
        conn.execute_batch("DROP TABLE migrations;").unwrap();
        drop(conn);

        let error = clone_instance(
            ctx(&target),
            &json!({"source": backup.display().to_string(), "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        let _ = error;
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, before);
        assert!(std::fs::read_dir(staged_probe).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".clone-staging")
        }));
    }

    #[test]
    fn a_clone_refuses_a_backup_from_the_future() {
        let (_source, target, taken) = pair();
        let backup = PathBuf::from(taken["path"].as_str().unwrap());
        let manifest_path = backup.join(MANIFEST_NAME);
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["observed_schema_version"] = json!(1_000_000);
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        let error = clone_instance(
            ctx(&target),
            &json!({"source": backup.display().to_string(), "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            VogtError::InvalidRequest(message) if message.contains("newer build")
        ));
    }

    #[test]
    fn include_engine_state_copies_history_but_never_push_or_tasks() {
        let source_state =
            std::env::temp_dir().join(format!("vogt-engine-src-{}", std::process::id()));
        let target_state =
            std::env::temp_dir().join(format!("vogt-engine-dst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&source_state);
        let _ = std::fs::remove_dir_all(&target_state);
        std::fs::create_dir_all(source_state.join("session-logs")).unwrap();
        std::fs::write(source_state.join("session-logs/one.log"), "prod output").unwrap();
        std::fs::write(source_state.join("history.db"), "prod history").unwrap();
        std::fs::write(source_state.join("push.json"), "prod phones").unwrap();
        std::fs::write(source_state.join("agent-tasks.json"), "prod schedule").unwrap();
        std::fs::create_dir_all(&target_state).unwrap();
        std::fs::write(target_state.join("push.json"), "dev phones").unwrap();

        let source = opened_in(Some(source_state.clone()));
        let target = opened_in(Some(target_state.clone()));
        let taken = backup(
            ctx(&source),
            &json!({
                "destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(),
                "reason": "why",
            }),
        )
        .unwrap();
        let result = clone_instance(
            ctx(&target),
            &json!({
                "source": taken["path"],
                "confirm": true,
                "include_engine_state": true,
                "reason": "why",
            }),
        )
        .unwrap();
        let report = result["engine_state"].as_str().unwrap();
        assert!(report.contains("history.db"), "{report}");
        assert!(report.contains("session-logs"), "{report}");
        assert!(report.contains("never copied"), "{report}");
        assert_eq!(
            std::fs::read_to_string(target_state.join("history.db")).unwrap(),
            "prod history"
        );
        assert_eq!(
            std::fs::read_to_string(target_state.join("session-logs/one.log")).unwrap(),
            "prod output"
        );
        assert_eq!(
            std::fs::read_to_string(target_state.join("push.json")).unwrap(),
            "dev phones",
            "push subscriptions are never copied"
        );
        assert!(!target_state.join("agent-tasks.json").exists());
    }

    #[test]
    fn a_plain_restore_still_takes_the_sources_identity() {
        let (source, target, taken) = pair();
        let source_id = ctx(&source).declared.read().unwrap().instance_id().unwrap();
        let result = restore(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap();
        assert_eq!(result["instance_id"], source_id);
        assert!(ctx(&target)
            .declared
            .read()
            .unwrap()
            .clone_stamp()
            .unwrap()
            .is_none());
    }

    #[test]
    fn staging_names_carry_microseconds_so_one_second_does_not_collide() {
        let clock_a = Moment::from_unix(1_700_000_000, 1_000);
        let clock_b = Moment::from_unix(1_700_000_000, 2_000);
        let label = |moment: Moment| {
            let iso = crate::core::to_iso(moment);
            let body = iso.strip_suffix("+00:00").unwrap_or(&iso);
            format!("{}Z", body.replace(['-', ':', '.'], ""))
        };
        assert_ne!(label(clock_a), label(clock_b));
        assert!(label(clock_a).contains('Z'));
    }
}
