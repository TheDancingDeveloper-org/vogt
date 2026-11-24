//! Bootstrap token adoption. Ports `adopt_bootstrap_core_token` and
//! `adopt_bootstrap_agent_token` in `services/auth.py`.
//!
//! `init` calls this once per token. It takes the caller's clock and id
//! counter by mutable reference rather than a clone: a copy of
//! `SequentialIds` draws ids from the count it held, so the rows it writes
//! would collide with the ones the caller hands out next.

use std::path::Path;

use crate::config::VogtConfig;
use crate::core::{IdFactory, SequentialIds};
use crate::errors::VogtError;
use crate::storage::sqlite::declared_path;

/// Which operator-supplied bootstrap secret `init` adopts.
#[derive(Clone, Copy)]
pub enum Bootstrap {
    /// The stack secret the engine presents (`adopt_bootstrap_core_token`).
    Core,
    /// The pod-wide token sessions' agents present (`adopt_bootstrap_agent_token`).
    Agent,
}

/// Adopt one operator-supplied bootstrap token, the way Python's
/// `adopt_bootstrap_core_token` and `adopt_bootstrap_agent_token` do at init.
/// The two differ only in configuration: which file, actor and scopes apply,
/// and the token name the row is stored under.
///
/// `"not_configured"` when the file is unset, unreadable or empty — the
/// instance still boots. `"already_present"` when an unrevoked row already
/// holds the hash, so the init that runs on every container start writes
/// nothing the second time. A secret that rotated *back* to a value a
/// rotation revoked reinstates that row instead of inserting a second one
/// (the hash is unique). A secret under 24 characters or an unknown scope is
/// a startup error: a deployment that believes it supplied a credential must
/// not come up without one.
pub fn adopt_bootstrap_token(
    config: &VogtConfig,
    data_dir: &Path,
    clock: &mut Option<crate::core::StepClock>,
    ids: &mut Option<SequentialIds>,
    which: Bootstrap,
) -> Result<String, VogtError> {
    let (file, actor, scope_text, name, reason, field) = match which {
        Bootstrap::Core => (
            config.bootstrap_core_token_file.clone(),
            config.bootstrap_core_token_actor.clone(),
            config.bootstrap_core_token_scopes.clone(),
            "bootstrap core token",
            "adopting the operator-supplied core token at init",
            "bootstrap_core_token_actor",
        ),
        Bootstrap::Agent => (
            config.bootstrap_agent_token_file.clone(),
            config.bootstrap_agent_token_actor.clone(),
            config.agent_session_scopes.clone(),
            "bootstrap agent token",
            "adopting the operator-supplied agent token at init",
            "bootstrap_agent_token_actor",
        ),
    };
    let Some(configured) = file else {
        return Ok("not_configured".to_string());
    };
    let secret = match std::fs::read_to_string(&configured) {
        Ok(text) => text.trim().to_string(),
        Err(_) => return Ok("not_configured".to_string()),
    };
    if secret.is_empty() {
        return Ok("not_configured".to_string());
    }
    let scopes = super::auth::scopes_of(&scope_text)?;
    let token_hash = crate::auth::adopt(&secret)
        .map_err(|error| VogtError::InvalidRequest(format!("bootstrap token: {error}")))?;
    let identity_ref = actor;
    let changed = crate::storage::sqlite::bootstrap_store::adopt_token(
        &declared_path(data_dir),
        &crate::storage::sqlite::bootstrap_store::AdoptRequest {
            token_hash: &token_hash,
            identity_ref: &identity_ref,
            name,
            reason,
            field,
            scopes: &scopes,
        },
        &mut || crate::application::instance::stamp(clock),
        &mut |prefix| next_id(ids, prefix),
    )?;
    Ok(if changed {
        "adopted"
    } else {
        "already_present"
    }
    .to_string())
}

