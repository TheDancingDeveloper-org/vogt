//! Bootstrap token adoption writes (moved from `application::services::bootstrap`,
//! WI-4633). Same statements, one `BEGIN IMMEDIATE` transaction, same error mapping.

use std::path::Path;

use rusqlite::params;

use crate::errors::VogtError;

/// Everything the adoption decides on, resolved by the caller.
pub struct AdoptRequest<'a> {
    pub token_hash: &'a str,
    pub identity_ref: &'a str,
    pub name: &'a str,
    pub reason: &'a str,
    pub field: &'a str,
    pub scopes: &'a [String],
}

/// Adopt one token; `Ok(true)` when a row changed, `Ok(false)` when already live.
/// `clock` and `ids` are called where the original code read them.
pub fn adopt_token(
    path: &Path,
    req: &AdoptRequest<'_>,
    clock: &mut dyn FnMut() -> String,
    ids: &mut dyn FnMut(&str) -> String,
) -> Result<bool, VogtError> {
    let AdoptRequest {
        token_hash,
        identity_ref,
        name,
        reason,
        field,
        scopes,
    } = *req;
    let conn = crate::storage::sqlite::connection::connect(path).map_err(sql_error)?;

    // Everything that decides the row lives inside the transaction. The
    // revision and the actor are read there too: a read taken before
    // BEGIN IMMEDIATE can lose to the other container of the same stack.
    conn.execute("BEGIN IMMEDIATE", []).map_err(sql_error)?;
    let written = (|| -> Result<bool, rusqlite::Error> {
        let live: Option<String> = conn
            .query_row(
                "SELECT id FROM tokens WHERE token_hash = ?1 AND revoked_at IS NULL",
                params![token_hash],
                |row| row.get(0),
            )
            .ok();
        if live.is_some() {
            return Ok(false);
        }

        // The clock is read where Python reads it, one step each, so the
        // stamps and the revocation's `revoked_at` match row for row. Each
        // audited write also records the caller's principal, not the actor the
        // token is bound to.
        //
        // The secret rotated back to a value a rotation revoked. Reinstate that
        // row on its own actor, as Python does, whichever actor the config
        // names now. Revoke whatever holds the row's name on that actor.
        let revoked: Option<(String, String, String)> = conn
            .query_row(
                "SELECT id, actor_id, name FROM tokens WHERE token_hash = ?1",
                params![token_hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();
        if let Some((existing_id, existing_actor, existing_name)) = revoked {
            let revoke_at = clock();
            let superseded = revoke_named(&conn, &existing_actor, &existing_name, &revoke_at)?;
            conn.execute(
                "UPDATE tokens SET revoked_at = NULL, revoked_reason = NULL WHERE id = ?1",
                params![existing_id],
            )?;
            let payload = serde_json::json!({
                "adopted": true,
                "reason": "reinstated",
                "superseded": superseded,
            });
            let holder_ref: String = conn.query_row(
                "SELECT identity_ref FROM actors WHERE id = ?1",
                params![existing_actor],
                |row| row.get(0),
            )?;
            let summary = crate::decisions::python_json_dumps(
                &serde_json::json!({"actor": holder_ref}),
                false,
            );
            let now = clock();
            let principal = principal_actor(&conn)?;
            record_adoption(
                &conn,
                ids,
                &now,
                &principal,
                &existing_id,
                reason,
                &payload,
                &summary,
            )?;
            return Ok(true);
        }

        // Python refuses `kind != "agent"` on the fresh-insert path only. A
        // missing actor is created as an agent below.
        let holder: Option<(String, String)> = conn
            .query_row(
                "SELECT id, kind FROM actors WHERE identity_ref = ?1",
                params![identity_ref],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        if let Some((_, kind)) = &holder {
            if kind != "agent" {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "{field} {identity_ref:?} is a person ({kind}); \
                     the token must be bound to an agent actor \
                     (for example agent:vogt-engine)"
                )));
            }
        }
        let actor_id = match &holder {
            Some((id, _)) => id.clone(),
            None => {
                let id = ids("act");
                let created_at = clock();
                conn.execute(
                    "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at)
                     VALUES (?1, 'agent', ?2, ?3, 0, ?4)",
                    params![id, identity_ref, identity_ref, created_at],
                )?;
                id
            }
        };

        let revoke_at = clock();
        let superseded = revoke_named(&conn, &actor_id, name, &revoke_at)?;
        let token_id = ids("tok");
        let created_at = clock();
        let scopes_json = crate::decisions::python_json_dumps(&serde_json::json!(scopes), false);
        conn.execute(
            "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                token_id,
                actor_id,
                name,
                token_hash,
                scopes_json,
                created_at
            ],
        )?;
        // Python's fresh payload is {actor, scopes, name, source, superseded}.
        // The digest is over that object, so the keys have to match.
        let payload = serde_json::json!({
            "actor": identity_ref,
            "scopes": scopes,
            "name": name,
            "source": "operator-supplied",
            "superseded": superseded,
        });
        let summary = crate::decisions::python_json_dumps(
            &serde_json::json!({"actor": identity_ref, "scopes": scopes}),
            false,
        );
        let now = clock();
        let principal = principal_actor(&conn)?;
        record_adoption(
            &conn, ids, &now, &principal, &token_id, reason, &payload, &summary,
        )?;
        Ok(true)
    })();
    match written {
        Ok(changed) => {
            conn.execute("COMMIT", []).map(|_| ()).map_err(sql_error)?;
            Ok(changed)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(VogtError::InvalidRequest(format!("adopt: {err}")))
        }
    }
}

