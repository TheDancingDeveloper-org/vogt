//! The `deploy-lanes` collector: which revision each configured lane runs.
//! Ports `adapters/forge/lanes.py`.
//!
//! "Is dev at head?" had no single answer source. Each lane in `deploy_lanes`
//! names where its evidence lives — a receipt a deploy pipeline commits to a
//! forge repository, and/or the running instance's own public version endpoint
//! — and this collector reads both, then asks the project's forge how the
//! deployed revision compares with the branch the lane tracks. The result is
//! one observation per lane (`deploy.lane`).
//!
//! Configuration only: no hostname, repository or path is known here. A lane
//! whose source cannot be read records *why* in its observation rather than
//! failing the sweep, so "not readable" never reads as "not deployed".

use serde_json::{Map, Value};

use super::kinds::{COLLECTOR_DEPLOY_LANES, KIND_DEPLOY_LANE};
use super::sync::{finding, ForgeDirectory};
use crate::config::{DeployLane, VogtConfig};
use crate::core::Project;
use crate::errors::VogtError;

/// How many unpromoted commits a lane observation carries. The count is always
/// exact (`ahead_by`); the list is for reading.
const MAX_COMMITS: usize = 100;

const PASSED: [&str; 5] = ["passed", "success", "succeeded", "ok", "green"];
const FAILED: [&str; 5] = ["failed", "failure", "error", "errored", "red"];

/// One GET against a version endpoint. Tests record answers; the real fetch is
/// the caller's, because this layer does not open sockets of its own.
pub trait VersionFetch {
    fn get(&self, url: &str) -> Result<(u16, Vec<u8>), String>;
}

/// One observation per configured deployment lane of a project.
pub struct DeployLanesCollector<'a> {
    directory: &'a dyn ForgeDirectory,
    versions: &'a dyn VersionFetch,
}

impl<'a> DeployLanesCollector<'a> {
    pub fn new(directory: &'a dyn ForgeDirectory, versions: &'a dyn VersionFetch) -> Self {
        Self {
            directory,
            versions,
        }
    }

