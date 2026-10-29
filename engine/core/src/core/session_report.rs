//! A session's own short, typed report (WI-2039). Ports the matching
//! entities in `src/vogt/core/entities.py`.

#![allow(dead_code)] // Domain vocabulary the bin re-exports; tests and later services use what the bin does not.
use super::Moment;

/// The structured half of a session report (WI-2039). A field left unset was
/// not stated. The text is the reporting agent's: untrusted data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SessionReportFields {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub next_step: Option<String>,
}

/// A session's own short, typed report of where it is (WI-2039).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReport {
    pub id: String,
    pub actor_id: String,
    pub engine_session_id: Option<String>,
    pub work_item_id: Option<String>,
    pub kind: String,
    pub summary: String,
    pub fields: SessionReportFields,
    pub created_at: Moment,
}