/// The row of the principal `init` runs as, the actor Python's audited writes
/// record. `bootstrap` creates it from the OS user.
fn principal_actor(conn: &rusqlite::Connection) -> Result<String, rusqlite::Error> {
    let identity = crate::core::local_principal(&crate::core::os_user()).identity_ref;
    conn.query_row(
        "SELECT id FROM actors WHERE identity_ref = ?1",
        params![identity],
        |row| row.get(0),
    )
}

/// Revoke the live tokens `actor_id` holds under `name`, and return their ids.
fn revoke_named(
    conn: &rusqlite::Connection,
    actor_id: &str,
    name: &str,
    at: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id FROM tokens WHERE actor_id = ?1 AND name = ?2 AND revoked_at IS NULL",
    )?;
    let ids_to_revoke: Vec<String> = stmt
        .query_map(params![actor_id, name], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for id in &ids_to_revoke {
        conn.execute(
            "UPDATE tokens SET revoked_at = ?1,
                    revoked_reason = 'superseded by a rotated bootstrap secret'
             WHERE id = ?2 AND revoked_at IS NULL",
            params![at, id],
        )?;
    }
    Ok(ids_to_revoke)
}

/// Write the `token.adopt` audit row, its revision bump and the event.
#[allow(clippy::too_many_arguments)]
fn record_adoption(
    conn: &rusqlite::Connection,
    ids: &mut dyn FnMut(&str) -> String,
    at: &str,
    actor_id: &str,
    entity_id: &str,
    reason: &str,
    payload: &serde_json::Value,
    summary: &str,
) -> Result<(), rusqlite::Error> {
    let revision: i64 = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'revision'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0)
        + 1;
    let digest = crate::decisions::digest_of(payload);
    let audit_id = ids("aud");
    let txn_id = ids("txn");
    conn.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'revision'",
        params![revision.to_string()],
    )?;
    conn.execute(
        "INSERT INTO audit (id, txn_id, revision, actor_id, operation, entity_kind,
                            entity_id, reason, payload_digest, at)
         VALUES (?1, ?2, ?3, ?4, 'token.adopt', 'token', ?5, ?6, ?7, ?8)",
        params![audit_id, txn_id, revision, actor_id, entity_id, reason, digest, at],
    )?;
    conn.execute(
        "INSERT INTO events (kind, entity_kind, entity_id, actor_id, audit_id, summary, at)
         VALUES ('token.adopted', 'token', ?1, ?2, ?3, ?4, ?5)",
        params![entity_id, actor_id, audit_id, summary, at],
    )?;
    Ok(())
}

fn sql_error(err: rusqlite::Error) -> VogtError {
    VogtError::InvalidRequest(format!("bootstrap token: {err}"))
}