    pub fn name(&self) -> &'static str {
        COLLECTOR_DEPLOY_LANES
    }

    pub fn requires_network(&self) -> bool {
        true
    }

    pub fn collect(
        &self,
        config: &VogtConfig,
        project: &Project,
    ) -> Vec<crate::storage::observed_types::PendingObservation> {
        config
            .deploy_lanes
            .iter()
            .filter(|lane| lane.project == project.slug)
            .map(|lane| {
                finding(
                    KIND_DEPLOY_LANE,
                    format!("deploy:{}/{}", project.slug, lane.name),
                    Value::Object(self.observe(config, project, lane)),
                    Some(&project.id),
                    lane.version_url.clone(),
                    false,
                )
            })
            .collect()
    }

    fn observe(
        &self,
        config: &VogtConfig,
        project: &Project,
        lane: &DeployLane,
    ) -> Map<String, Value> {
        let (receipt, receipt_detail) = self.receipt(config, lane);
        let (live, live_detail) = self.live(lane);
        let (deployed_sha, deployed_from) = deployed_of(live.as_ref(), receipt.as_ref());
        let (compare, compare_detail) = match &deployed_sha {
            Some(sha) => self.compare(config, project, lane, sha),
            None => (None, None),
        };
        let mut payload = Map::new();
        payload.insert("lane".into(), Value::String(lane.name.clone()));
        payload.insert("project".into(), Value::String(project.slug.clone()));
        payload.insert("branch".into(), Value::String(lane.branch.clone()));
        payload.insert(
            "receipt".into(),
            receipt.map(Value::Object).unwrap_or(Value::Null),
        );
        payload.insert("receipt_detail".into(), opt(receipt_detail));
        payload.insert(
            "live".into(),
            live.map(Value::Object).unwrap_or(Value::Null),
        );
        payload.insert("live_detail".into(), opt(live_detail));
        payload.insert("deployed_sha".into(), opt(deployed_sha));
        payload.insert("deployed_from".into(), opt(deployed_from));
        payload.insert(
            "compare".into(),
            compare.map(Value::Object).unwrap_or(Value::Null),
        );
        payload.insert("compare_detail".into(), opt(compare_detail));
        payload
    }

    fn receipt(
        &self,
        _config: &VogtConfig,
        lane: &DeployLane,
    ) -> (Option<Map<String, Value>>, Option<String>) {
        let (Some(receipt_repo), Some(receipt_path)) = (&lane.receipt_repo, &lane.receipt_path)
        else {
            return (None, None);
        };
        let provider = self.directory.provider_for(Some(receipt_repo));
        let repo = provider.and_then(|provider| provider.parse(Some(receipt_repo)));
        let (Some(provider), Some(repo)) = (provider, repo) else {
            return (
                None,
                Some(self.directory.unsupported_reason(Some(receipt_repo))),
            );
        };
        let raw = match provider.read_file(&repo, receipt_path) {
            Ok(raw) => raw,
            Err(error) => {
                return (
                    None,
                    Some(format!("receipt unreadable: {}", error.message())),
                )
            }
        };
        let Some(raw) = raw else {
            return (None, Some(format!("no receipt at {receipt_path}")));
        };
        let Some(parsed) = json_object(&raw) else {
            return (
                None,
                Some(format!("receipt at {receipt_path} is not a JSON object")),
            );
        };
        (Some(receipt_fields(&parsed)), None)
    }

    fn live(&self, lane: &DeployLane) -> (Option<Map<String, Value>>, Option<String>) {
        let Some(url) = &lane.version_url else {
            return (None, None);
        };
        let (status, body) = match self.versions.get(url) {
            Ok(answer) => answer,
            Err(error) => return (None, Some(format!("version endpoint unreachable: {error}"))),
        };
        if status >= 400 {
            return (None, Some(format!("version endpoint answered {status}")));
        }
        let Some(parsed) = json_object(&body) else {
            return (
                None,
                Some("version endpoint did not answer a JSON object".into()),
            );
        };
        let mut live = Map::new();
        live.insert(
            "source_sha".into(),
            opt(first_text(
                &parsed,
                &["source_sha", "sha", "revision", "commit"],
            )),
        );
        live.insert(
            "version".into(),
            opt(first_text(&parsed, &["product_version", "version"])),
        );
        live.insert(
            "source_ref".into(),
            opt(first_text(&parsed, &["source_ref", "ref"])),
        );
        (Some(live), None)
    }

    fn compare(
        &self,
        _config: &VogtConfig,
        project: &Project,
        lane: &DeployLane,
        deployed_sha: &str,
    ) -> (Option<Map<String, Value>>, Option<String>) {
        let provider = self.directory.provider_for(project.repo_url.as_deref());
        let repo = provider.and_then(|provider| provider.parse(project.repo_url.as_deref()));
        let (Some(provider), Some(repo)) = (provider, repo) else {
            return (
                None,
                Some(
                    self.directory
                        .unsupported_reason(project.repo_url.as_deref()),
                ),
            );
        };
        let comparison = match provider.compare(&repo, deployed_sha, &lane.branch) {
            Ok(comparison) => comparison,
            Err(error) => return (None, Some(format!("compare failed: {}", error.message()))),
        };
        let Some(comparison) = comparison else {
            return (
                None,
                Some(format!(
                    "the forge could not compare {} with {}",
                    &deployed_sha[..deployed_sha.len().min(12)],
                    lane.branch
                )),
            );
        };
        let commits = comparison
            .commits
            .iter()
            .rev()
            .take(MAX_COMMITS)
            .rev()
            .map(|(sha, subject)| {
                let mut row = Map::new();
                row.insert("sha".into(), Value::String(sha.clone()));
                row.insert("subject".into(), Value::String(subject.clone()));
                Value::Object(row)
            })
            .collect::<Vec<_>>();
        let mut out = Map::new();
        out.insert("status".into(), opt(comparison.status.clone()));
        out.insert("ahead_by".into(), Value::from(comparison.ahead_by));
        out.insert("behind_by".into(), Value::from(comparison.behind_by));
        out.insert("head_sha".into(), opt(comparison.head_sha.clone()));
        out.insert("commits".into(), Value::Array(commits.clone()));
        out.insert(
            "truncated".into(),
            Value::Bool(comparison.ahead_by > commits.len() as i64),
        );
        (Some(out), None)
    }
}

