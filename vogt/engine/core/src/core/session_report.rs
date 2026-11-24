//! A session's own short, typed report (WI-2039). Ports the matching
//! entities in `src/vogt/core/entities.py`.

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
    /// Accepted from stored reports and never written back: a report written
    /// with an idempotency key must still read, and new reports do not carry one.
    #[serde(default, skip_serializing)]
    pub idempotency_key: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::SessionReportFields;

    #[test]
    fn a_stored_report_with_an_idempotency_key_still_reads() {
        // Reports written with an idempotency key carry it in their stored
        // `data` JSON. It must read (the brief and the Inbox read these rows),
        // and it is not written back out.
        let stored = r#"{"branch":"backlog-1","sha":"abc123","idempotency_key":"rpt-key-1","checks":["fmt"]}"#;
        let fields: SessionReportFields = serde_json::from_str(stored).unwrap();
        assert_eq!(fields.idempotency_key.as_deref(), Some("rpt-key-1"));
        assert_eq!(fields.branch.as_deref(), Some("backlog-1"));
        assert_eq!(fields.checks, vec!["fmt".to_string()]);

        let written = serde_json::to_value(&fields).unwrap();
        assert!(written.get("idempotency_key").is_none(), "{written}");
    }

    #[test]
    fn an_unknown_field_other_than_the_idempotency_key_is_still_refused() {
        let stored = r#"{"branch":"x","surprise":true}"#;
        assert!(serde_json::from_str::<SessionReportFields>(stored).is_err());
    }
}
