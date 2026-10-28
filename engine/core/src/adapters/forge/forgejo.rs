//! The Forgejo/Gitea implementation of [`ForgeProvider`](super::provider::ForgeProvider).
//! Ports `adapters/forge/forgejo.py`.
//!
//! One installation, its host taken from configuration rather than a constant,
//! behind the same transport the GitHub provider uses. The differences that
//! matter are named where they happen: the host-qualified subject key, issues
//! that carry a null `pull_request` field, checks folded into one status, and
//! writes that report a failure instead of raising.

use super::models::{
    ForgeActor, ForgeCapabilities, ForgeCheck, ForgeComparison, ForgeIssue, ForgeJob, ForgeLabel,
    ForgeNotification, ForgePosture, ForgePull, ForgeRelease, ForgeRepo, IssuesItem, RepoRef,
};
use super::payloads::{comparison, decoded_content, quote_path};
use super::provider::ForgeProvider;
use super::transport::{api_path, as_list, ForgeResponse, ForgeTransport, WriteMethod};
use super::writeback::{WriteBackOutcome, WriteBackResult};
use crate::core::py_repr;
use crate::errors::VogtError;

const DEFAULT_PER_PAGE: &str = "100";

/// Statuses Forgejo reports that are terminal, and so usable as a conclusion.
/// A run still in flight has a status and honestly no conclusion.
const TERMINAL_CHECK_STATES: &[&str] = &["success", "failure", "cancelled", "skipped"];

pub struct ForgejoProvider<T: ForgeTransport> {
    transport: T,
    /// The installation's host, as configured. The API root is the transport's.
    host: String,
    capabilities: ForgeCapabilities,
}

impl<T: ForgeTransport> ForgejoProvider<T> {
    pub fn new(transport: T, host: impl Into<String>) -> Self {
        let host = host.into();
        let capabilities = ForgeCapabilities {
            hosts: vec![host.clone()],
            supports_since: true,
            // No Dependabot-style posture surface; the gap reports itself
            // through the collector's capability gate.
            supports_posture: false,
            supports_notifications: true,
            supports_webhooks: false,
        };
        Self {
            transport,
            host,
            capabilities,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }
}

impl<T: ForgeTransport> ForgeProvider for ForgejoProvider<T> {
    fn capabilities(&self) -> ForgeCapabilities {
        self.capabilities.clone()
    }

    fn list_repos(&self) -> Result<Vec<ForgeRepo>, VogtError> {
        let response = self
            .transport
            .get("/user/repos", &[("limit", DEFAULT_PER_PAGE.to_owned())])?;
        let mut repos = Vec::new();
        for item in as_list(&response) {
            let Some(name) = text(item.get("name")) else {
                continue;
            };
            let Some(owner) = login(item.get("owner")) else {
                continue;
            };
            repos.push(ForgeRepo {
                web_url: text(item.get("html_url"))
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("https://{}/{owner}/{name}", self.host)),
                owner: owner.to_owned(),
                name: name.to_owned(),
                default_branch: text(item.get("default_branch")).map(str::to_owned),
                visibility: visibility(item.get("private"), false),
            });
        }
        Ok(repos)
    }

    fn parse(&self, repo_url: Option<&str>) -> Option<RepoRef> {
        parse_repo_url(repo_url, &[&self.host])
    }

    /// `forge:{host}/{owner}/{repo}#{n}` — host-qualified, because a second
    /// installation can hold the same `owner/repo`.
    fn subject_key(&self, repo: &RepoRef, number: i64) -> String {
        format!("forge:{}/{}/{}#{number}", repo.host, repo.owner, repo.repo)
    }

    fn number_of(&self, subject_key: Option<&str>) -> Option<i64> {
        let tail = subject_key?.split_once('#')?.1;
        tail.parse()
            .ok()
            .filter(|_| tail.chars().all(|c| c.is_ascii_digit()))
    }

    fn clone_url(&self, repo: &RepoRef) -> String {
        format!("https://{}/{}/{}.git", repo.host, repo.owner, repo.repo)
    }

    fn web_url(&self, repo: &RepoRef) -> String {
        format!("https://{}/{}/{}", repo.host, repo.owner, repo.repo)
    }

    fn describe(
        &self,
        repo: &RepoRef,
    ) -> Result<Option<serde_json::Map<String, serde_json::Value>>, VogtError> {
        Ok(
            match self
                .transport
                .get(&format!("/repos/{}/{}", repo.owner, repo.repo), &[])?
            {
                ForgeResponse::Json(serde_json::Value::Object(map)) => Some(map),
                ForgeResponse::Json(_) => Some(serde_json::Map::new()),
                ForgeResponse::Empty | ForgeResponse::Missing => None,
            },
        )
    }

    fn clone_token(&self) -> Option<String> {
        self.transport.token().map(str::to_owned)
    }

    fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
        // Forgejo reports no scope header, so the second half is honestly empty.
        let ForgeResponse::Json(serde_json::Value::Object(user)) =
            self.transport.get("/user", &[])?
        else {
            return Ok(None);
        };
        Ok(text(user.get("login")).map(|login| (login.to_owned(), String::new())))
    }

    fn issues_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<IssuesItem>, VogtError> {
        // `type=issues` keeps pull requests out server-side; the null check
        // below guards older installs that ignore the parameter. Gitea carries
        // `pull_request` on *every* issue, null for the real ones, so only a
        // non-null value marks a PR.
        let mut query = vec![
            ("state", "all".to_owned()),
            ("type", "issues".to_owned()),
            ("limit", DEFAULT_PER_PAGE.to_owned()),
        ];
        if let Some(since) = since {
            query.push(("since", since.to_owned()));
        }
        let response = self.transport.get(
            &format!("/repos/{}/{}/issues", repo.owner, repo.repo),
            &query,
        )?;
        let mut items: Vec<&serde_json::Map<String, serde_json::Value>> = as_list(&response)
            .into_iter()
            .filter(|item| {
                item.get("pull_request")
                    .is_none_or(serde_json::Value::is_null)
            })
            .collect();
        items.sort_by_key(|item| text(item.get("updated_at")).unwrap_or("").to_owned());
        Ok(items
            .into_iter()
            .map(|item| IssuesItem::Issue(to_issue(repo, item)))
            .collect())
    }

    fn pulls_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<ForgePull>, VogtError> {
        let response = self.transport.get(
            &format!("/repos/{}/{}/pulls", repo.owner, repo.repo),
            &[
                ("state", "all".to_owned()),
                ("sort", "recentupdate".to_owned()),
                ("limit", DEFAULT_PER_PAGE.to_owned()),
            ],
        )?;
        let mut pulls: Vec<ForgePull> = as_list(&response)
            .into_iter()
            .map(|item| to_pull(repo, item))
            .collect();
        if let Some(since) = since {
            pulls.retain(|pull| pull.updated_at.as_deref().is_none_or(|at| at >= since));
        }
        pulls.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        Ok(pulls)
    }

    fn releases(&self, repo: &RepoRef) -> Result<Vec<ForgeRelease>, VogtError> {
        let response = self.transport.get(
            &format!("/repos/{}/{}/releases", repo.owner, repo.repo),
            &[("limit", DEFAULT_PER_PAGE.to_owned())],
        )?;
        Ok(as_list(&response)
            .into_iter()
            .map(|item| ForgeRelease {
                tag: text(item.get("tag_name")).unwrap_or("").to_owned(),
                repo: repo.slug(),
                name: text(item.get("name")).map(str::to_owned),
                draft: flag(item.get("draft")),
                prerelease: flag(item.get("prerelease")),
                published_at: text(item.get("published_at")).map(str::to_owned),
                source_url: text(item.get("html_url")).map(str::to_owned),
            })
            .collect())
    }

    fn checks(&self, repo: &RepoRef) -> Result<Vec<ForgeCheck>, VogtError> {
        // An install without Actions answers 404, which is an honest empty.
        let response = self.transport.get(
            &format!("/repos/{}/{}/actions/tasks", repo.owner, repo.repo),
            &[("limit", "20".to_owned())],
        )?;
        let items = match &response {
            ForgeResponse::Json(value) => value
                .get("workflow_runs")
                .and_then(serde_json::Value::as_array),
            _ => None,
        };
        Ok(items
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_object)
            .map(|item| {
                let status = text(item.get("status")).map(str::to_owned);
                let mut check = ForgeCheck::new(
                    text(item.get("head_sha")).unwrap_or(""),
                    text(item.get("name")).unwrap_or("workflow"),
                    repo.slug(),
                );
                check.conclusion = status
                    .as_deref()
                    .filter(|state| TERMINAL_CHECK_STATES.contains(state))
                    .map(str::to_owned);
                check.status = status;
                check.branch = text(item.get("head_branch")).map(str::to_owned);
                check.event = text(item.get("event")).map(str::to_owned);
                check.run_number = item.get("run_number").and_then(serde_json::Value::as_i64);
                check.workflow_path = text(item.get("path")).map(str::to_owned);
                check.updated_at = text(item.get("updated_at"))
                    .or_else(|| text(item.get("created_at")))
                    .map(str::to_owned);
                check.source_url = text(item.get("url")).map(str::to_owned);
                check
            })
            .collect())
    }

    fn watched_ref_checks(&self, _repo: &RepoRef) -> Result<Vec<ForgeCheck>, VogtError> {
        // The tasks endpoint takes no event filter, so there is no second page.
        Ok(Vec::new())
    }

    fn failed_jobs(&self, _repo: &RepoRef, _run_id: i64) -> Result<Vec<ForgeJob>, VogtError> {
        // A Forgejo task already is one job; there is no per-run listing.
        Ok(Vec::new())
    }

    fn read_file(&self, repo: &RepoRef, path: &str) -> Result<Option<Vec<u8>>, VogtError> {
        let response = self.transport.get(
            &format!(
                "/repos/{}/{}/contents/{}",
                repo.owner,
                repo.repo,
                quote_path(path)
            ),
            &[],
        )?;
        Ok(match &response {
            ForgeResponse::Json(value) => decoded_content(value),
            _ => None,
        })
    }

    fn compare(
        &self,
        repo: &RepoRef,
        base: &str,
        head: &str,
    ) -> Result<Option<ForgeComparison>, VogtError> {
        let response = self.transport.get(
            &format!(
                "/repos/{}/{}/compare/{}...{}",
                repo.owner,
                repo.repo,
                quote_path(base),
                quote_path(head)
            ),
            &[],
        )?;
        Ok(match &response {
            ForgeResponse::Json(value) => comparison(base, head, value),
            _ => None,
        })
    }

    fn labels(&self, repo: &RepoRef) -> Result<Vec<ForgeLabel>, VogtError> {
        let response = self.transport.get(
            &format!("/repos/{}/{}/labels", repo.owner, repo.repo),
            &[("limit", DEFAULT_PER_PAGE.to_owned())],
        )?;
        Ok(as_list(&response)
            .into_iter()
            .filter_map(|item| {
                Some(ForgeLabel {
                    name: text(item.get("name"))?.to_owned(),
                    repo: repo.slug(),
                    color: text(item.get("color")).map(str::to_owned),
                    description: text(item.get("description")).map(str::to_owned),
                })
            })
            .collect())
    }

    fn posture(&self, repo: &RepoRef) -> Result<ForgePosture, VogtError> {
        // Not offered, and the capability says so. Three `None`s are "could
        // not tell", which is not the same answer as "off".
        Ok(ForgePosture {
            version_updates_config: None,
            vulnerability_alerts: None,
            automated_security_fixes: None,
            repo: repo.slug(),
        })
    }

    fn notifications(&self, repo: &RepoRef) -> Result<Vec<ForgeNotification>, VogtError> {
        let response = self.transport.get(
            &format!("/repos/{}/{}/notifications", repo.owner, repo.repo),
            &[
                ("limit", DEFAULT_PER_PAGE.to_owned()),
                ("all", "true".to_owned()),
            ],
        )?;
        Ok(as_list(&response)
            .into_iter()
            .map(|item| {
                let subject = item.get("subject").and_then(serde_json::Value::as_object);
                let subject_url = subject.and_then(|s| s.get("url"));
                ForgeNotification {
                    thread: json_text(item.get("id")),
                    repo: repo.slug(),
                    // No reason and no per-thread read timestamp; `None` is the
                    // honest value, not a guess.
                    reason: None,
                    unread: flag(item.get("unread")),
                    title: subject
                        .and_then(|s| text(s.get("title")))
                        .unwrap_or("")
                        .to_owned(),
                    subject_type: subject.and_then(|s| text(s.get("type"))).map(str::to_owned),
                    updated_at: text(item.get("updated_at")).map(str::to_owned),
                    last_read_at: None,
                    source_url: subject_web_url(subject_url),
                    subject_api_url: text(subject_url).map(str::to_owned),
                    latest_comment_api_url: subject
                        .and_then(|s| text(s.get("latest_comment_url")))
                        .map(str::to_owned),
                }
            })
            .collect())
    }

    fn resolve_actor(&self, api_url: &str) -> Result<Option<ForgeActor>, VogtError> {
        // Forgejo reports no account type or association, so only the login is
        // known. Anything outside this API's repository tree is not followed.
        let Some(path) = api_path(api_url, self.transport.api_root()) else {
            return Ok(None);
        };
        let ForgeResponse::Json(serde_json::Value::Object(payload)) =
            self.transport.get(&path, &[])?
        else {
            return Ok(None);
        };
        Ok(
            login(payload.get("user").or_else(|| payload.get("author"))).map(|name| ForgeActor {
                login: Some(name.to_owned()),
                user_type: None,
                association: None,
            }),
        )
    }

    fn org_members(&self, owner: &str) -> Result<Option<Vec<String>>, VogtError> {
        let mut members = Vec::new();
        for page in 1..=10 {
            let response = self.transport.get(
                &format!("/orgs/{owner}/members"),
                &[
                    ("limit", DEFAULT_PER_PAGE.to_owned()),
                    ("page", page.to_string()),
                ],
            )?;
            if matches!(response, ForgeResponse::Missing) {
                return Ok(if page == 1 { None } else { Some(members) });
            }
            let batch = as_list(&response);
            let count = batch.len();
            for item in batch {
                if let Some(name) = login(Some(&serde_json::Value::Object(item.clone()))) {
                    members.push(name.to_ascii_lowercase());
                }
            }
            if count < 100 {
                break;
            }
        }
        Ok(Some(members))
    }

    fn comment(
        &self,
        repo: &RepoRef,
        number: i64,
        body: &str,
    ) -> Result<WriteBackResult, VogtError> {
        self.post(
            repo,
            &format!(
                "/repos/{}/{}/issues/{number}/comments",
                repo.owner, repo.repo
            ),
            &serde_json::json!({"body": body}),
            Some(number),
            WriteMethod::Post,
        )
    }

    fn create_issue(
        &self,
        repo: &RepoRef,
        title: &str,
        body: &str,
        labels: Option<&[String]>,
    ) -> Result<WriteBackResult, VogtError> {
        // Two appends: Forgejo's create takes label ids, so names ride the
        // labels endpoint afterwards. A create whose labels did not land is
        // still a created issue, and the result says which half held.
        let created = self.post(
            repo,
            &format!("/repos/{}/{}/issues", repo.owner, repo.repo),
            &serde_json::json!({"title": title, "body": body}),
            None,
            WriteMethod::Post,
        )?;
        let Some(labels) = labels.filter(|labels| !labels.is_empty()) else {
            return Ok(created);
        };
        if created.outcome != WriteBackOutcome::Succeeded {
            return Ok(created);
        }
        let Some(number) = self.number_of(created.subject_key.as_deref()) else {
            return Ok(created);
        };
        let labelled = self.add_labels(repo, number, labels)?;
        if labelled.outcome == WriteBackOutcome::Succeeded {
            return Ok(created);
        }
        let detail = match &created.detail {
            Some(detail) => format!(
                "{detail}; labels did not land: {}",
                labelled.detail.unwrap_or_default()
            ),
            None => format!(
                "labels did not land: {}",
                labelled.detail.unwrap_or_default()
            ),
        };
        Ok(WriteBackResult {
            outcome: WriteBackOutcome::Succeeded,
            detail: Some(detail),
            source_url: created.source_url,
            subject_key: created.subject_key,
        })
    }

    fn add_labels(
        &self,
        repo: &RepoRef,
        number: i64,
        labels: &[String],
    ) -> Result<WriteBackResult, VogtError> {
        // POST appends; the replacing verb (PUT) is never spoken.
        self.post(
            repo,
            &format!("/repos/{}/{}/issues/{number}/labels", repo.owner, repo.repo),
            &serde_json::json!({"labels": labels}),
            Some(number),
            WriteMethod::Post,
        )
    }

    fn set_state(
        &self,
        repo: &RepoRef,
        number: i64,
        state: &str,
    ) -> Result<WriteBackResult, VogtError> {
        if state != "closed" && state != "open" {
            return Err(VogtError::InvalidRequest(format!(
                "{} is not a state; use 'closed' or 'open'",
                py_repr(state)
            )));
        }
        self.post(
            repo,
            &format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo),
            &serde_json::json!({"state": state}),
            Some(number),
            WriteMethod::Patch,
        )
    }

    fn update_issue_body(
        &self,
        repo: &RepoRef,
        number: i64,
        body: &str,
    ) -> Result<WriteBackResult, VogtError> {
        self.post(
            repo,
            &format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo),
            &serde_json::json!({"body": body}),
            Some(number),
            WriteMethod::Patch,
        )
    }

    fn create_repo(
        &self,
        name: &str,
        private: bool,
        description: Option<&str>,
    ) -> Result<ForgeRepo, VogtError> {
        // No auto-init: the local history is about to be pushed, and a
        // generated first commit would make that push non-fast-forward.
        let mut payload = serde_json::json!({"name": name, "private": private, "auto_init": false});
        if let Some(description) = description.filter(|text| !text.is_empty()) {
            payload["description"] = serde_json::Value::String(description.to_owned());
        }
        let response = match self
            .transport
            .send(WriteMethod::Post, "/user/repos", Some(&payload))
        {
            Err(VogtError::ForgeHttpStatus {
                status: 409 | 422, ..
            }) => {
                return Err(VogtError::RemoteRepoExists(format!(
                    "{} already has a repository named {} reachable by this account; \
                     `forge.publish` never adopts or overwrites an existing remote — pick \
                     another name, or attach to the existing repository with `forge link` \
                     after setting the project's repo_url",
                    self.host,
                    py_repr(name)
                )));
            }
            // Any other refusal is Forgejo's own unavailable error, the way
            // Python re-raises `ForgejoUnavailable` when the status is neither
            // 409 nor 422.
            Err(VogtError::ForgeHttpStatus { message, .. }) => {
                return Err(VogtError::ForgejoUnavailable(message));
            }
            other => other?,
        };
        let owner = login(response.get("owner")).ok_or_else(|| {
            VogtError::UpstreamWriteFailed(format!(
                "{} accepted the repository create but returned no owner, so there is no address \
                 to push to",
                self.host
            ))
        })?;
        let created = text(response.get("name")).unwrap_or(name);
        Ok(ForgeRepo {
            web_url: text(response.get("html_url"))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("https://{}/{owner}/{created}", self.host)),
            owner: owner.to_owned(),
            name: created.to_owned(),
            default_branch: text(response.get("default_branch")).map(str::to_owned),
            visibility: visibility(response.get("private"), private),
        })
    }
}

