//! WI-1838: the forge sweep must not spend the account's shared GitHub budget.
//! Ports `tests/test_github_rate_budget.py`.
//!
//! Three mechanisms, each tested through the client's transport seam:
//!
//! - conditional requests (ETag / If-None-Match), so an unchanged resource is a
//!   free 304 replayed from cache;
//! - the rate budget, read from `X-RateLimit-*` headers, which pauses reads
//!   below a floor until the window resets;
//! - posture read once per interval per repository, from the git tree rather
//!   than eight `/contents/` probes.
//!
//! The process-wide state is per test thread under `cargo test`, which is the
//! Python suite's autouse reset.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{json, Value};

use super::github::GitHubProvider;
use super::kinds::KIND_POSTURE;
use super::models::RepoRef;
use super::provider::ForgeProvider;
use super::sync::ForgeDirectory;
use super::{ForgePostureCollector, ForgeResponse};
use crate::adapters::github::{configure_rate_limit_floor, with_budget, GitHubClient, Transport};
use crate::config::VogtConfig;
use crate::core::{Moment, Project};
use crate::errors::VogtError;

/// Far future, so the budget stays blocked.
const RESET: i64 = 2_000_000_000;

type Answer = (u16, Vec<u8>, BTreeMap<String, String>);
type Calls = Rc<RefCell<Vec<(String, BTreeMap<String, String>)>>>;

/// A transport that answers from a function and records each call.
fn recorder(
    answer: impl Fn(&str, &BTreeMap<String, String>) -> Answer + 'static,
) -> (Calls, Rc<dyn Fn() -> Transport>) {
    let calls: Calls = Rc::new(RefCell::new(Vec::new()));
    let answer = Rc::new(answer);
    let record = Rc::clone(&calls);
    let make = move || -> Transport {
        let record = Rc::clone(&record);
        let answer = Rc::clone(&answer);
        Box::new(move |url, headers, _body, _method| {
            record.borrow_mut().push((url.to_owned(), headers.clone()));
            answer(url, headers)
        })
    };
    (calls, Rc::new(make))
}

fn headers(remaining: i64, extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::from([
        ("X-RateLimit-Remaining".to_owned(), remaining.to_string()),
        ("X-RateLimit-Reset".to_owned(), RESET.to_string()),
        ("X-RateLimit-Resource".to_owned(), "core".to_owned()),
    ]);
    for (name, value) in extra {
        out.insert((*name).to_owned(), (*value).to_owned());
    }
    out
}

fn client(token: &str, make: &Rc<dyn Fn() -> Transport>) -> GitHubClient {
    GitHubClient::new(Some(token.to_owned()), Some(make()))
}

fn last_segment(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("").to_owned()
}

// -- ETag / If-None-Match ---------------------------------------------------

#[test]
fn second_read_sends_if_none_match_and_replays_a_304() {
    let body = json!([{"name": "bug"}]).to_string().into_bytes();
    let (calls, make) = recorder(move |_, sent| {
        if sent.get("If-None-Match").map(String::as_str) == Some("\"v1\"") {
            return (304, Vec::new(), headers(4000, &[("ETag", "\"v1\"")]));
        }
        (200, body.clone(), headers(4000, &[("ETag", "\"v1\"")]))
    });
    let client = client("t", &make);

    let first = client.get("/repos/o/r/labels", &[]).unwrap();
    let second = client.get("/repos/o/r/labels", &[]).unwrap();

    assert_eq!(first, ForgeResponse::Json(json!([{"name": "bug"}])));
    assert_eq!(first, second);
    let calls = calls.borrow();
    assert!(!calls[0].1.contains_key("If-None-Match"));
    assert_eq!(calls[1].1["If-None-Match"], "\"v1\"");
}

#[test]
fn etag_cache_is_per_token() {
    let (calls, make) =
        recorder(|_, _| (200, b"[]".to_vec(), headers(4000, &[("ETag", "\"v1\"")])));
    client("a", &make).get("/repos/o/r/labels", &[]).unwrap();
    client("b", &make).get("/repos/o/r/labels", &[]).unwrap();
    assert!(!calls.borrow()[1].1.contains_key("If-None-Match"));
}

#[test]
fn a_transport_without_headers_gets_no_conditional_requests() {
    let (calls, make) = recorder(|_, _| (200, b"[]".to_vec(), BTreeMap::new()));
    let client = client("t", &make);
    client.get("/x", &[]).unwrap();
    client.get("/x", &[]).unwrap();
    assert!(calls
        .borrow()
        .iter()
        .all(|(_, sent)| !sent.contains_key("If-None-Match")));
}

