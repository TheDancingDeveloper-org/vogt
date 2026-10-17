//! The stand-in forge the tests drive the forge paths through. Ports the
//! `RecordingForge` and `forge_fixture_manifest` idea from the Python harness: a
//! GitHub-shaped server answered from canned lists, that records every request it
//! was asked to send. Nothing here opens a socket.
//!
//! A test asserts two things against it: the answer the service gave, and that
//! every request was a read. An unexpected path answers 404, which the providers
//! read as "not visible", so a missing route shows up as a missing count rather
//! than a silent pass.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::application::forge_transport::SharedForgeTransport;

/// The canned lists one repository answers with.
pub struct StandInForge {
    pub issues: Value,
    pub pulls: Value,
    pub labels: Value,
    pub releases: Value,
    seen: Arc<Mutex<Vec<String>>>,
}

impl StandInForge {
    /// One issue, one label and one release on `acme/widget`: the smallest
    /// repository that makes every read non-empty.
    pub fn one_of_each() -> Self {
        Self {
            issues: json!([{
                "number": 7,
                "title": "Onboard me",
                "state": "open",
                "html_url": "https://github.com/acme/widget/issues/7",
                "updated_at": "2026-01-02T03:04:05Z",
                "created_at": "2026-01-01T00:00:00Z",
                "labels": [],
                "user": {"login": "octo"},
                "body": "",
            }]),
            pulls: json!([]),
            labels: json!([{"name": "bug", "color": "d73a4a"}]),
            releases: json!([{
                "tag_name": "v1.0.0",
                "name": "v1.0.0",
                "html_url": "https://github.com/acme/widget/releases/tag/v1.0.0",
                "published_at": "2026-01-03T00:00:00Z",
                "draft": false,
                "prerelease": false,
            }]),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every request sent so far, as `METHOD url`.
    pub fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// The stand-in as the transport a service context carries.
    pub fn transport(&self) -> SharedForgeTransport {
        let seen = Arc::clone(&self.seen);
        let issues = self.issues.clone();
        let pulls = self.pulls.clone();
        let labels = self.labels.clone();
        let releases = self.releases.clone();
        Arc::new(
            move |url: &str, _headers: &BTreeMap<String, String>, _body: &[u8], method: &str| {
                seen.lock().unwrap().push(format!("{method} {url}"));
                let answer = if url.contains("/issues?") {
                    &issues
                } else if url.contains("/pulls?") {
                    &pulls
                } else if url.contains("/labels?") {
                    &labels
                } else if url.contains("/releases?") {
                    &releases
                } else {
                    return (404, Vec::new(), BTreeMap::new());
                };
                (200, answer.to_string().into_bytes(), BTreeMap::new())
            },
        )
    }
}