impl<T: ForgeTransport> ForgejoProvider<T> {
    /// One append upstream. A transport failure is a `failed` result, never
    /// fatal to the declared write: the local change stands and the ledger
    /// records that the upstream half did not land.
    fn post(
        &self,
        repo: &RepoRef,
        endpoint: &str,
        payload: &serde_json::Value,
        number: Option<i64>,
        method: WriteMethod,
    ) -> Result<WriteBackResult, VogtError> {
        let response = match self.transport.send(method, endpoint, Some(payload)) {
            Err(error) => {
                return Ok(WriteBackResult::failed(error.message().to_owned()));
            }
            Ok(serde_json::Value::Null) => {
                return Ok(WriteBackResult::failed(format!(
                    "{endpoint} returned nothing (404?)"
                )));
            }
            Ok(response) => response,
        };
        let upstream_number = response
            .get("number")
            .and_then(serde_json::Value::as_i64)
            .or(number);
        Ok(WriteBackResult {
            outcome: WriteBackOutcome::Succeeded,
            source_url: response
                .get("html_url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            subject_key: upstream_number.map(|number| self.subject_key(repo, number)),
            detail: Some(
                serde_json::json!({"method": method.as_str(), "endpoint": endpoint}).to_string(),
            ),
        })
    }
}

fn text(value: Option<&serde_json::Value>) -> Option<&str> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
}