// -- rate budget -------------------------------------------------------------

#[test]
fn reads_pause_below_the_floor_without_touching_the_network() {
    configure_rate_limit_floor(500);
    let (calls, make) = recorder(|_, _| (200, b"[]".to_vec(), headers(499, &[])));
    let client = client("t", &make);

    client.get("/one", &[]).unwrap(); // learns remaining=499
    let raised = client.get("/two", &[]).unwrap_err();

    assert_eq!(calls.borrow().len(), 1, "the paused read must not be sent");
    let VogtError::GitHubRateLimited { message, reset_at } = &raised else {
        panic!("expected github_rate_limited, got {raised:?}");
    };
    assert_eq!(*reset_at, Some(RESET));
    assert_eq!(
        message,
        "GitHub rate budget below 500; skipping /two until the reset at 03:33:20Z"
    );
    assert_eq!(raised.code(), "github_rate_limited");
    assert_eq!(raised.http_status(), 503);
}

#[test]
fn reads_continue_above_the_floor() {
    configure_rate_limit_floor(500);
    let (calls, make) = recorder(|_, _| (200, b"[]".to_vec(), headers(501, &[])));
    let client = client("t", &make);
    client.get("/one", &[]).unwrap();
    client.get("/two", &[]).unwrap();
    assert_eq!(calls.borrow().len(), 2);
}

#[test]
fn reads_resume_after_the_reset() {
    with_budget(|state| state.budget.clock = Some((RESET - 60) as f64));
    let (calls, make) = recorder(|_, _| (200, b"[]".to_vec(), headers(10, &[])));
    let client = client("t", &make);

    client.get("/one", &[]).unwrap();
    assert!(matches!(
        client.get("/two", &[]),
        Err(VogtError::GitHubRateLimited { .. })
    ));

    with_budget(|state| state.budget.clock = Some((RESET + 1) as f64));
    client.get("/three", &[]).unwrap();
    let urls: Vec<String> = calls
        .borrow()
        .iter()
        .map(|(url, _)| last_segment(url))
        .collect();
    assert_eq!(urls, ["one", "three"]);
}

#[test]
fn an_exhausted_403_raises_rate_limited() {
    let (_, make) = recorder(|_, _| {
        (
            403,
            br#"{"message":"rate limit"}"#.to_vec(),
            headers(0, &[]),
        )
    });
    let raised = client("t", &make).get("/one", &[]).unwrap_err();
    assert_eq!(
        raised,
        VogtError::GitHubRateLimited {
            message: "GitHub rate limit exhausted reading /one".to_owned(),
            reset_at: Some(RESET),
        }
    );
}

#[test]
fn a_non_core_bucket_does_not_gate_core_reads() {
    let (calls, make) = recorder(|_, _| {
        (
            200,
            b"[]".to_vec(),
            headers(1, &[("X-RateLimit-Resource", "search")]),
        )
    });
    let client = client("t", &make);
    client.get("/one", &[]).unwrap();
    client.get("/two", &[]).unwrap();
    assert_eq!(calls.borrow().len(), 2);
}

// -- posture: one tree read, once a day ---------------------------------------

fn tree(paths: &[(&str, &str)]) -> Vec<u8> {
    let entries: Vec<Value> = paths
        .iter()
        .map(|(path, sha)| json!({"path": path, "sha": sha}))
        .collect();
    json!({ "tree": entries }).to_string().into_bytes()
}

fn posture_answer(url: &str, _: &BTreeMap<String, String>) -> Answer {
    if url.ends_with("/git/trees/HEAD") {
        return (
            200,
            tree(&[("README.md", "r1"), (".github", "gh1")]),
            BTreeMap::new(),
        );
    }
    if url.ends_with("/git/trees/gh1") {
        return (
            200,
            tree(&[("dependabot.yml", "d1"), ("workflows", "w1")]),
            BTreeMap::new(),
        );
    }
    (404, Vec::new(), BTreeMap::new())
}

fn repo() -> RepoRef {
    RepoRef {
        host: "github.com".into(),
        owner: "o".into(),
        repo: "r".into(),
    }
}

