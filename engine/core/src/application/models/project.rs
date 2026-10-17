//! The contract and scaffold operations' parameter and result models.
//!
//! Ports the `ContractAdopt*`, `ScaffoldProject*` classes in
//! `src/vogt/application/models.py`. `contract.decline` shares the adopt
//! models, as it does in Python.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::optional_date_time;

/// Opt a registered project into the contract.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractAdoptParams {
    /// A registered project slug.
    pub project: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractAdoptResult {
    pub project: String,
    pub adopted: bool,
    #[schemars(schema_with = "optional_date_time")]
    #[schemars(required)]
    pub adopted_at: Option<String>,
    /// What compliance reports for this project after the change.
    pub status: String,
    pub detail: String,
}

/// Lay the contract's skeleton into an already-registered project.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScaffoldProjectParams {
    /// A registered project slug.
    pub project: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScaffoldProjectResult {
    pub project: String,
    pub root_path: String,
    /// Paths written, relative to the project's root.
    pub created: Vec<String>,
    /// Paths already present and therefore left exactly as they were.
    pub skipped: Vec<String>,
    pub detail: String,
}