fn flag(value: Option<&serde_json::Value>) -> bool {
    value.and_then(serde_json::Value::as_bool).unwrap_or(false)
}

fn visibility(value: Option<&serde_json::Value>, default: bool) -> String {
    if value
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default)
    {
        "private"
    } else {
        "public"
    }
    .to_owned()
}

fn json_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// A Gitea user's login. The API carries both `login` and `username`; either
/// satisfies, neither is invented.
fn login(user: Option<&serde_json::Value>) -> Option<&str> {
    let user = user?.as_object()?;
    text(user.get("login")).or_else(|| text(user.get("username")))
}

fn to_issue(repo: &RepoRef, item: &serde_json::Map<String, serde_json::Value>) -> ForgeIssue {
    ForgeIssue {
        number: item
            .get("number")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        title: text(item.get("title")).unwrap_or("").to_owned(),
        state: text(item.get("state")).unwrap_or("open").to_owned(),
        repo: repo.slug(),
        labels: label_names(item.get("labels")),
        author: login(item.get("user")).map(str::to_owned),
        author_type: None,
        author_association: None,
        assignees: item
            .get("assignees")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|a| login(Some(a)))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        comments: item
            .get("comments")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        body: item
            .get("body")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        updated_at: text(item.get("updated_at")).map(str::to_owned),
        closed_at: text(item.get("closed_at")).map(str::to_owned),
        // Gitea/Forgejo do not say why an issue closed; a close reads `done`.
        state_reason: None,
        source_url: text(item.get("html_url")).map(str::to_owned),
    }
}

fn to_pull(repo: &RepoRef, item: &serde_json::Map<String, serde_json::Value>) -> ForgePull {
    let head = item.get("head").and_then(serde_json::Value::as_object);
    ForgePull {
        number: item
            .get("number")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        title: text(item.get("title")).unwrap_or("").to_owned(),
        state: text(item.get("state")).unwrap_or("open").to_owned(),
        repo: repo.slug(),
        draft: flag(item.get("draft")),
        merged: flag(item.get("merged")) || text(item.get("merged_at")).is_some(),
        author: login(item.get("user")).map(str::to_owned),
        author_type: None,
        author_association: None,
        head: head.and_then(|h| text(h.get("sha"))).map(str::to_owned),
        head_ref: head.and_then(|h| text(h.get("ref"))).map(str::to_owned),
        base: item
            .get("base")
            .and_then(serde_json::Value::as_object)
            .and_then(|b| text(b.get("ref")))
            .map(str::to_owned),
        body: item
            .get("body")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        labels: label_names(item.get("labels")),
        review_state: None,
        mergeable: match item.get("mergeable").and_then(serde_json::Value::as_bool) {
            Some(true) => Some("clean".to_owned()),
            Some(false) => Some("dirty".to_owned()),
            None => None,
        },
        checks: None,
        updated_at: text(item.get("updated_at")).map(str::to_owned),
        closed_at: text(item.get("closed_at")).map(str::to_owned),
        source_url: text(item.get("html_url")).map(str::to_owned),
    }
}

fn label_names(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|label| text(label.as_object().and_then(|o| o.get("name"))))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn subject_web_url(api_url: Option<&serde_json::Value>) -> Option<String> {
    let api_url = text(api_url)?;
    Some(api_url.replacen("/api/v1/repos/", "/", 1).to_owned())
}

/// A `RepoRef` from a URL naming one of `hosts`, else `None` — "not mine",
/// not "malformed". Owner and repo are interpolated raw into API URLs, so a
/// value carrying `..`, `?`, `#` or `%` is rejected.
pub fn parse_repo_url(repo_url: Option<&str>, hosts: &[&str]) -> Option<RepoRef> {
    if hosts.is_empty() {
        return None;
    }
    let candidate = repo_url?
        .trim()
        .strip_prefix("git+")
        .unwrap_or(repo_url?.trim());
    let (host, path) = split_host(candidate)?;
    let canonical = hosts
        .iter()
        .find(|configured| configured.eq_ignore_ascii_case(&host))?;
    let path = path.strip_suffix(".git").unwrap_or(&path).trim_matches('/');
    let mut parts = path.split('/');
    let owner = parts.next().filter(|part| valid_name(part))?;
    let repo = parts.next().filter(|part| valid_name(part))?;
    Some(RepoRef {
        host: (*canonical).to_owned(),
        owner: owner.to_owned(),
        repo: repo.to_owned(),
    })
}