/// The live endpoint wins over the receipt when both name a sha.
fn deployed_of(
    live: Option<&Map<String, Value>>,
    receipt: Option<&Map<String, Value>>,
) -> (Option<String>, Option<String>) {
    for (source, from) in [(live, "live"), (receipt, "receipt")] {
        if let Some(sha) = source
            .and_then(|row| row.get("source_sha"))
            .and_then(Value::as_str)
        {
            if is_sha(sha) {
                return (Some(sha.to_string()), Some(from.to_string()));
            }
        }
    }
    (None, None)
}

/// `passed`, `failed`, or the receipt's own word, from what it reported.
pub fn receipt_status(value: Option<&str>) -> Option<String> {
    let lowered = value?.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return None;
    }
    if PASSED.contains(&lowered.as_str()) {
        return Some("passed".into());
    }
    if FAILED.contains(&lowered.as_str()) {
        return Some("failed".into());
    }
    Some(lowered)
}

/// The receipt facts Vogt reads — never the whole document, which may carry
/// image digests and deployment ids nobody asked to retain.
fn receipt_fields(parsed: &Map<String, Value>) -> Map<String, Value> {
    let smoke = parsed.get("live_smoke").and_then(Value::as_object);
    let status = first_present(parsed, &["status", "outcome"])
        .or_else(|| smoke.and_then(|smoke| smoke.get("status")));
    let mut out = Map::new();
    out.insert(
        "source_sha".into(),
        opt(first_text(parsed, &["source_sha", "sha", "commit"])),
    );
    out.insert(
        "source_tag".into(),
        opt(first_text(parsed, &["source_tag", "tag"])),
    );
    out.insert(
        "timestamp".into(),
        opt(first_text(
            parsed,
            &["timestamp", "deployed_at", "finished_at"],
        )),
    );
    out.insert(
        "environment".into(),
        opt(first_text(parsed, &["environment"])),
    );
    out.insert(
        "status".into(),
        opt(receipt_status(status.and_then(Value::as_str))),
    );
    out.insert(
        "url".into(),
        opt(first_text(parsed, &["url", "pipeline_url", "log_url"])),
    );
    out
}

fn json_object(raw: &[u8]) -> Option<Map<String, Value>> {
    let text = std::str::from_utf8(raw).ok()?;
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn first_text(payload: &Map<String, Value>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| match payload.get(*name) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    })
}

/// The first field that carries a real word. A JSON null, an empty string and
/// a whitespace-only string all count as absent, so `{"status": "", "outcome":
/// "failed"}` reads `failed` — the fallback the Inbox deploy alert depends on.
fn first_present<'a>(payload: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| match payload.get(*name) {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(payload.get(*name).unwrap()),
        Some(value) if !value.is_null() && !value.is_string() => Some(value),
        _ => None,
    })
}

/// Lowercase hex, seven to forty characters. Uppercase is rejected: a SHA the
/// forge reports is lowercase, and accepting both would let two spellings of
/// one commit read as two. A trailing newline is also rejected — the Python
/// pattern happens to allow one, and that quirk is deliberately not kept.
fn is_sha(value: &str) -> bool {
    (7..=40).contains(&value.len())
        && value
            .chars()
            .all(|char| char.is_ascii_hexdigit() && !char.is_ascii_uppercase())
}

fn opt(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::String)
}

