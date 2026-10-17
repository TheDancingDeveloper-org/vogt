//! First-run install mode. Ports `services/install.py`.
//!
//! Active exactly while the token store holds no rows, revoked included. The
//! zero-token check runs inside the write, so two racing bootstraps cannot
//! both succeed.

use super::auth::{password_hash_of, username_of};
use super::{now_of, write_context};
use crate::application::context::{AppContext, Built};
use crate::application::writes::{audited_write, WriteContext, WriteOutcome};
use crate::auth::{self, TOKEN_ENTROPY_BYTES};
use crate::core::{slugify, Actor, ActorKind, Clock, IdFactory, Moment, Token};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};
use crate::storage::sqlite::declared::SqliteDeclaredStore;
use serde_json::{json, Value};

const INSTALL_BOOTSTRAP: &str = "install.bootstrap";
const INSTALL_BOOTSTRAPPED_EVENT: &str = "install.bootstrapped";
const BOOTSTRAP_SCOPES: &[&str] = &["admin"];

const API_WARNING: &str = "This is the only time the secret is shown. It is not \
    stored and cannot be recovered — losing it means issuing another over the \
    loopback surface.";
const SESSION_WARNING: &str = "This is a browser session; sign in again with your \
    password when it expires. API tokens for agents come from `vogt token issue`.";

type SqliteWrite<'a, C, I> = WriteContext<'a, C, I, SqliteDeclaredStore<C, I>>;

/// `install.status`. Closed when the operator disabled the bootstrap, and
/// otherwise exactly while no token row exists.
pub fn install_status_op(ctx: &Built, _params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| {
        if !ctx.config.install_bootstrap_enabled {
            return Ok(json!({ "install_mode": false }));
        }
        let view = ctx.declared.read()?;
        Ok(json!({ "install_mode": !view.install_closed()? }))
    })
}

/// `install.bootstrap`. Names the first operator and mints the first token,
/// exactly once. With a password the token is a browser session and the
/// durable credential is the login; without one it is an admin API token
/// shown once.
pub fn install_bootstrap_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| install_bootstrap(ctx, &params))
}

struct BootstrapRequest {
    display_name: String,
    identity_ref: String,
    token_name: String,
    username: Option<String>,
    password_hash: Option<String>,
    now: Moment,
    expires_at: Option<Moment>,
    secret: String,
    token_hash: String,
}

fn install_bootstrap<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    if !ctx.config.install_bootstrap_enabled {
        return Err(VogtError::InstallClosed(
            "install mode is disabled on this instance (install_bootstrap_enabled=false): \
             create the first operator in the container with `vogt user create --scopes admin`, \
             not over this endpoint."
                .to_string(),
        ));
    }
    let display_name = params["display_name"].as_str().unwrap_or("").to_string();
    let identity_ref = match params.get("identity_ref").and_then(Value::as_str) {
        Some(given) => given.to_string(),
        None => derived_identity(&display_name)?,
    };
    let request = bootstrap_request(ctx, params, &display_name, &identity_ref)?;
    let mut writing = write_context(
        &ctx.declared,
        &ctx.principal,
        std::sync::Arc::clone(&ctx.clock),
        std::sync::Arc::clone(&ctx.id_factory),
    );
    // The write is attributed to the actor it creates: no other principal exists.
    writing.set_principal(&identity_ref, ActorKind::Human, &display_name);
    bootstrap_recorded(&mut writing, &identity_ref, request)
}