/// Host and path, with the scheme, userinfo and port removed. `ssh://git@host`,
/// `ssh://git@host:2222` and `https://host:3000` all parse; a query is kept out
/// of the path but does not disqualify the URL — Forgejo tolerates one where
/// GitHub does not.
fn split_host(candidate: &str) -> Option<(String, String)> {
    let (host, path, _) = super::urls::split_repo_url(candidate)?;
    Some((host, path.to_owned()))
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.contains('%')
}

fn forge_fail(message: &str) -> VogtError {
    // A fixture names the status the way the real client does ("answered 422").
    // The digits become a status, not something create_repo sniffs back out.
    for status in [409_u16, 422, 500, 403, 401] {
        if message.contains(&status.to_string()) {
            return VogtError::ForgeHttpStatus {
                status,
                forge: Some(crate::errors::Forge::Forgejo),
                message: message.to_owned(),
            };
        }
    }
    VogtError::UpstreamWriteFailed(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fixture {
        answers: Vec<(String, ForgeResponse)>,
        seen: RefCell<Vec<String>>,
        fail_with: Option<String>,
        /// Fail only writes whose path contains this, so a two-step write can
        /// succeed at the first step and fail at the second.
        fail_paths_containing: Option<String>,
    }

    impl Fixture {
        fn new(answers: Vec<(&str, serde_json::Value)>) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|(path, value)| (path.to_owned(), ForgeResponse::Json(value)))
                    .collect(),
                seen: RefCell::new(Vec::new()),
                fail_with: None,
                fail_paths_containing: None,
            }
        }
    }

    impl ForgeTransport for Fixture {
        fn api_root(&self) -> &str {
            "https://forge.example/api/v1"
        }
        fn token(&self) -> Option<&str> {
            Some("fixture-token")
        }
        fn get(&self, path: &str, _query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
            self.seen.borrow_mut().push(path.to_owned());
            Ok(self
                .answers
                .iter()
                .find(|(candidate, _)| candidate == path)
                .map(|(_, response)| response.clone())
                .unwrap_or(ForgeResponse::Missing))
        }
        fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
            Ok(None)
        }
        fn send(
            &self,
            method: WriteMethod,
            path: &str,
            body: Option<&serde_json::Value>,
        ) -> Result<serde_json::Value, VogtError> {
            self.seen.borrow_mut().push(path.to_owned());
            if let Some(message) = &self.fail_with {
                return Err(forge_fail(message));
            }
            if self
                .fail_paths_containing
                .as_deref()
                .is_some_and(|part| path.contains(part))
            {
                return Err(VogtError::UpstreamWriteFailed("unknown label".to_owned()));
            }
            // A write answers with the created object, as the forge does: the
            // number and URL come back, the request body does not.
            let number = body
                .and_then(|body| body.get("title"))
                .map(|_| serde_json::json!(4))
                .unwrap_or(serde_json::Value::Null);
            Ok(serde_json::json!({
                "number": number,
                "html_url": format!("https://forge.example{path}"),
                "method": method.as_str(),
            }))
        }
    }

    fn provider(answers: Vec<(&str, serde_json::Value)>) -> ForgejoProvider<Fixture> {
        ForgejoProvider::new(Fixture::new(answers), "forge.example")
    }

    fn repo() -> RepoRef {
        RepoRef {
            host: "forge.example".to_owned(),
            owner: "acme".to_owned(),
            repo: "widget".to_owned(),
        }
    }

    #[test]
    fn a_null_pull_request_field_does_not_hide_the_issue() {
        let provider = provider(vec![(
            "/repos/acme/widget/issues",
            serde_json::json!([
                {"number": 4, "title": "Later", "pull_request": null, "updated_at": "2026-03-02T00:00:00Z",
                 "user": {"username": "grace"}},
                {"number": 3, "title": "Earlier", "pull_request": null, "updated_at": "2026-03-01T00:00:00Z"},
                {"number": 9, "title": "A PR", "pull_request": {"merged": false}}
            ]),
        )]);
        let issues = provider
            .issues_updated_since(&repo(), None)
            .unwrap()
            .into_iter()
            .filter_map(IssuesItem::into_issue)
            .collect::<Vec<_>>();
        assert_eq!(
            issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(issues[1].author.as_deref(), Some("grace"));
    }

    #[test]
    fn keys_are_host_qualified_and_urls_parse_for_the_configured_host_only() {
        let provider = provider(vec![]);
        assert_eq!(
            provider.subject_key(&repo(), 4),
            "forge:forge.example/acme/widget#4"
        );
        for url in [
            "git@forge.example:acme/widget.git",
            "ssh://git@forge.example/acme/widget.git",
            "ssh://git@forge.example:2222/acme/widget.git",
            "https://forge.example:3000/acme/widget",
        ] {
            let parsed = provider.parse(Some(url)).unwrap();
            assert_eq!(parsed.host, "forge.example", "{url}");
            assert_eq!(
                (parsed.owner.as_str(), parsed.repo.as_str()),
                ("acme", "widget")
            );
        }
        assert!(provider
            .parse(Some("https://github.com/acme/widget"))
            .is_none());
        assert!(provider
            .parse(Some("https://forge.example/acme/../widget"))
            .is_none());
    }

    #[test]
    fn a_running_check_has_a_status_and_no_conclusion() {
        let provider = provider(vec![(
            "/repos/acme/widget/actions/tasks",
            serde_json::json!({"workflow_runs": [
                {"name": "ci", "status": "running", "head_sha": "abc"},
                {"name": "ci", "status": "failure", "head_sha": "def"}
            ]}),
        )]);
        let checks = provider.checks(&repo()).unwrap();
        assert_eq!(checks[0].conclusion, None);
        assert_eq!(checks[1].conclusion.as_deref(), Some("failure"));
        assert!(provider.watched_ref_checks(&repo()).unwrap().is_empty());
        assert!(provider.failed_jobs(&repo(), 1).unwrap().is_empty());
    }

    #[test]
    fn a_write_failure_is_a_result_and_a_duplicate_repo_is_a_refusal() {
        let mut fixture = Fixture::new(vec![]);
        fixture.fail_with = Some("connection refused".to_owned());
        let provider = ForgejoProvider::new(fixture, "forge.example");
        let failed = provider.comment(&repo(), 4, "hello").unwrap();
        assert_eq!(failed.outcome, WriteBackOutcome::Failed);

        let mut fixture = Fixture::new(vec![]);
        fixture.fail_with = Some("Forgejo answered 409 Conflict".to_owned());
        let provider = ForgejoProvider::new(fixture, "forge.example");
        let error = provider.create_repo("widget", false, None).unwrap_err();
        assert!(matches!(error, VogtError::RemoteRepoExists(_)), "{error:?}");
    }

    #[test]
    fn create_issue_reports_labels_that_did_not_land() {
        let mut fixture = Fixture::new(vec![]);
        fixture.fail_paths_containing = Some("/labels".to_owned());
        let provider = ForgejoProvider::new(fixture, "forge.example");
        let created = provider
            .create_issue(&repo(), "Title", "body", Some(&["bug".to_owned()]))
            .unwrap();
        assert_eq!(created.outcome, WriteBackOutcome::Succeeded);
        assert!(created.detail.unwrap().contains("labels did not land"));
    }

    #[test]
    fn resolve_actor_stays_inside_its_own_api() {
        let provider = provider(vec![(
            "/repos/acme/widget/issues/4",
            serde_json::json!({"user": {"login": "ada"}}),
        )]);
        let actor = provider
            .resolve_actor("https://forge.example/api/v1/repos/acme/widget/issues/4")
            .unwrap()
            .unwrap();
        assert_eq!(actor.login.as_deref(), Some("ada"));
        assert!(actor.user_type.is_none());
        assert!(provider
            .resolve_actor("https://evil.example/api/v1/repos/acme/widget/issues/4")
            .unwrap()
            .is_none());
    }

    /// The Python suite's fake transport and recorder in one. A read answers by
    /// a fragment of its path and query, a missing fragment is a 404, and every
    /// request is recorded so a test can assert its verb, path and shape.
    /// A read's path and its query pairs, as the provider sent them.
    type RecordedRead = (String, Vec<(String, String)>);

    struct Recorder {
        routes: Vec<(String, serde_json::Value)>,
        reads: RefCell<Vec<RecordedRead>>,
        writes: RefCell<Vec<(WriteMethod, String, Option<serde_json::Value>)>>,
        write_error: Option<String>,
    }

    impl Recorder {
        fn new(routes: Vec<(&str, serde_json::Value)>) -> Self {
            Self {
                routes: routes
                    .into_iter()
                    .map(|(fragment, value)| (fragment.to_owned(), value))
                    .collect(),
                reads: RefCell::new(Vec::new()),
                writes: RefCell::new(Vec::new()),
                write_error: None,
            }
        }
    }

    impl ForgeTransport for Recorder {
        fn api_root(&self) -> &str {
            "https://forge.example/api/v1"
        }
        fn token(&self) -> Option<&str> {
            Some("x")
        }
        fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
            let query: Vec<(String, String)> = query
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect();
            let rendered = query
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&");
            let haystack = format!("{path}?{rendered}");
            self.reads.borrow_mut().push((path.to_owned(), query));
            Ok(self
                .routes
                .iter()
                .find(|(fragment, _)| haystack.contains(fragment.as_str()))
                .map_or(ForgeResponse::Missing, |(_, value)| {
                    ForgeResponse::Json(value.clone())
                }))
        }
        fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
            Ok(None)
        }
        fn send(
            &self,
            method: WriteMethod,
            path: &str,
            body: Option<&serde_json::Value>,
        ) -> Result<serde_json::Value, VogtError> {
            self.writes
                .borrow_mut()
                .push((method, path.to_owned(), body.cloned()));
            if let Some(message) = &self.write_error {
                // The real client turns a non-2xx body into ForgeHttpStatus
                // stamped with this forge; the fixture must too, or a test of
                // the unavailable mapping never reaches it.
                return Err(forge_fail(message));
            }
            // A write answers with the created object: number, URL and, for a
            // repository create, the owner and visibility.
            Ok(serde_json::json!({
                "number": 12,
                "html_url": "https://forge.example/acme/Widgets/issues/12",
                "name": "widgets",
                "owner": {"login": "acme"},
                "private": true,
                "default_branch": "main",
            }))
        }
    }

    fn recorded(routes: Vec<(&str, serde_json::Value)>) -> ForgejoProvider<Recorder> {
        ForgejoProvider::new(Recorder::new(routes), "forge.example")
    }

    fn widgets() -> RepoRef {
        RepoRef {
            host: "forge.example".to_owned(),
            owner: "acme".to_owned(),
            repo: "Widgets".to_owned(),
        }
    }

    #[test]
    fn the_provider_declares_what_the_api_offers() {
        let provider = recorded(vec![]);
        let as_dyn: &dyn ForgeProvider = &provider;
        let caps = as_dyn.capabilities();
        assert_eq!(caps.hosts, vec!["forge.example".to_owned()]);
        assert!(caps.supports_since, "the issues endpoint takes `since`");
        assert!(
            !caps.supports_posture,
            "no Dependabot-style posture surface"
        );
        assert!(caps.supports_notifications, "per-repo notifications exist");
        assert!(!caps.supports_webhooks, "deferred by name, like GitHub");
    }

    #[test]
    fn subject_keys_are_host_qualified_and_round_trip() {
        let provider = recorded(vec![]);
        let key = provider.subject_key(&widgets(), 42);
        assert_eq!(key, "forge:forge.example/acme/Widgets#42");
        assert_eq!(provider.number_of(Some(&key)), Some(42));
        assert!(
            !key.starts_with("gh:"),
            "cannot collide with github.com's legacy keys"
        );
    }

    #[test]
    fn number_of_declines_a_key_that_names_no_number() {
        let provider = recorded(vec![]);
        assert_eq!(
            provider.number_of(Some("posture:forge.example/acme/Widgets")),
            None
        );
        assert_eq!(provider.number_of(None), None);
        assert_eq!(
            provider.number_of(Some("forge:forge.example/acme/Widgets#notanumber")),
            None
        );
    }

    #[test]
    fn the_urls_and_the_clone_token_come_from_the_provider() {
        let provider = recorded(vec![]);
        assert_eq!(
            provider.clone_url(&widgets()),
            "https://forge.example/acme/Widgets.git"
        );
        assert_eq!(
            provider.web_url(&widgets()),
            "https://forge.example/acme/Widgets"
        );
        assert_eq!(
            provider.clone_token().as_deref(),
            Some("x"),
            "the clone authenticates as the token"
        );
    }

    #[test]
    fn issues_normalise_ascending_and_drop_disguised_pulls() {
        let provider = recorded(vec![(
            "/issues",
            serde_json::json!([
                {"number": 9, "title": "newer", "state": "open", "labels": [{"name": "bug"}],
                 "user": {"login": "alice"}, "updated_at": "2026-08-10T00:00:00Z",
                 "html_url": "https://forge.example/acme/Widgets/issues/9"},
                {"number": 7, "title": "older", "state": "closed", "labels": [],
                 "user": {"username": "bob"}, "updated_at": "2026-08-01T00:00:00Z",
                 "closed_at": "2026-08-01T00:00:00Z"},
                {"number": 8, "title": "a PR in disguise", "pull_request": {}}
            ]),
        )]);
        let issues = provider
            .issues_updated_since(&widgets(), None)
            .unwrap()
            .into_iter()
            .filter_map(IssuesItem::into_issue)
            .collect::<Vec<_>>();
        assert_eq!(
            issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
            vec![7, 9],
            "ascending by update, PRs dropped"
        );
        assert_eq!(issues[1].labels, vec!["bug".to_owned()]);
        assert_eq!(issues[1].author.as_deref(), Some("alice"));
        assert_eq!(
            issues[0].author.as_deref(),
            Some("bob"),
            "`username` satisfies when `login` is absent"
        );
        assert_eq!(issues[0].state, "closed");
        assert!(issues[0].closed_at.is_some());
    }

    #[test]
    fn issues_ask_for_issues_only_and_carry_since_upstream() {
        let provider = recorded(vec![]);
        provider
            .issues_updated_since(&widgets(), Some("2026-08-01T00:00:00Z"))
            .unwrap();
        let reads = provider.transport.reads.borrow();
        let (path, query) = &reads[0];
        assert_eq!(path, "/repos/acme/Widgets/issues");
        for expected in [
            ("type", "issues"),
            ("state", "all"),
            ("since", "2026-08-01T00:00:00Z"),
        ] {
            assert!(
                query
                    .iter()
                    .any(|(key, value)| (key.as_str(), value.as_str()) == expected),
                "{expected:?} missing from {query:?}"
            );
        }
    }

    #[test]
    fn pulls_filter_by_since_locally_and_yield_ascending() {
        let provider = recorded(vec![(
            "/pulls",
            serde_json::json!([
                {"number": 3, "title": "new", "state": "open", "draft": true,
                 "head": {"sha": "abc"}, "base": {"ref": "main"},
                 "updated_at": "2026-08-10T00:00:00Z"},
                {"number": 1, "title": "old", "state": "closed",
                 "updated_at": "2026-07-01T00:00:00Z"}
            ]),
        )]);
        let pulls = provider
            .pulls_updated_since(&widgets(), Some("2026-08-01T00:00:00Z"))
            .unwrap();
        assert_eq!(
            pulls.iter().map(|pull| pull.number).collect::<Vec<_>>(),
            vec![3],
            "the stale PR is filtered locally"
        );
        assert!(pulls[0].draft);
        assert_eq!(pulls[0].head.as_deref(), Some("abc"));
        assert_eq!(pulls[0].base.as_deref(), Some("main"));
    }

    #[test]
    fn releases_and_labels_normalise() {
        let provider = recorded(vec![
            (
                "/releases",
                serde_json::json!([{"tag_name": "v1.2.0", "name": "1.2.0", "prerelease": true}]),
            ),
            (
                "/labels",
                serde_json::json!([{"name": "bug", "color": "ee0701", "description": "broken"}]),
            ),
        ]);
        let releases = provider.releases(&widgets()).unwrap();
        assert_eq!(releases[0].tag, "v1.2.0");
        assert!(releases[0].prerelease);
        let labels = provider.labels(&widgets()).unwrap();
        assert_eq!(labels[0].name, "bug");
        assert_eq!(labels[0].color.as_deref(), Some("ee0701"));
    }

    #[test]
    fn action_tasks_become_generic_checks() {
        let provider = recorded(vec![(
            "/actions/tasks",
            serde_json::json!({"total_count": 2, "workflow_runs": [
                {"name": "ci", "head_sha": "abc123", "head_branch": "main",
                 "status": "failure", "run_number": 8},
                {"name": "ci", "head_sha": "def456", "status": "running"}
            ]}),
        )]);
        let checks = provider.checks(&widgets()).unwrap();
        assert_eq!(checks[0].conclusion.as_deref(), Some("failure"));
        assert_eq!(checks[1].status.as_deref(), Some("running"));
        assert_eq!(
            checks[1].conclusion, None,
            "in flight honestly has no conclusion yet"
        );
    }

    #[test]
    fn an_install_without_actions_reports_no_checks_not_an_error() {
        // No route answers, so the tasks endpoint is a 404.
        let provider = recorded(vec![]);
        assert!(provider.checks(&widgets()).unwrap().is_empty());
    }

    #[test]
    fn notifications_are_repo_scoped_and_honest_about_missing_fields() {
        let provider = recorded(vec![(
            "/notifications",
            serde_json::json!([{
                "id": 55,
                "unread": true,
                "updated_at": "2026-08-10T00:00:00Z",
                "subject": {
                    "title": "Crashes on empty segment",
                    "type": "Issue",
                    "url": "https://forge.example/api/v1/repos/acme/Widgets/issues/9"
                }
            }]),
        )]);
        let notes = provider.notifications(&widgets()).unwrap();
        assert_eq!(notes[0].thread, "55");
        assert!(notes[0].unread);
        assert_eq!(
            notes[0].source_url.as_deref(),
            Some("https://forge.example/acme/Widgets/issues/9")
        );
        // Forgejo's thread model has no reason and no per-thread read time:
        // `None`, never a guess.
        assert!(notes[0].reason.is_none() && notes[0].last_read_at.is_none());
    }

    #[test]
    fn the_posture_gap_is_three_unknowns_not_three_offs() {
        let provider = recorded(vec![]);
        assert!(!provider.capabilities().supports_posture);
        let posture = provider.posture(&widgets()).unwrap();
        assert!(posture.version_updates_config.is_none());
        assert!(posture.vulnerability_alerts.is_none());
        assert!(posture.automated_security_fixes.is_none());
    }

    #[test]
    fn list_repos_enumerates_what_the_credential_sees() {
        let provider = recorded(vec![(
            "/user/repos",
            serde_json::json!([
                {"name": "Widgets", "owner": {"login": "acme"}, "private": true,
                 "default_branch": "main", "html_url": "https://forge.example/acme/Widgets"},
                {"name": "orphan"}
            ]),
        )]);
        let repos = provider.list_repos().unwrap();
        assert_eq!(
            repos.iter().map(|repo| repo.slug()).collect::<Vec<_>>(),
            vec!["acme/Widgets".to_owned()],
            "an entry with no owner is skipped, not invented"
        );
        assert_eq!(repos[0].visibility, "private");
        assert_eq!(repos[0].web_url, "https://forge.example/acme/Widgets");
    }

    #[test]
    fn comment_posts_and_reports_the_host_qualified_key() {
        let provider = recorded(vec![]);
        let result = provider.comment(&widgets(), 5, "hello").unwrap();
        assert_eq!(result.outcome, WriteBackOutcome::Succeeded);
        assert_eq!(
            result.subject_key.as_deref(),
            Some("forge:forge.example/acme/Widgets#12")
        );
        let writes = provider.transport.writes.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, WriteMethod::Post);
        assert_eq!(writes[0].2, Some(serde_json::json!({"body": "hello"})));
    }

    #[test]
    fn create_issue_appends_twice_because_labels_ride_their_own_endpoint() {
        let provider = recorded(vec![]);
        let result = provider
            .create_issue(&widgets(), "t", "b", Some(&["bug".to_owned()]))
            .unwrap();
        assert_eq!(result.outcome, WriteBackOutcome::Succeeded);
        let writes = provider.transport.writes.borrow();
        assert_eq!(
            writes.iter().map(|write| write.0).collect::<Vec<_>>(),
            vec![WriteMethod::Post, WriteMethod::Post]
        );
        assert!(
            writes[1].1.ends_with("/issues/12/labels"),
            "{}",
            writes[1].1
        );
        assert_eq!(
            writes[0].2,
            Some(serde_json::json!({"title": "t", "body": "b"}))
        );
        assert_eq!(writes[1].2, Some(serde_json::json!({"labels": ["bug"]})));
    }

    #[test]
    fn add_labels_appends_and_never_replaces() {
        let provider = recorded(vec![]);
        provider
            .add_labels(&widgets(), 5, &["triage".to_owned()])
            .unwrap();
        let writes = provider.transport.writes.borrow();
        assert!(!writes.is_empty());
        assert!(
            writes.iter().all(|write| write.0 == WriteMethod::Post),
            "POST appends; the type has no replacing verb to speak"
        );
    }

    #[test]
    fn set_state_patches_and_refuses_a_non_state() {
        let provider = recorded(vec![]);
        let closed = provider.set_state(&widgets(), 5, "closed").unwrap();
        assert_eq!(closed.outcome, WriteBackOutcome::Succeeded);
        {
            let writes = provider.transport.writes.borrow();
            let last = writes.last().unwrap();
            assert_eq!(last.0, WriteMethod::Patch);
            assert_eq!(last.2, Some(serde_json::json!({"state": "closed"})));
        }
        let refused = provider.set_state(&widgets(), 5, "deleted").unwrap_err();
        assert!(
            matches!(refused, VogtError::InvalidRequest(_)),
            "{refused:?}"
        );
    }

    #[test]
    fn an_upstream_refusal_is_a_failed_result_not_an_exception() {
        // Never fatal to the declared write: the ledger records the miss.
        let mut provider = recorded(vec![]);
        provider.transport.write_error =
            Some("Forgejo answered 500 Internal Server Error".to_owned());
        let result = provider.comment(&widgets(), 5, "hello").unwrap();
        assert_eq!(result.outcome, WriteBackOutcome::Failed);
        assert!(result.detail.is_some_and(|detail| detail.contains("500")));
    }

    #[test]
    fn create_repo_lands_under_the_credential_without_auto_init() {
        let provider = recorded(vec![]);
        let repo = provider.create_repo("widgets", true, Some("d")).unwrap();
        assert_eq!(repo.slug(), "acme/widgets");
        assert_eq!(repo.visibility, "private");
        let writes = provider.transport.writes.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, WriteMethod::Post);
        assert_eq!(writes[0].1, "/user/repos");
        let body = writes[0].2.as_ref().unwrap();
        assert_eq!(
            body["auto_init"],
            serde_json::json!(false),
            "the first push must be clean"
        );
        assert_eq!(body["description"], serde_json::json!("d"));
    }

    #[test]
    fn create_repo_turns_any_other_refusal_into_forgejo_unavailable() {
        // A 500 is neither 409 nor 422. Python re-raises ForgejoUnavailable,
        // whose code is forgejo_unavailable, not upstream_write_failed.
        let mut provider = recorded(vec![]);
        provider.transport.write_error = Some("Forgejo answered 500".to_owned());
        let error = provider.create_repo("widgets", true, None).unwrap_err();
        assert_eq!(error.code(), "forgejo_unavailable", "{error:?}");
        assert!(matches!(error, VogtError::ForgejoUnavailable(_)));
    }

    #[test]
    fn an_empty_description_is_omitted_not_sent_blank() {
        let provider = recorded(vec![]);
        provider.create_repo("widgets", false, Some("")).unwrap();
        let writes = provider.transport.writes.borrow();
        assert!(writes[0].2.as_ref().unwrap().get("description").is_none());
    }

    #[test]
    fn parse_repo_url_rejects_injection_metacharacters() {
        // Owner and repo are interpolated raw into API URLs, so a crafted
        // `repo_url` must read as "not mine", never as a ref.
        let hosts = ["forge.example"];
        let ok = parse_repo_url(Some("https://forge.example/acme/Widgets"), &hosts).unwrap();
        assert_eq!((ok.owner.as_str(), ok.repo.as_str()), ("acme", "Widgets"));
        for bad in [
            "https://forge.example/a?x=1/b",
            "https://forge.example/../../user/repos",
            "https://forge.example/o/..",
            "https://forge.example/o/r%2f..",
            "forge.example/o/r%23frag",
        ] {
            assert!(parse_repo_url(Some(bad), &hosts).is_none(), "{bad}");
        }
    }
}