/// Kept so a caller can surface a provider error without this module reaching
/// for transport detail.
#[allow(dead_code)]
fn provider_error(error: &VogtError) -> String {
    error.message().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Moment;
    use crate::errors::VogtError;

    struct MapVersions(Vec<(String, u16, Vec<u8>)>);

    impl VersionFetch for MapVersions {
        fn get(&self, url: &str) -> Result<(u16, Vec<u8>), String> {
            self.0
                .iter()
                .find(|(known, _, _)| known == url)
                .map(|(_, status, body)| (*status, body.clone()))
                .ok_or_else(|| format!("no answer for {url}"))
        }
    }

    #[test]
    fn receipt_status_collapses_the_words_and_keeps_the_rest() {
        assert_eq!(receipt_status(Some(" Success ")).as_deref(), Some("passed"));
        assert_eq!(receipt_status(Some("errored")).as_deref(), Some("failed"));
        assert_eq!(receipt_status(Some("pending")).as_deref(), Some("pending"));
        assert_eq!(receipt_status(Some("  ")), None);
        assert_eq!(receipt_status(None), None);
    }

    #[test]
    fn a_sha_is_seven_to_forty_hex_characters() {
        assert!(is_sha("abc1234"));
        assert!(is_sha("0123456789abcdef"));
        assert!(!is_sha("ABCDEF0"), "a sha is lowercase hex");
        assert!(
            !is_sha("abc1234\n"),
            "a trailing newline is not part of a sha"
        );
        assert!(!is_sha("abc123"));
        assert!(!is_sha("zzzzzzz"));
        assert!(!is_sha(&"a".repeat(41)));
    }

    #[test]
    fn the_live_sha_wins_and_a_bad_endpoint_is_a_detail() {
        let lane = DeployLane {
            name: "dev".into(),
            project: "widgets".into(),
            branch: "main".into(),
            receipt_repo: None,
            receipt_path: None,
            version_url: Some("https://dev.example/version".into()),
        };
        let versions = MapVersions(vec![(
            "https://dev.example/version".into(),
            200,
            br#"{"sha":"0123456789abcdef","version":"1.2.3"}"#.to_vec(),
        )]);
        let directory = EmptyDirectory;
        let collector = DeployLanesCollector::new(&directory, &versions);
        let (live, detail) = collector.live(&lane);
        assert_eq!(detail, None);
        let (sha, from) = deployed_of(live.as_ref(), None);
        assert_eq!(sha.as_deref(), Some("0123456789abcdef"));
        assert_eq!(from.as_deref(), Some("live"));

        let broken = MapVersions(vec![(
            "https://dev.example/version".into(),
            503,
            Vec::new(),
        )]);
        let collector = DeployLanesCollector::new(&directory, &broken);
        let (live, detail) = collector.live(&lane);
        assert!(live.is_none());
        assert_eq!(detail.as_deref(), Some("version endpoint answered 503"));
    }

    #[test]
    fn a_receipt_keeps_only_the_fields_vogt_reads() {
        let raw = br#"{"source_sha":"abcdef0","image":"registry/secret@sha256:ff","status":"green","environment":"prod"}"#;
        let parsed = json_object(raw).unwrap();
        let fields = receipt_fields(&parsed);
        assert!(
            !fields.contains_key("image"),
            "a digest nobody asked for is dropped"
        );
        assert_eq!(fields["status"], Value::String("passed".into()));
        assert_eq!(fields["environment"], Value::String("prod".into()));
    }

    struct EmptyDirectory;

    impl ForgeDirectory for EmptyDirectory {
        fn provider_for(
            &self,
            _: Option<&str>,
        ) -> Option<&dyn super::super::provider::ForgeProvider> {
            None
        }
        fn unsupported_reason(&self, _: Option<&str>) -> String {
            "no forge configured".into()
        }
    }

    #[allow(dead_code)]
    fn _uses(error: VogtError, moment: Moment) -> (String, Moment) {
        (provider_error(&error), moment)
    }
}