fn bootstrap_request<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
    display_name: &str,
    identity_ref: &str,
) -> Result<BootstrapRequest, VogtError> {
    let password = params.get("password").and_then(Value::as_str);
    let given_username = params.get("username").and_then(Value::as_str);
    let (username, password_hash) = match password {
        Some(password) => {
            let raw = given_username.unwrap_or(identity_ref.trim_start_matches("human:"));
            Some((username_of(raw)?, password_hash_of(password)?))
        }
        None if given_username.is_some() => {
            return Err(VogtError::InvalidRequest(
                "a username needs a password to go with it".to_string(),
            ))
        }
        None => None,
    }
    .map_or((None, None), |(username, hash)| {
        (Some(username), Some(hash))
    });
    let now = now_of(&ctx.clock);
    let (secret, token_hash) = auth::issue(&super::auth::random_array::<TOKEN_ENTROPY_BYTES>()?);
    let with_password = password_hash.is_some();
    Ok(BootstrapRequest {
        display_name: display_name.to_string(),
        identity_ref: identity_ref.to_string(),
        token_name: params["token_name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or("first-run browser token")
            .to_string(),
        username,
        password_hash,
        now,
        expires_at: with_password.then_some(now.plus_days(ctx.config.session_ttl_days)),
        secret,
        token_hash,
    })
}

fn derived_identity(display_name: &str) -> Result<String, VogtError> {
    let slug = slugify(display_name);
    if slug.is_empty() {
        return Err(VogtError::InvalidRequest(format!(
            "cannot derive an identity from display name {}",
            crate::core::py_repr(display_name)
        )));
    }
    Ok(format!("human:{slug}"))
}

fn bootstrap_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut SqliteWrite<'_, C, I>,
    identity_ref: &str,
    request: BootstrapRequest,
) -> Result<Value, VogtError> {
    let ids = std::sync::Arc::clone(write.ids());
    audited_write(
        write,
        INSTALL_BOOTSTRAP,
        &format!("first-run install bootstrap for {identity_ref}"),
        move |txn: &mut _, actor: &Actor| {
            if txn.install_closed()? {
                return Err(VogtError::InstallClosed(
                    "install mode is closed: this instance already has an operator. Sign in, \
                     or create another login over the loopback surface with `vogt user create`."
                        .to_string(),
                ));
            }
            if let (Some(username), Some(password_hash)) =
                (&request.username, &request.password_hash)
            {
                let scopes = vec!["admin".to_string()];
                txn.upsert_password_credential(
                    &actor.id,
                    username,
                    password_hash,
                    &scopes,
                    request.now,
                )?;
            }
            let token = Token {
                id: ids.lock().expect("the id lock").next("tok"),
                actor_id: actor.id.clone(),
                actor_identity_ref: Some(actor.identity_ref.clone()),
                name: request.token_name.clone(),
                scopes: BOOTSTRAP_SCOPES
                    .iter()
                    .map(|scope| (*scope).to_string())
                    .collect(),
                kind: if request.password_hash.is_none() {
                    crate::core::TokenKind::Api
                } else {
                    crate::core::TokenKind::Session
                },
                created_at: request.now,
                expires_at: request.expires_at,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            };
            txn.insert_token(&token, &request.token_hash)?;
            let warning = if request.password_hash.is_none() {
                API_WARNING
            } else {
                SESSION_WARNING
            };
            let kind = match token.kind {
                crate::core::TokenKind::Api => "api",
                crate::core::TokenKind::Session => "session",
                crate::core::TokenKind::Agent => "agent",
            };
            Ok(WriteOutcome::new(
                json!({
                    "actor": actor,
                    "token": token,
                    "secret": request.secret,
                    "warning": warning,
                    "username": request.username,
                }),
                "token",
                &token.id,
                json!({
                    "actor": actor.identity_ref,
                    "scopes": BOOTSTRAP_SCOPES,
                    "name": token.name,
                    "kind": kind,
                    "username": request.username,
                    "source": "first-run install bootstrap",
                }),
                INSTALL_BOOTSTRAPPED_EVENT,
                json!({ "actor": actor.identity_ref, "scopes": BOOTSTRAP_SCOPES }),
            ))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::interface::AuditQuery;

    fn fresh(name: &str) -> (std::path::PathBuf, crate::application::context::Built) {
        let dir = std::env::temp_dir().join(format!("vogt-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir.clone(),
            ..crate::config::VogtConfig::default()
        };
        let built = crate::application::context::build_context(
            config, None, None, None, None, None, None, None,
        )
        .unwrap();
        (dir, built)
    }

    /// A fresh store is open, the bootstrap mints a session and a login, and a
    /// second bootstrap finds the door shut.
    #[test]
    fn the_first_bootstrap_closes_the_door() {
        let (dir, built) = fresh("once");
        let status = install_status_op(&built, json!({})).unwrap();
        assert_eq!(status["install_mode"], true);
        let minted = install_bootstrap_op(
            &built,
            json!({
                "display_name": "Ada Lovelace",
                "password": "correct horse battery",
            }),
        )
        .unwrap();
        assert_eq!(minted["actor"]["identity_ref"], "human:ada-lovelace");
        assert_eq!(minted["token"]["kind"], "session");
        // install.bootstrap has no recorded schema, so the validator fills
        // nothing and the service supplies the default itself.
        assert_eq!(minted["token"]["name"], "first-run browser token");
        assert_eq!(minted["username"], "ada-lovelace");
        assert!(minted["secret"].as_str().unwrap().starts_with("vogt_"));
        assert!(minted["token"]["expires_at"].is_string());
        let closed = install_status_op(&built, json!({})).unwrap();
        assert_eq!(closed["install_mode"], false);
        let again = install_bootstrap_op(
            &built,
            json!({"display_name": "Ada Lovelace", "password": "another passphrase"}),
        )
        .unwrap_err();
        assert!(matches!(again, VogtError::InstallClosed(_)), "{again}");
        assert!(
            again.message().contains("install mode is closed"),
            "{again}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No password means an API token shown once, and a username without a
    /// password is refused before anything is written.
    #[test]
    fn a_headless_bootstrap_mints_an_api_token() {
        let (dir, built) = fresh("headless");
        let refused = install_bootstrap_op(
            &built,
            json!({"display_name": "Ada", "username": "ada", "token_name": "cli"}),
        )
        .unwrap_err();
        assert!(
            refused.message().contains("a username needs a password"),
            "{refused}"
        );
        let still_open = install_status_op(&built, json!({})).unwrap();
        assert_eq!(still_open["install_mode"], true);
        let minted =
            install_bootstrap_op(&built, json!({"display_name": "Ada", "token_name": "cli"}))
                .unwrap();
        assert_eq!(minted["token"]["kind"], "api");
        assert!(minted["token"]["expires_at"].is_null());
        assert!(minted["username"].is_null());
        assert!(minted["warning"].as_str().unwrap().contains("only time"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A password with no token is enough to close the door, and so is a revoked
    /// token bound to a person. An agent token on its own leaves it open, which
    /// is how a fresh stack looks after adopting the bootstrap core token.
    #[test]
    fn the_door_follows_people_not_tokens() {
        let (dir, built) = fresh("door");
        seed(
            &dir,
            "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at) VALUES ('act_agent', 'agent', 'core', 'agent:core', 0, '2026-01-01T00:00:00Z')",
        );
        seed(
            &dir,
            "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, kind, created_at) VALUES ('tok_agent', 'act_agent', 'core', 'hash', 'admin', 'api', '2026-01-01T00:00:00Z')",
        );
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            true
        );

        seed(
            &dir,
            "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at) VALUES ('act_ada', 'human', 'Ada', 'human:ada', 0, '2026-01-01T00:00:00Z')",
        );
        seed(
            &dir,
            "INSERT INTO password_credentials (actor_id, username, password_hash, scopes, created_at, updated_at) VALUES ('act_ada', 'ada', 'scrypt$0$0$0$$', 'admin', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        );
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            false
        );
        let refused =
            install_bootstrap_op(&built, json!({"display_name": "Ada", "token_name": "x"}))
                .unwrap_err();
        assert!(
            matches!(
                &refused,
                VogtError::InstallClosed(message)
                    if message == "install mode is closed: this instance already has an operator. \
                        Sign in, or create another login over the loopback surface with \
                        `vogt user create`."
            ),
            "{refused}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An operator who has turned the bootstrap off gets the configured-off
    /// answer, not the already-has-an-operator one.
    #[test]
    fn a_disabled_bootstrap_says_it_is_disabled() {
        let dir = std::env::temp_dir().join(format!("vogt-install-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = crate::application::context::build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                install_bootstrap_enabled: false,
                ..crate::config::VogtConfig::default()
            },
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let refused =
            install_bootstrap_op(&built, json!({"display_name": "Ada", "token_name": "x"}))
                .unwrap_err();
        assert!(
            matches!(
                &refused,
                VogtError::InstallClosed(message)
                    if message == "install mode is disabled on this instance \
                        (install_bootstrap_enabled=false): create the first operator in the \
                        container with `vogt user create --scopes admin`, not over this endpoint."
            ),
            "{refused}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The latch alone closes the door, and a revoked token bound to a person
    /// closes it even with no live credential.
    #[test]
    fn a_latch_or_a_revoked_person_token_closes_the_door() {
        let (dir, built) = fresh("latch");
        seed(
            &dir,
            "INSERT INTO install_latch (id, closed_at, reason) VALUES (1, '2026-01-01T00:00:00Z', 'set')",
        );
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            false
        );
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, built) = fresh("revoked");
        seed(
            &dir,
            "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at) VALUES ('act_ada', 'human', 'Ada', 'human:ada', 0, '2026-01-01T00:00:00Z')",
        );
        seed(
            &dir,
            "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, kind, created_at) VALUES ('tok_old', 'act_ada', 'old', 'hash', 'admin', 'api', '2026-01-01T00:00:00Z')",
        );
        seed(
            &dir,
            "UPDATE tokens SET revoked_at = '2026-01-02T00:00:00Z', revoked_reason = 'retired' WHERE id = 'tok_old'",
        );
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            false
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two bootstraps at once: the write checks the door inside the transaction,
    /// so exactly one mints a token.
    #[test]
    fn two_bootstraps_at_once_mint_one_token() {
        let (dir, _) = fresh("race");
        let mut handles = Vec::new();
        for _ in 0..2 {
            let dir = dir.clone();
            handles.push(std::thread::spawn(move || {
                let built = open(&dir);
                install_bootstrap_op(
                    &built,
                    json!({"display_name": "Ada", "password": "correct horse battery", "token_name": "browser"}),
                )
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let won = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(won, 1, "{results:?}");
        assert_eq!(
            install_status_op(&open(&dir), json!({})).unwrap()["install_mode"],
            false
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn open(dir: &std::path::Path) -> crate::application::context::Built {
        crate::application::context::build_context(
            crate::config::VogtConfig {
                data_dir: dir.to_path_buf(),
                ..crate::config::VogtConfig::default()
            },
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    const PASSWORD: &str = "correct horse battery";
    /// The stack secret a Docker quick start writes to deploy/vogt-core-token.
    const CORE_SECRET: &str = "vogt_stack_secret_that_is_long_enough_0123456789";
    /// The migration that latches install mode closed, named rather than
    /// derived so the upgrade test keeps testing that upgrade.
    const LATCH_MIGRATION: i64 = 20;

    fn start() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn actor_refs(built: &Built) -> Vec<String> {
        let view = crate::with_ctx!(built, |ctx| ctx.declared.read()).unwrap();
        view.list_actors(100, 0)
            .unwrap()
            .into_iter()
            .map(|actor| actor.identity_ref)
            .collect()
    }

    /// The audit rows as (operation, actor) pairs, newest first.
    fn audit_pairs(built: &Built) -> Vec<(String, String)> {
        let view = crate::with_ctx!(built, |ctx| ctx.declared.read()).unwrap();
        view.list_audit(&AuditQuery {
            limit: 50,
            offset: 0,
            actor_id: None,
            operation: None,
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap()
        .into_iter()
        .map(|row| (row.operation, row.actor_identity_ref))
        .collect()
    }

    /// A declared store migrated with every shipped migration before the
    /// latch, the way a v0.7.7 instance's store is. Returns its path.
    fn store_before_the_latch(dir: &std::path::Path) -> std::path::PathBuf {
        let old = dir.join("before-latch");
        std::fs::create_dir_all(&old).unwrap();
        for migration in
            crate::storage::sqlite::migrator::load_migrations("declared", None).unwrap()
        {
            if migration.number() < LATCH_MIGRATION {
                std::fs::write(old.join(format!("{}.sql", migration.id)), &migration.sql).unwrap();
            }
        }
        let path = dir.join("declared.sqlite3");
        let mut conn = crate::storage::sqlite::connection::connect(&path).unwrap();
        crate::storage::sqlite::migrator::migrate(
            &mut conn,
            "declared",
            Some(&old),
            "old/1",
            "2026-09-01T00:00:00+00:00",
        )
        .unwrap();
        path
    }

    /// The gate's answer for a bearer secret, as HTTP asks it.
    fn bearer(
        built: &Built,
        secret: &str,
    ) -> Result<crate::adapters::auth_gate::Grant, crate::adapters::auth_gate::Denial> {
        use crate::adapters::auth_gate;
        let registry = crate::registry::default_registry();
        let operation = registry.get("auth.whoami").unwrap();
        crate::with_ctx!(built, |ctx| auth_gate::authenticate(
            &ctx.declared,
            auth_gate::Request {
                operation,
                transport: crate::registry::Transport::Http,
                presented: Some(secret),
                no_auth: false,
                writes_enabled: true,
                now: start(),
            },
            ctx.config.session_ttl_days,
        ))
    }

    /// A failed bootstrap writes nothing: the actor it would have registered
    /// is rolled back with the refused write.
    #[test]
    fn a_failed_bootstrap_leaves_no_actor_behind() {
        use crate::application::services::auth::create_user_op;
        let (dir, built) = fresh("no-actor");
        create_user_op(
            &built,
            json!({"username": "ada", "password": PASSWORD, "scopes": "admin", "reason": "first"}),
        )
        .unwrap();
        let refused = install_bootstrap_op(&built, json!({"display_name": "Eve"})).unwrap_err();
        assert!(matches!(refused, VogtError::InstallClosed(_)), "{refused}");
        assert!(!actor_refs(&built).contains(&"human:eve".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A login made with a password closes the door the same way a token does.
    #[test]
    fn a_password_login_closes_install_mode() {
        use crate::application::services::auth::create_user_op;
        let (dir, built) = fresh("login-closes");
        create_user_op(
            &built,
            json!({"username": "ada", "password": PASSWORD, "scopes": "admin", "reason": "first"}),
        )
        .unwrap();
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            false
        );
        let refused = install_bootstrap_op(&built, json!({"display_name": "Eve"})).unwrap_err();
        assert!(matches!(refused, VogtError::InstallClosed(_)), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_bootstrap_is_audited_and_attributed_to_the_new_actor() {
        let (dir, built) = fresh("audit");
        install_bootstrap_op(&built, json!({"display_name": "Ada"})).unwrap();
        let rows = audit_pairs(&built);
        assert!(
            rows.contains(&("install.bootstrap".to_string(), "human:ada".to_string())),
            "{rows:?}"
        );
        assert!(
            rows.contains(&("actor.auto_register".to_string(), "human:ada".to_string())),
            "{rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_explicit_identity_ref_is_honoured() {
        let (dir, built) = fresh("explicit");
        let minted = install_bootstrap_op(
            &built,
            json!({"display_name": "Ada", "identity_ref": "human:ada.l"}),
        )
        .unwrap();
        assert_eq!(minted["actor"]["identity_ref"], "human:ada.l");
        assert!(actor_refs(&built).contains(&"human:ada.l".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unsluggable_display_name_is_refused() {
        let (dir, built) = fresh("unsluggable");
        let refused = install_bootstrap_op(&built, json!({"display_name": "???"})).unwrap_err();
        assert!(
            matches!(&refused, VogtError::InvalidRequest(message) if message.contains("cannot derive an identity")),
            "{refused}"
        );
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            true
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The core token the Docker quick start adopts is an agent's, so it leaves
    /// the door open, and the engine's own call still authenticates with it.
    /// The row is seeded in the shape `init` writes, because adoption reads its
    /// file through the environment, which a unit test should not set.
    #[test]
    fn the_adopted_core_token_leaves_install_mode_open() {
        let (dir, built) = fresh("core");
        seed(
            &dir,
            "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at) VALUES ('act_engine', 'agent', 'agent:vogt-engine', 'agent:vogt-engine', 0, '2026-01-01T00:00:00Z')",
        );
        let hash = crate::auth::hash_token(CORE_SECRET);
        seed(
            &dir,
            &format!(
                "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, kind, created_at) VALUES ('tok_core', 'act_engine', 'bootstrap-core-token', '{hash}', '[\"project.write\",\"read\",\"work.write\"]', 'api', '2026-01-01T00:00:00Z')"
            ),
        );
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            true
        );
        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        assert!(view.list_password_credentials().unwrap().is_empty());
        drop(view);
        let grant = bearer(&built, CORE_SECRET).unwrap();
        assert_eq!(grant.identity_ref.as_deref(), Some("agent:vogt-engine"));
        assert_eq!(grant.kind, crate::core::ActorKind::Agent);
        let mut scopes = grant.scopes.clone();
        scopes.sort();
        assert_eq!(scopes, vec!["project.write", "read", "work.write"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An upgraded store that held only agent-bound tokens, and no person's, is
    /// latched closed by migration 0020 rather than reopened by the upgrade.
    #[test]
    fn an_upgraded_instance_operated_without_a_person_stays_closed() {
        let dir = std::env::temp_dir().join(format!("vogt-install-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = store_before_the_latch(&dir);
        let stamp = "2026-09-01T00:00:00+00:00";
        {
            let conn = crate::storage::sqlite::connection::connect(&path).unwrap();
            conn.execute(
                "INSERT INTO actors (id, kind, display_name, identity_ref, created_at) \
                 VALUES ('act_engine', 'agent', 'agent:vogt-engine', 'agent:vogt-engine', ?1)",
                [stamp],
            )
            .unwrap();
            for (token_id, name, kind) in [
                ("tok_core", "bootstrap-core-token", "api"),
                ("tok_session", "session ses_1", "agent"),
            ] {
                conn.execute(
                    "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, kind, created_at) \
                     VALUES (?1, 'act_engine', ?2, ?3, '[\"read\"]', ?4, ?5)",
                    rusqlite::params![token_id, name, format!("hash-{token_id}"), kind, stamp],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects (id, slug, name, root_path, lifecycle_state, \
                 compliance_status, exclusions, trust_state, created_at, updated_at) \
                 VALUES ('prj_1', 'months-of-work', 'Months of work', '/srv/w', 'active', \
                 'not_checked', '[]', 'unverified', ?1, ?1)",
                [stamp],
            )
            .unwrap();
        }
        let store = crate::storage::sqlite::declared::SqliteDeclaredStore::new(
            path,
            crate::core::StepClock::new(start()),
            crate::application::context::RandomIds,
        );
        store
            .bootstrap(&crate::core::local_principal("tester"))
            .unwrap();
        let report = store.migrate().unwrap();
        assert!(
            report.applied.contains(&"0020_install_latch".to_string()),
            "{report:?}"
        );
        let view = store.read().unwrap();
        assert!(view.install_closed().unwrap());
        drop(view);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The latch copies the old rule exactly: a store that held no token at all
    /// was in install mode before the upgrade and still is.
    #[test]
    fn an_upgraded_store_with_no_tokens_is_still_a_fresh_install() {
        let dir = std::env::temp_dir().join(format!("vogt-install-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = store_before_the_latch(&dir);
        let store = crate::storage::sqlite::declared::SqliteDeclaredStore::new(
            path,
            crate::core::StepClock::new(start()),
            crate::application::context::RandomIds,
        );
        store
            .bootstrap(&crate::core::local_principal("tester"))
            .unwrap();
        store.migrate().unwrap();
        let view = store.read().unwrap();
        assert!(!view.install_closed().unwrap());
        drop(view);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A login removed after it was made still leaves the door shut: the latch
    /// was set when the person was given credentials, and removal does not undo it.
    #[test]
    fn removing_the_only_operator_does_not_reopen_install_mode() {
        use crate::application::services::auth::{create_user_op, remove_user_op};
        let (dir, built) = fresh("removed");
        create_user_op(
            &built,
            json!({"username": "adda", "password": PASSWORD, "scopes": "admin", "reason": "first"}),
        )
        .unwrap();
        remove_user_op(&built, json!({"username": "adda", "reason": "typo"})).unwrap();
        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        assert!(view.list_password_credentials().unwrap().is_empty());
        drop(view);
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            false
        );
        let refused = install_bootstrap_op(&built, json!({"display_name": "Eve"})).unwrap_err();
        assert!(matches!(refused, VogtError::InstallClosed(_)), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With a username and password the bootstrap creates a login, returns a
    /// browser session, and the password signs in as the same actor.
    #[test]
    fn the_bootstrap_can_create_a_login_and_hands_back_a_session() {
        use crate::application::services::auth::login_op;
        let _gate = crate::application::services::auth::throttle_gate()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::application::services::auth::throttle_reset();
        let (dir, built) = fresh("login-bootstrap");
        let minted = install_bootstrap_op(
            &built,
            json!({"display_name": "Ada Lovelace", "password": PASSWORD}),
        )
        .unwrap();
        assert_eq!(minted["username"], "ada-lovelace");
        assert_eq!(minted["token"]["kind"], "session");
        assert!(!minted["token"]["expires_at"].is_null());
        assert_eq!(minted["token"]["scopes"], json!(["admin"]));
        let signed_in = login_op(
            &built,
            json!({"username": "ada-lovelace", "password": PASSWORD}),
        )
        .unwrap();
        assert_eq!(signed_in["actor"]["id"], minted["actor"]["id"]);
        assert_eq!(signed_in["token"]["scopes"], json!(["admin"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A username with no password is refused before anything is written.
    #[test]
    fn the_bootstrap_refuses_a_username_without_a_password() {
        let (dir, built) = fresh("username-only");
        let refused =
            install_bootstrap_op(&built, json!({"display_name": "Ada", "username": "ada"}))
                .unwrap_err();
        assert!(
            matches!(&refused, VogtError::InvalidRequest(message) if message.contains("password")),
            "{refused}"
        );
        assert!(!actor_refs(&built).contains(&"human:ada".to_string()));
        assert_eq!(
            install_status_op(&built, json!({})).unwrap()["install_mode"],
            true
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn seed(dir: &std::path::Path, sql: &str) {
        let conn = rusqlite::Connection::open(dir.join("declared.sqlite3")).unwrap();
        conn.execute(sql, []).unwrap();
    }
}