/// The next id, from the caller's counter when there is one. Without a
/// counter the id is fresh, which is what a plain init does.
fn next_id(ids: &mut Option<SequentialIds>, prefix: &str) -> String {
    match ids {
        Some(factory) => factory.next(prefix),
        None => crate::core::fresh_id(prefix),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rusqlite::{params, Connection};
    use serde_json::{json, Map, Value};

    use crate::application::instance::{init_with, InitOutcome};
    use crate::storage::sqlite::{connection, declared_path, migrator};

    /// A fresh scratch directory for one test. Tests pass token paths through
    /// config overrides, never the process environment: the other tests in the
    /// binary run init too and would read a variable set here.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vogt-bootstrap-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn overrides(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn init(
        dir: &Path,
        config: &Map<String, Value>,
    ) -> Result<InitOutcome, migrator::MigrateError> {
        let mut clock = None;
        let mut ids = None;
        init_with(dir, config, &mut clock, &mut ids)
    }

    fn connect(dir: &Path) -> Connection {
        connection::connect(&declared_path(dir)).unwrap()
    }

    fn write(dir: &Path, name: &str, secret: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, secret).unwrap();
        path.display().to_string()
    }

    #[test]
    fn the_core_token_is_adopted_once_and_rotates_back() {
        let dir = scratch("core");
        let secret = "core-token-abcdef1234567890";
        let config = overrides(&[(
            "bootstrap_core_token_file",
            json!(write(&dir, "core-token", secret)),
        )]);

        let outcome = init(&dir, &config).expect("init adopts");
        assert!(outcome.created);
        assert_eq!(outcome.bootstrap_core_token, "adopted");
        assert_eq!(outcome.bootstrap_agent_token, "not_configured");
        let again = init(&dir, &config).expect("second init is quiet");
        assert!(!again.created);
        assert_eq!(again.bootstrap_core_token, "already_present");

        let conn = connect(&dir);
        let (name, identity): (String, String) = conn
            .query_row(
                "SELECT t.name, a.identity_ref FROM tokens t
                 JOIN actors a ON a.id = t.actor_id
                 WHERE t.token_hash = ?1 AND t.revoked_at IS NULL",
                params![crate::auth::hash_token(secret)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "bootstrap core token");
        assert_eq!(identity, "agent:vogt-engine");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tokens", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "a second init must not mint another token");

        // Rotate A -> B -> A. The revoked A row is unique on the hash, so
        // coming back to A reinstates it rather than inserting a second row.
        let secret_b = "core-token-bbbbbbbbbbbbbbbb";
        let path_b = write(&dir, "core-token", secret_b);
        init(&dir, &config).expect("rotation adopts B");
        assert_eq!(
            path_b,
            config["bootstrap_core_token_file"].as_str().unwrap()
        );
        write(&dir, "core-token", secret);
        init(&dir, &config).expect("rotate-back reinstates A");

        let live: String = conn
            .query_row(
                "SELECT token_hash FROM tokens WHERE revoked_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live, crate::auth::hash_token(secret), "A is live again");
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM tokens", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 2, "rotate-back reuses the revoked row");

        let digest: String = conn
            .query_row(
                "SELECT payload_digest FROM audit WHERE operation = 'token.adopt'
                 ORDER BY revision DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let superseded: String = conn
            .query_row(
                "SELECT id FROM tokens WHERE token_hash = ?1",
                params![crate::auth::hash_token(secret_b)],
                |row| row.get(0),
            )
            .unwrap();
        let expected = crate::decisions::digest_of(&json!({
            "adopted": true,
            "reason": "reinstated",
            "superseded": [superseded],
        }));
        assert_eq!(digest, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_agent_token_is_adopted_under_its_own_actor() {
        let dir = scratch("agent");
        let secret = "agent-token-abcdef1234567890";
        let config = overrides(&[(
            "bootstrap_agent_token_file",
            json!(write(&dir, "agent-token", secret)),
        )]);

        let outcome = init(&dir, &config).expect("init adopts");
        assert_eq!(outcome.bootstrap_agent_token, "adopted");
        assert_eq!(outcome.bootstrap_core_token, "not_configured");
        let again = init(&dir, &config).expect("second init is quiet");
        assert_eq!(again.bootstrap_agent_token, "already_present");

        let (name, identity): (String, String) = connect(&dir)
            .query_row(
                "SELECT t.name, a.identity_ref FROM tokens t
                 JOIN actors a ON a.id = t.actor_id
                 WHERE t.token_hash = ?1 AND t.revoked_at IS NULL",
                params![crate::auth::hash_token(secret)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "bootstrap agent token");
        assert_eq!(identity, "agent:vogt-sessions");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_agent_token_carries_agent_session_scopes() {
        let dir = scratch("agent-scopes");
        let secret = "agent-token-scopes-abcdef1234";
        let config = overrides(&[
            (
                "bootstrap_agent_token_file",
                json!(write(&dir, "agent-token", secret)),
            ),
            ("agent_session_scopes", json!("read,work.write")),
        ]);

        assert_eq!(
            init(&dir, &config)
                .expect("init adopts")
                .bootstrap_agent_token,
            "adopted"
        );
        let scopes: String = connect(&dir)
            .query_row(
                "SELECT scopes FROM tokens WHERE token_hash = ?1",
                params![crate::auth::hash_token(secret)],
                |row| row.get(0),
            )
            .unwrap();
        let mut held: Vec<String> = serde_json::from_str(&scopes).unwrap();
        held.sort();
        assert_eq!(held, vec!["read".to_string(), "work.write".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_agent_scope_fails_startup() {
        let dir = scratch("agent-bad-scope");
        let config = overrides(&[
            (
                "bootstrap_agent_token_file",
                json!(write(&dir, "agent-token", "agent-token-scopes-abcdef1234")),
            ),
            ("agent_session_scopes", json!("read,not-a-scope")),
        ]);

        let message = init(&dir, &config)
            .err()
            .expect("unknown scope refuses startup")
            .to_string();
        assert!(message.contains("not-a-scope"), "{message}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_secret_fails_startup_for_either_token() {
        for key in ["bootstrap_core_token_file", "bootstrap_agent_token_file"] {
            let dir = scratch("short");
            let config = overrides(&[(key, json!(write(&dir, "token", "changeme")))]);

            let message = init(&dir, &config)
                .err()
                .expect("short secret refuses startup")
                .to_string();
            assert!(message.contains("at least"), "{key}: {message}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn core_and_agent_tokens_coexist_in_one_init() {
        let dir = scratch("coexist");
        let config = overrides(&[
            (
                "bootstrap_core_token_file",
                json!(write(
                    &dir,
                    "core-token",
                    "vogt_bootstrap_core_secret_long_enough_0123"
                )),
            ),
            (
                "bootstrap_agent_token_file",
                json!(write(&dir, "agent-token", "agent-token-coexist-abcdef1234")),
            ),
        ]);

        let outcome = init(&dir, &config).expect("both adopt");
        assert_eq!(outcome.bootstrap_core_token, "adopted");
        assert_eq!(outcome.bootstrap_agent_token, "adopted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotating_back_reinstates_the_row_on_its_own_actor() {
        // The config names a different actor on the rotate-back. Python
        // reinstates the revoked row on the actor it was adopted under, and
        // creates nothing for the new name.
        let dir = scratch("rotate-actor");
        let secret_a = "core-token-abcdef1234567890";
        let secret_b = "core-token-bbbbbbbbbbbbbbbb";
        let path = dir.join("core-token");
        let config = |actor: &str| {
            overrides(&[
                (
                    "bootstrap_core_token_file",
                    json!(path.display().to_string()),
                ),
                ("bootstrap_core_token_actor", json!(actor)),
            ])
        };

        std::fs::write(&path, secret_a).unwrap();
        init(&dir, &config("agent:first")).expect("A adopts under agent:first");
        std::fs::write(&path, secret_b).unwrap();
        init(&dir, &config("agent:first")).expect("B rotates under agent:first");
        std::fs::write(&path, secret_a).unwrap();
        init(&dir, &config("agent:second")).expect("rotate-back to A under a new actor");

        let conn = connect(&dir);
        let holder: String = conn
            .query_row(
                "SELECT a.identity_ref FROM tokens t JOIN actors a ON a.id = t.actor_id
                 WHERE t.token_hash = ?1 AND t.revoked_at IS NULL",
                params![crate::auth::hash_token(secret_a)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            holder, "agent:first",
            "A stays on the actor it was adopted under"
        );
        let second: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM actors WHERE identity_ref = 'agent:second'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(second, 0, "the new configured name creates no actor");
        let live: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tokens WHERE revoked_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live, 1, "rotating back revokes B and leaves one live token");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
