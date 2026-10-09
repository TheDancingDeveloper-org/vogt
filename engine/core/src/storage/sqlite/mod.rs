//! SQLite storage. Ports `src/vogt/storage/sqlite/`.

pub mod connection;
pub mod embedded;
pub mod migrator;

use std::path::{Path, PathBuf};

/// `config.DECLARED_DB_NAME`.
pub const DECLARED_DB_NAME: &str = "declared.sqlite3";
/// `config.OBSERVED_DB_NAME`.
pub const OBSERVED_DB_NAME: &str = "observed.sqlite3";

pub fn declared_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DECLARED_DB_NAME)
}

pub fn observed_path(data_dir: &Path) -> PathBuf {
    data_dir.join(OBSERVED_DB_NAME)
}

pub mod declared;