#[test]
fn posture_reads_the_tree_not_eight_contents_probes() {
    let (calls, make) = recorder(posture_answer);
    let provider = GitHubProvider::new(client("t", &make));

    let posture = provider.posture(&repo()).unwrap();

    assert_eq!(
        posture.version_updates_config.as_deref(),
        Some(".github/dependabot.yml")
    );
    let calls = calls.borrow();
    assert!(!calls.iter().any(|(url, _)| url.contains("/contents/")));
    // Two tree reads plus the two toggles.
    assert_eq!(calls.len(), 4);
}

#[test]
fn posture_without_a_github_dir_is_one_tree_read() {
    let (calls, make) = recorder(|url, _| {
        if url.ends_with("/git/trees/HEAD") {
            return (200, tree(&[("renovate.json", "x")]), BTreeMap::new());
        }
        (404, Vec::new(), BTreeMap::new())
    });
    let provider = GitHubProvider::new(client("t", &make));
    let posture = provider.posture(&repo()).unwrap();

    assert_eq!(
        posture.version_updates_config.as_deref(),
        Some("renovate.json")
    );
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|(url, _)| url.contains("/git/trees/"))
            .count(),
        1
    );
}

/// A directory over one provider on the recorder, as Python's patched
/// `provider_for` answers per project; the budget state is process-wide
/// either way.
struct Recorded(GitHubProvider<GitHubClient>);

impl Recorded {
    fn new(make: &Rc<dyn Fn() -> Transport>) -> Self {
        Self(GitHubProvider::new(client("t", make)))
    }
}

impl ForgeDirectory for Recorded {
    fn provider_for(&self, _: Option<&str>) -> Option<&dyn ForgeProvider> {
        Some(&self.0)
    }

    fn unsupported_reason(&self, _: Option<&str>) -> String {
        "unused".into()
    }
}

fn project() -> Project {
    let mut project = Project::new("prj_1", "r", "r", "/tmp/r", Moment::from_unix(0, 0));
    project.repo_url = Some("https://github.com/o/r".into());
    project
}

/// 2026-10-18T12:00:00Z.
const NOON: i64 = 1_792_324_800;

#[test]
fn posture_is_read_once_per_interval() {
    for (interval, later, reads_again) in [(86400, 3600, false), (86400, 86400, true), (0, 1, true)]
    {
        with_posture_reset();
        let (calls, make) = recorder(posture_answer);
        let directory = Recorded::new(&make);
        let config = VogtConfig {
            forge_posture_interval_seconds: interval,
            ..VogtConfig::default()
        };
        let collector = ForgePostureCollector::new(&directory);

        let first = collector
            .collect(&config, &project(), Moment::from_unix(NOON, 0))
            .unwrap();
        let calls_after_first = calls.borrow().len();
        let second = collector
            .collect(&config, &project(), Moment::from_unix(NOON + later, 0))
            .unwrap();

        assert_eq!(
            first
                .iter()
                .filter(|found| found.kind == KIND_POSTURE)
                .count(),
            1
        );
        let read = calls.borrow().len() > calls_after_first;
        assert_eq!(read, reads_again, "interval {interval}, later {later}");
        // A skipped sweep still records its receipt, with nothing read.
        assert_eq!(second.len(), if reads_again { 2 } else { 1 });
    }
}

#[test]
fn a_rate_limited_posture_read_records_nothing_and_retries() {
    // Review blocker: a paused read must not be stored as "no config" and
    // then pinned for a day by the schedule.
    let limited = Rc::new(Cell::new(true));
    let gate = Rc::clone(&limited);
    let (_, make) = recorder(move |url, sent| {
        if gate.get() {
            return (403, b"{}".to_vec(), headers(0, &[]));
        }
        posture_answer(url, sent)
    });
    let directory = Recorded::new(&make);
    let config = VogtConfig::default();
    let collector = ForgePostureCollector::new(&directory);

    assert!(matches!(
        collector.collect(&config, &project(), Moment::from_unix(NOON, 0)),
        Err(VogtError::GitHubRateLimited { .. })
    ));

    limited.set(false);
    with_budget(|state| *state = Default::default());
    let found = collector
        .collect(&config, &project(), Moment::from_unix(NOON + 900, 0))
        .unwrap();
    let posture: Vec<_> = found
        .iter()
        .filter(|found| found.kind == KIND_POSTURE)
        .collect();
    assert_eq!(
        posture.len(),
        1,
        "the next sweep reads posture, not in a day"
    );
    assert_eq!(
        posture[0].payload["version_updates_config"],
        ".github/dependabot.yml"
    );
}

fn with_posture_reset() {
    super::collectors::with_posture_schedule(|schedule| *schedule = Default::default());
    with_budget(|state| *state = Default::default());
}
