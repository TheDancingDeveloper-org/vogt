//! Instance lifecycle. Ports `init_instance` in
//! `src/vogt/application/services/instance.py`.
//!
//! Creates the data directory and migrates both databases. Does not bootstrap
//! an actor or issue a token: those need the declared store, which is a later
//! chunk. The token file is written only when `VOGT_BOOTSTRAP_TOKEN` is set,
//! so a recorded run can place a known secret without this binary minting one.

use std::path::Path;

use crate::storage::sqlite::migrator::{self, Report};
use crate::storage::sqlite::{declared_path, observed_path};

pub struct InitOutcome {
    pub declared: Report,
    pub observed: Report,
    pub created: bool,
}

pub fn init(data_dir: &Path, now: &str) -> Result<InitOutcome, migrator::MigrateError> {
    std::fs::create_dir_all(data_dir)?;
    let root = migrator::migrations_root();
    let declared_existed = declared_path(data_dir).exists();
    let declared = migrator::open_and_migrate(
        &declared_path(data_dir),
        "declared",
        &root.join("declared"),
        "vogt-core",
        now,
    )?;
    let observed = migrator::open_and_migrate(
        &observed_path(data_dir),
        "observed",
        &root.join("observed"),
        "vogt-core",
        now,
    )?;
    if let Some(token) = std::env::var_os("VOGT_BOOTSTRAP_TOKEN") {
        let path = data_dir.join("token");
        if !path.exists() {
            std::fs::write(&path, token.as_encoded_bytes())?;
        }
    }
    Ok(InitOutcome {
        created: !declared_existed,
        declared,
        observed,
    })
}

/// Pending migrations on a database that already exists. Zero means a
/// Python-written database is already at this build's schema.
pub fn pending(data_dir: &Path) -> Result<(usize, usize), migrator::MigrateError> {
    let root = migrator::migrations_root();
    Ok((
        pending_in(&declared_path(data_dir), &root.join("declared"))?,
        pending_in(&observed_path(data_dir), &root.join("observed"))?,
    ))
}

fn pending_in(path: &Path, directory: &Path) -> Result<usize, migrator::MigrateError> {
    let conn = crate::storage::sqlite::connection::connect(path)?;
    let applied = migrator::applied_version(&conn)?;
    let bundled = migrator::bundled_version(directory)?;
    Ok(usize::try_from(bundled.saturating_sub(applied)).unwrap_or(0))
}
