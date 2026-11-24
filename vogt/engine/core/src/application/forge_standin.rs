//! The stand-in forge the tests drive the forge paths through. Ports the
//! `RecordingForge` and `forge_fixture_manifest` idea from the Python harness: a
//! GitHub-shaped server answered from canned lists, that records every request it
//! was asked to send. Nothing here opens a socket.
//!
//! A test asserts two things against it: the answer the service gave, and the
//! requests it was sent. The reads answer from canned lists. The writes are the
//! repository create, which mints the repository and records the body it was sent
//! (or answers 422 for a name the account already holds), the issue create, which
//! numbers each issue from 8 up, and the `GET /user` a pasted token is checked
//! against. An unexpected path answers 404, which the providers read
//! as "not visible", so a missing route shows up as a missing count rather than a
//! silent pass.

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
    /// The repositories the account already holds: a create of one answers 422.
    pub existing: Vec<String>,
    seen: Arc<Mutex<Vec<String>>>,
    creates: Arc<Mutex<Vec<Value>>>,
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
            existing: Vec::new(),
            seen: Arc::new(Mutex::new(Vec::new())),
            creates: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// As [`StandInForge::one_of_each`], with `names` already taken upstream.
    pub fn with_existing(mut self, names: &[&str]) -> Self {
        self.existing = names.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    /// Every request sent so far, as `METHOD url`.
    pub fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// The body of every repository create sent so far.
    pub fn creates(&self) -> Vec<Value> {
        self.creates.lock().unwrap().clone()
    }

    /// The stand-in as the transport a service context carries.
    pub fn transport(&self) -> SharedForgeTransport {
        let seen = Arc::clone(&self.seen);
        let issues = self.issues.clone();
        let pulls = self.pulls.clone();
        let labels = self.labels.clone();
        let releases = self.releases.clone();
        let repos = json!([{
            "name": "fuelwatchx",
            "owner": {"login": "acme"},
            "default_branch": "main",
            "private": false,
            "html_url": "https://forge.example.net/acme/fuelwatchx",
        }]);
        let existing = self.existing.clone();
        let numbers = Arc::new(Mutex::new(8_i64));
        let creates = Arc::clone(&self.creates);
        Arc::new(
            move |url: &str, _headers: &BTreeMap<String, String>, body: &[u8], method: &str| {
                seen.lock().unwrap().push(format!("{method} {url}"));
                if method == "GET" && url.ends_with("/user") {
                    let answer = json!({"login": "octo-actor"});
                    let headers =
                        BTreeMap::from([("x-oauth-scopes".to_owned(), "repo".to_owned())]);
                    return (200, answer.to_string().into_bytes(), headers);
                }
                if method == "POST" && url.ends_with("/user/repos") {
                    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                    let name = payload["name"].as_str().unwrap_or_default().to_owned();
                    if existing.contains(&name) {
                        let answer = json!({"message": "name already exists on this account"});
                        return (422, answer.to_string().into_bytes(), BTreeMap::new());
                    }
                    let private = payload["private"].as_bool().unwrap_or(true);
                    let answer = json!({
                        "name": name,
                        "owner": {"login": "acme"},
                        "private": private,
                        "default_branch": null,
                        "html_url": format!("https://github.com/acme/{name}"),
                    });
                    creates.lock().unwrap().push(payload);
                    return (201, answer.to_string().into_bytes(), BTreeMap::new());
                }
                if method == "POST" && url.contains("/repos/acme/") && url.ends_with("/issues") {
                    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                    let repo = url
                        .split("/repos/acme/")
                        .nth(1)
                        .unwrap_or_default()
                        .trim_end_matches("/issues")
                        .to_owned();
                    let number = {
                        let mut next = numbers.lock().unwrap();
                        *next += 1;
                        *next - 1
                    };
                    let answer = json!({
                        "number": number,
                        "title": payload["title"],
                        "state": "open",
                        "html_url": format!("https://github.com/acme/{repo}/issues/{number}"),
                    });
                    return (201, answer.to_string().into_bytes(), BTreeMap::new());
                }
                if method == "GET" && is_repo_address(url) {
                    let name = url.rsplit('/').next().unwrap_or_default();
                    let answer = json!({"name": name, "default_branch": "main"});
                    return (200, answer.to_string().into_bytes(), BTreeMap::new());
                }
                let answer = if url.contains("/issues?") {
                    &issues
                } else if url.contains("/user/repos") {
                    &repos
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

/// `…/repos/<owner>/<name>` and nothing after it: the repository describe.
fn is_repo_address(url: &str) -> bool {
    let Some(tail) = url.split("/repos/").nth(1) else {
        return false;
    };
    tail.split('/').count() == 2 && !tail.contains('?')
}
