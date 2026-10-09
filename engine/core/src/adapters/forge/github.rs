//! The GitHub implementation of [`ForgeProvider`](super::provider::ForgeProvider).
//! Ports `adapters/forge/github.py`.
//!
//! A thin wrapper over a [`ForgeTransport`](super::transport::ForgeTransport).
//! The real client arrives with WI-1064; this module owns the *shape* Vogt
//! speaks, and tests drive it with recorded fixtures. Nothing here opens a
//! connection, and forge bodies stay data.

use super::models::{
    ForgeActor, ForgeCapabilities, ForgeCheck, ForgeComparison, ForgeIssue, ForgeJob, ForgeLabel,
    ForgeNotification, ForgePosture, ForgePull, ForgeRelease, ForgeRepo, RepoRef,
};
use super::payloads::{comparison, decoded_content, quote_path};
use super::provider::ForgeProvider;
use super::transport::{api_path, as_list, ForgeResponse, ForgeTransport};
use super::writeback::{WriteBackOutcome, WriteBackResult};
use crate::errors::VogtError;

/// The one host this provider answers for.
pub const HOST: &str = "github.com";

const DEFAULT_PER_PAGE: &str = "100";
const WATCHED_RUNS_PAGE: &str = "50";
const ORG_MEMBER_PAGES: i64 = 10;

const VERSION_UPDATE_CONFIGS: &[&str] = &[
    "renovate.json",
    "renovate.json5",
    ".github/renovate.json",
    ".github/renovate.json5",
    ".renovaterc",
    ".renovaterc.json",
    ".github/dependabot.yml",
    ".github/dependabot.yaml",
];

/// What GitHub can do, declared rather than probed.
pub fn capabilities() -> ForgeCapabilities {
    ForgeCapabilities {
        hosts: vec![HOST.to_owned()],
        supports_since: true,
        supports_posture: true,
        supports_notifications: true,
        // Deferred by name in the v1 ceiling; the flag exists so a caller can
        // branch on it rather than special-casing GitHub.
        supports_webhooks: false,
    }
}

pub struct GitHubProvider<T: ForgeTransport> {
    transport: T,
    capabilities: ForgeCapabilities,
}

impl<T: ForgeTransport> GitHubProvider<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            capabilities: capabilities(),
        }
    }
}

impl<T: ForgeTransport> ForgeProvider for GitHubProvider<T> {
    fn capabilities(&self) -> &ForgeCapabilities {
        &self.capabilities
    }

    fn list_repos(&self) -> Result<Vec<ForgeRepo>, VogtError> {
        let response = self.transport.get(
            "/user/repos",
            &[
                ("per_page", DEFAULT_PER_PAGE.to_owned()),
                ("sort", "pushed".to_owned()),
                ("direction", "desc".to_owned()),
                (
                    "affiliation",
                    "owner,collaborator,organization_member".to_owned(),
                ),
            ],
        )?;
        let mut repos = Vec::new();
        for item in as_list(&response) {
            let Some(name) = text(item.get("name")) else {
                continue;
            };
            let Some(owner) = item
                .get("owner")
                .and_then(|v| v.as_object())
                .and_then(|o| text(o.get("login")))
            else {
                continue;
            };
            repos.push(ForgeRepo {
                owner: owner.to_owned(),
                name: name.to_owned(),
                default_branch: text(item.get("default_branch")).map(str::to_owned),
                visibility: if item
                    .get("private")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    "private"
                } else {
                    "public"
                }
                .to_owned(),
                web_url: text(item.get("html_url"))
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("https://{HOST}/{owner}/{name}")),
            });
        }
        Ok(repos)
    }

    fn parse(&self, repo_url: Option<&str>) -> Option<RepoRef> {
        let (owner, repo) = repo_of(repo_url)?;
        Some(RepoRef {
            host: HOST.to_owned(),
            owner,
            repo,
        })
    }

    fn subject_key(&self, repo: &RepoRef, number: i64) -> String {
        format!("gh:{}/{}#{number}", repo.owner, repo.repo)
    }

    fn number_of(&self, subject_key: Option<&str>) -> Option<i64> {
        let tail = subject_key?.split_once('#')?.1;
        tail.parse()
            .ok()
            .filter(|_| tail.chars().all(|c| c.is_ascii_digit()))
    }

    fn clone_url(&self, repo: &RepoRef) -> String {
        format!("https://{HOST}/{}/{}.git", repo.owner, repo.repo)
    }

    fn web_url(&self, repo: &RepoRef) -> String {
        format!("https://{HOST}/{}/{}", repo.owner, repo.repo)
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

    fn clone_token(&self) -> Option<&str> {
        self.transport.token()
    }

    fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
        self.transport.identity()
    }

    fn issues_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<ForgeIssue>, VogtError> {
        // Ascending by update time so a watermark walks history forward.
        let mut query = vec![
            ("state", "all".to_owned()),
            ("sort", "updated".to_owned()),
            ("direction", "asc".to_owned()),
            ("per_page", DEFAULT_PER_PAGE.to_owned()),
        ];
        if let Some(since) = since {
            query.push(("since", since.to_owned()));
        }
        let response = self.transport.get(
            &format!("/repos/{}/{}/issues", repo.owner, repo.repo),
            &query,
        )?;
        Ok(as_list(&response)
            .into_iter()
            .filter(|item| !item.contains_key("pull_request"))
            .map(|item| to_issue(repo, item))
            .collect())
    }

    fn pulls_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<ForgePull>, VogtError> {
        // No server-side `since`: most-recently-updated first, filtered locally,
        // then yielded ascending so the watermark advances as it does for issues.
        let response = self.transport.get(
            &format!("/repos/{}/{}/pulls", repo.owner, repo.repo),
            &[
                ("state", "all".to_owned()),
                ("sort", "updated".to_owned()),
                ("direction", "desc".to_owned()),
                ("per_page", DEFAULT_PER_PAGE.to_owned()),
            ],
        )?;
        let mut pulls: Vec<ForgePull> = as_list(&response)
            .into_iter()
            .map(|item| to_pull(repo, item))
            .collect();
        if let Some(since) = since {
            pulls.retain(|pull| pull.updated_at.as_deref().is_none_or(|at| at >= since));
        }
        pulls.reverse();
        Ok(pulls)
    }

    fn releases(&self, repo: &RepoRef) -> Result<Vec<ForgeRelease>, VogtError> {
        let response = self.transport.get(
            &format!("/repos/{}/{}/releases", repo.owner, repo.repo),
            &[("per_page", DEFAULT_PER_PAGE.to_owned())],
        )?;
        Ok(as_list(&response)
            .into_iter()
            .map(|item| ForgeRelease {
                tag: text(item.get("tag_name")).unwrap_or("").to_owned(),
                repo: repo.slug(),
                name: text(item.get("name")).map(str::to_owned),
                draft: item
                    .get("draft")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                prerelease: item
                    .get("prerelease")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                published_at: text(item.get("published_at")).map(str::to_owned),
                source_url: text(item.get("html_url")).map(str::to_owned),
            })
            .collect())
    }

    fn checks(&self, repo: &RepoRef) -> Result<Vec<ForgeCheck>, VogtError> {
        let response = self.transport.get(
            &format!("/repos/{}/{}/actions/runs", repo.owner, repo.repo),
            &[("per_page", "20".to_owned())],
        )?;
        Ok(runs(repo, &response))
    }

    fn watched_ref_checks(&self, repo: &RepoRef) -> Result<Vec<ForgeCheck>, VogtError> {
        // `event=push` is every branch and tag push and never a pull-request run.
        let response = self.transport.get(
            &format!("/repos/{}/{}/actions/runs", repo.owner, repo.repo),
            &[
                ("per_page", WATCHED_RUNS_PAGE.to_owned()),
                ("event", "push".to_owned()),
            ],
        )?;
        Ok(runs(repo, &response))
    }

    fn failed_jobs(&self, repo: &RepoRef, run_id: i64) -> Result<Vec<ForgeJob>, VogtError> {
        let response = self.transport.get(
            &format!(
                "/repos/{}/{}/actions/runs/{run_id}/jobs",
                repo.owner, repo.repo
            ),
            &[
                ("filter", "latest".to_owned()),
                ("per_page", DEFAULT_PER_PAGE.to_owned()),
            ],
        )?;
        let jobs = match &response {
            ForgeResponse::Json(value) => value.get("jobs").and_then(serde_json::Value::as_array),
            _ => None,
        };
        let mut failed = Vec::new();
        for job in jobs
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_object)
        {
            let conclusion = text(job.get("conclusion"));
            if matches!(
                conclusion,
                None | Some("success" | "skipped" | "neutral" | "cancelled")
            ) {
                continue;
            }
            failed.push(ForgeJob {
                name: text(job.get("name")).unwrap_or("job").to_owned(),
                conclusion: conclusion.map(str::to_owned),
                source_url: text(job.get("html_url")).map(str::to_owned),
            });
        }
        Ok(failed)
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
            &[("per_page", DEFAULT_PER_PAGE.to_owned())],
        )?;
        Ok(as_list(&response)
            .into_iter()
            .filter_map(|item| {
                let name = text(item.get("name"))?;
                Some(ForgeLabel {
                    name: name.to_owned(),
                    repo: repo.slug(),
                    color: text(item.get("color")).map(str::to_owned),
                    description: text(item.get("description")).map(str::to_owned),
                })
            })
            .collect())
    }

    fn posture(&self, repo: &RepoRef) -> Result<ForgePosture, VogtError> {
        Ok(ForgePosture {
            version_updates_config: self.version_update_config(repo)?,
            vulnerability_alerts: self.toggle(repo, "vulnerability-alerts")?,
            automated_security_fixes: self.toggle(repo, "automated-security-fixes")?,
            repo: repo.slug(),
        })
    }

    fn notifications(&self, repo: &RepoRef) -> Result<Vec<ForgeNotification>, VogtError> {
        // Per-repository, never the account-wide inbox. `all=true` keeps read
        // threads; `unread` is a field, not a collection-time decision.
        let response = self.transport.get(
            &format!("/repos/{}/{}/notifications", repo.owner, repo.repo),
            &[
                ("per_page", DEFAULT_PER_PAGE.to_owned()),
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
                    reason: text(item.get("reason")).map(str::to_owned),
                    unread: item
                        .get("unread")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    title: subject
                        .and_then(|s| text(s.get("title")))
                        .unwrap_or("")
                        .to_owned(),
                    subject_type: subject.and_then(|s| text(s.get("type"))).map(str::to_owned),
                    updated_at: text(item.get("updated_at")).map(str::to_owned),
                    last_read_at: text(item.get("last_read_at")).map(str::to_owned),
                    source_url: notification_web_url(subject_url),
                    subject_api_url: text(subject_url).map(str::to_owned),
                    latest_comment_api_url: subject
                        .and_then(|s| text(s.get("latest_comment_url")))
                        .map(str::to_owned),
                }
            })
            .collect())
    }

    fn resolve_actor(&self, api_url: &str) -> Result<Option<ForgeActor>, VogtError> {
        let Some(path) = api_path(api_url, self.transport.api_root()) else {
            return Ok(None);
        };
        let response = self.transport.get(&path, &[])?;
        let ForgeResponse::Json(serde_json::Value::Object(payload)) = response else {
            return Ok(None);
        };
        Ok(actor_of(&payload))
    }

    fn org_members(&self, owner: &str) -> Result<Option<Vec<String>>, VogtError> {
        let mut members: Vec<String> = Vec::new();
        for page in 1..=ORG_MEMBER_PAGES {
            let response = self.transport.get(
                &format!("/orgs/{owner}/members"),
                &[
                    ("per_page", DEFAULT_PER_PAGE.to_owned()),
                    ("page", page.to_string()),
                ],
            )?;
            if matches!(response, ForgeResponse::Missing) {
                return Ok(if page == 1 { None } else { Some(members) });
            }
            let batch = as_list(&response);
            let count = batch.len();
            for item in batch {
                if let Some(login) = text(item.get("login")) {
                    members.push(login.to_ascii_lowercase());
                }
            }
            if count < DEFAULT_PER_PAGE.parse::<usize>().unwrap_or(100) {
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
        self.write(
            repo,
            &format!(
                "/repos/{}/{}/issues/{number}/comments",
                repo.owner, repo.repo
            ),
            &serde_json::json!({"body": body}),
            Some(number),
            "POST",
        )
    }

    fn create_issue(
        &self,
        repo: &RepoRef,
        title: &str,
        body: &str,
        labels: Option<&[String]>,
    ) -> Result<WriteBackResult, VogtError> {
        // An empty list is "no labels": the key is omitted, matching the client
        // that only sends the field when there is something to attach.
        let mut payload = serde_json::json!({"title": title, "body": body});
        if let Some(labels) = labels.filter(|labels| !labels.is_empty()) {
            payload["labels"] = serde_json::json!(labels);
        }
        self.write(
            repo,
            &format!("/repos/{}/{}/issues", repo.owner, repo.repo),
            &payload,
            None,
            "POST",
        )
    }

    fn add_labels(
        &self,
        repo: &RepoRef,
        number: i64,
        labels: &[String],
    ) -> Result<WriteBackResult, VogtError> {
        // Adds only: POST appends, it never replaces the set.
        self.write(
            repo,
            &format!("/repos/{}/{}/issues/{number}/labels", repo.owner, repo.repo),
            &serde_json::json!({"labels": labels}),
            Some(number),
            "POST",
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
                "{state:?} is not a state; use 'closed' or 'open'"
            )));
        }
        self.write(
            repo,
            &format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo),
            &serde_json::json!({"state": state}),
            Some(number),
            "PATCH",
        )
    }

    fn update_issue_body(
        &self,
        repo: &RepoRef,
        number: i64,
        body: &str,
    ) -> Result<WriteBackResult, VogtError> {
        self.write(
            repo,
            &format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo),
            &serde_json::json!({"body": body}),
            Some(number),
            "PATCH",
        )
    }

    fn create_repo(
        &self,
        name: &str,
        private: bool,
        description: Option<&str>,
    ) -> Result<ForgeRepo, VogtError> {
        // Under the acting actor's token the repository lands in their account.
        // A name that already exists is a 422, and that is a typed refusal, never
        // an adoption of the existing repository.
        let mut payload = serde_json::json!({
            "name": name,
            "private": private,
            "auto_init": false,
        });
        if let Some(description) = description.filter(|text| !text.is_empty()) {
            payload["description"] = serde_json::Value::String(description.to_owned());
        }
        let response = match self.transport.send("POST", "/user/repos", Some(&payload)) {
            Err(error) if error.message().contains("422") => {
                return Err(VogtError::RemoteRepoExists(format!(
                    "github.com already has a repository named {name:?} reachable by this \
                     account; `forge.publish` never adopts or overwrites an existing remote — \
                     pick another name, or attach to the existing repository with `forge link` \
                     after setting the project's repo_url"
                )));
            }
            other => other?,
        };
        let owner = response
            .get("owner")
            .and_then(|v| v.as_object())
            .and_then(|o| text(o.get("login")))
            .ok_or_else(|| {
                VogtError::UpstreamWriteFailed(
                    "GitHub accepted the repository create but returned no owner, so there is no \
                     address to push to"
                        .to_owned(),
                )
            })?;
        let created = text(response.get("name")).unwrap_or(name);
        Ok(ForgeRepo {
            owner: owner.to_owned(),
            name: created.to_owned(),
            default_branch: text(response.get("default_branch")).map(str::to_owned),
            visibility: if response
                .get("private")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(private)
            {
                "private"
            } else {
                "public"
            }
            .to_owned(),
            web_url: text(response.get("html_url"))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("https://{HOST}/{owner}/{created}")),
        })
    }
}

impl<T: ForgeTransport> GitHubProvider<T> {
    /// One append upstream. A transport failure or a 404 is a `failed` result,
    /// never fatal to the declared write: the local change stands and the
    /// ledger records that the upstream half did not land. A success carries
    /// the subject key (`gh:{owner}/{repo}#{number}`) and the method and
    /// endpoint that produced it, which is what a projection reads back.
    fn write(
        &self,
        repo: &RepoRef,
        endpoint: &str,
        payload: &serde_json::Value,
        number: Option<i64>,
        method: &str,
    ) -> Result<WriteBackResult, VogtError> {
        let response = match self.transport.send(method, endpoint, Some(payload)) {
            Err(error) => return Ok(WriteBackResult::failed(error.message().to_owned())),
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
            detail: Some(serde_json::json!({"method": method, "endpoint": endpoint}).to_string()),
        })
    }

    fn version_update_config(&self, repo: &RepoRef) -> Result<Option<String>, VogtError> {
        for path in VERSION_UPDATE_CONFIGS {
            match self.transport.get(
                &format!("/repos/{}/{}/contents/{path}", repo.owner, repo.repo),
                &[],
            ) {
                Err(_) => return Ok(None),
                Ok(ForgeResponse::Missing | ForgeResponse::Empty) => {}
                Ok(_) => return Ok(Some((*path).to_owned())),
            }
        }
        Ok(None)
    }

    /// 204 is on, 404 is off, and a transport error is "could not tell".
    fn toggle(&self, repo: &RepoRef, endpoint: &str) -> Result<Option<bool>, VogtError> {
        match self.transport.get(
            &format!("/repos/{}/{}/{endpoint}", repo.owner, repo.repo),
            &[],
        ) {
            Err(_) => Ok(None),
            Ok(ForgeResponse::Missing) => Ok(Some(false)),
            Ok(_) => Ok(Some(true)),
        }
    }
}

impl ForgeResponse {
    fn into_text(self) -> Option<String> {
        match self {
            Self::Json(serde_json::Value::String(text)) => Some(text),
            _ => None,
        }
    }
}

fn text(value: Option<&serde_json::Value>) -> Option<&str> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
}

fn json_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn to_issue(repo: &RepoRef, item: &serde_json::Map<String, serde_json::Value>) -> ForgeIssue {
    let user = item.get("user").and_then(serde_json::Value::as_object);
    ForgeIssue {
        number: item
            .get("number")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        title: text(item.get("title")).unwrap_or("").to_owned(),
        state: text(item.get("state")).unwrap_or("open").to_owned(),
        repo: repo.slug(),
        labels: names(item.get("labels")),
        author: user.and_then(|u| text(u.get("login"))).map(str::to_owned),
        author_type: user.and_then(|u| text(u.get("type"))).map(str::to_owned),
        author_association: text(item.get("author_association")).map(str::to_owned),
        assignees: item
            .get("assignees")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|a| text(a.as_object().and_then(|o| o.get("login"))))
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
        source_url: text(item.get("html_url")).map(str::to_owned),
    }
}

fn to_pull(repo: &RepoRef, item: &serde_json::Map<String, serde_json::Value>) -> ForgePull {
    let user = item.get("user").and_then(serde_json::Value::as_object);
    let head = item.get("head").and_then(serde_json::Value::as_object);
    let status = item.get("status").and_then(serde_json::Value::as_object);
    ForgePull {
        number: item
            .get("number")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        title: text(item.get("title")).unwrap_or("").to_owned(),
        state: text(item.get("state")).unwrap_or("open").to_owned(),
        repo: repo.slug(),
        draft: item
            .get("draft")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        // A merged PR reads `state: closed`; the list page omits `merged` but
        // carries `merged_at`, so read either.
        merged: item
            .get("merged")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || text(item.get("merged_at")).is_some(),
        author: user.and_then(|u| text(u.get("login"))).map(str::to_owned),
        author_type: user.and_then(|u| text(u.get("type"))).map(str::to_owned),
        author_association: text(item.get("author_association")).map(str::to_owned),
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
        labels: names(item.get("labels")),
        review_state: None,
        mergeable: text(item.get("mergeable_state")).map(str::to_owned),
        checks: status.and_then(|s| text(s.get("state"))).map(str::to_owned),
        updated_at: text(item.get("updated_at")).map(str::to_owned),
        closed_at: text(item.get("closed_at")).map(str::to_owned),
        source_url: text(item.get("html_url")).map(str::to_owned),
    }
}

fn names(value: Option<&serde_json::Value>) -> Vec<String> {
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

fn actor_of(payload: &serde_json::Map<String, serde_json::Value>) -> Option<ForgeActor> {
    let user = payload
        .get("user")
        .or_else(|| payload.get("author"))?
        .as_object()?;
    Some(ForgeActor {
        login: text(user.get("login")).map(str::to_owned),
        user_type: text(user.get("type")).map(str::to_owned),
        association: text(payload.get("author_association")).map(str::to_owned),
    })
}

fn notification_web_url(api_url: Option<&serde_json::Value>) -> Option<String> {
    let api_url = text(api_url)?;
    let Some((_, tail)) = api_url.split_once("api.github.com/repos/") else {
        return Some(api_url.to_owned());
    };
    if tail.is_empty() {
        return Some(api_url.to_owned());
    }
    Some(format!(
        "https://github.com/{}",
        tail.replace("/pulls/", "/pull/")
    ))
}

fn runs(repo: &RepoRef, response: &ForgeResponse) -> Vec<ForgeCheck> {
    let items = match response {
        ForgeResponse::Json(value) => value
            .get("workflow_runs")
            .and_then(serde_json::Value::as_array),
        _ => None,
    };
    items
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
        .map(|item| {
            let mut check = ForgeCheck::new(
                text(item.get("head_sha")).unwrap_or(""),
                text(item.get("name")).unwrap_or("workflow"),
                repo.slug(),
            );
            check.status = text(item.get("status")).map(str::to_owned);
            check.conclusion = text(item.get("conclusion")).map(str::to_owned);
            check.branch = text(item.get("head_branch")).map(str::to_owned);
            check.event = text(item.get("event")).map(str::to_owned);
            check.run_number = item.get("run_number").and_then(serde_json::Value::as_i64);
            check.workflow_path = text(item.get("path")).map(str::to_owned);
            check.updated_at = text(item.get("updated_at")).map(str::to_owned);
            check.source_url = text(item.get("html_url")).map(str::to_owned);
            check.run_id = item.get("id").and_then(serde_json::Value::as_i64);
            check.run_attempt = item.get("run_attempt").and_then(serde_json::Value::as_i64);
            check
        })
        .collect()
}

/// `(owner, repo)` from a repository URL, or `None` when it is not GitHub.
///
/// The host is `urlsplit().hostname`: lowercased, with userinfo and port
/// stripped, so `https://GitHub.com/…`, `:443` and `user@github.com` all
/// resolve. A query or a fragment disqualifies the URL, matching the client
/// that refuses to guess which repository a parameterised URL names.
pub fn repo_of(repo_url: Option<&str>) -> Option<(String, String)> {
    let raw = repo_url?.trim();
    // urlparse drops tabs, carriage returns and newlines before it reads a
    // URL, so a repository pasted across a line break still resolves.
    let raw: String = raw
        .chars()
        .filter(|ch| !matches!(ch, '\t' | '\r' | '\n'))
        .collect();
    let candidate = raw.strip_prefix("git+").unwrap_or(&raw);
    // Python strips only https, http and ssh, and also accepts the bare
    // `github.com/owner/repo` form. Any other scheme, and an scp host whose
    // case isn't exactly `github.com`, is not a GitHub repo.
    let has_scheme = candidate.contains("://");
    let allowed = ["https://", "http://", "ssh://"]
        .iter()
        .any(|scheme| candidate.starts_with(scheme))
        || candidate.starts_with("git@github.com:")
        || !has_scheme;
    if !allowed {
        return None;
    }
    let (host, path, has_query) = super::urls::split_repo_url(candidate)?;
    // A trailing `?` or `#` with nothing after it is an empty query and an
    // empty fragment, so it doesn't disqualify the URL. Anything after either
    // one does, which is what urlsplit reports.
    let query_and_fragment_empty = candidate
        .split(['?', '#'])
        .skip(1)
        .all(|rest| rest.is_empty());
    if (has_query && !query_and_fragment_empty) || !host.eq_ignore_ascii_case(HOST) {
        return None;
    }
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    let mut parts = path.split('/');
    let owner = parts.next().filter(|part| valid_name(part))?;
    let repo = parts.next().filter(|part| valid_name(part))?;
    Some((owner.to_owned(), repo.to_owned()))
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fixture transport: paths map to recorded answers, and every call is
    /// remembered so a test can assert which URL the token would have followed.
    struct Fixture {
        answers: Vec<(String, ForgeResponse)>,
        writes: Vec<(String, serde_json::Value)>,
        seen: RefCell<Vec<String>>,
        sent: RefCell<Vec<serde_json::Value>>,
        fail_with: Option<String>,
    }

    impl Fixture {
        fn new(answers: Vec<(&str, serde_json::Value)>) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|(path, value)| (path.to_owned(), ForgeResponse::Json(value)))
                    .collect(),
                writes: Vec::new(),
                seen: RefCell::new(Vec::new()),
                sent: RefCell::new(Vec::new()),
                fail_with: None,
            }
        }
    }

    impl ForgeTransport for Fixture {
        fn api_root(&self) -> &str {
            "https://api.github.com"
        }
        fn token(&self) -> Option<&str> {
            Some("fixture-token")
        }
        fn get(&self, path: &str, _query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
            self.seen.borrow_mut().push(path.to_owned());
            if let Some(message) = &self.fail_with {
                return Err(VogtError::UpstreamWriteFailed(message.clone()));
            }
            Ok(self
                .answers
                .iter()
                .find(|(candidate, _)| candidate == path)
                .map(|(_, response)| response.clone())
                .unwrap_or(ForgeResponse::Missing))
        }
        fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
            Ok(Some(("ada".to_owned(), String::new())))
        }
        fn send(
            &self,
            _method: &str,
            path: &str,
            body: Option<&serde_json::Value>,
        ) -> Result<serde_json::Value, VogtError> {
            self.seen.borrow_mut().push(path.to_owned());
            if let Some(body) = body {
                self.sent.borrow_mut().push(body.clone());
            }
            if let Some(message) = &self.fail_with {
                return Err(VogtError::UpstreamWriteFailed(message.clone()));
            }
            Ok(self
                .writes
                .iter()
                .find(|(candidate, _)| candidate == path)
                .map(|(_, value)| value.clone())
                .unwrap_or(serde_json::Value::Null))
        }
    }

    fn provider() -> GitHubProvider<Fixture> {
        GitHubProvider::new(Fixture::new(vec![(
            "/repos/acme/widget/issues",
            serde_json::json!([
                {"number": 7, "title": "Real issue", "state": "open", "user": {"login": "ada", "type": "User"},
                 "labels": [{"name": "bug"}], "updated_at": "2026-01-02T00:00:00Z"},
                {"number": 8, "title": "Actually a PR", "pull_request": {}}
            ]),
        )]))
    }

    #[test]
    fn issues_skip_pull_requests_and_keep_the_shape() {
        let issues = provider().issues_updated_since(&repo(), None).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 7);
        assert_eq!(issues[0].labels, vec!["bug".to_owned()]);
        assert_eq!(issues[0].author.as_deref(), Some("ada"));
        assert_eq!(issues[0].repo, "acme/widget");
    }

    #[test]
    fn subject_keys_round_trip_and_reject_junk() {
        let provider = provider();
        assert_eq!(provider.subject_key(&repo(), 7), "gh:acme/widget#7");
        assert_eq!(provider.number_of(Some("gh:acme/widget#123")), Some(123));
        assert_eq!(provider.number_of(Some("gh:acme/widget")), None);
        assert_eq!(provider.number_of(Some("gh:acme/widget#nope")), None);
    }

    #[test]
    fn parse_accepts_the_real_forms_and_rejects_lookalikes() {
        let provider = provider();
        for url in [
            "https://github.com/acme/widget",
            "https://GitHub.com/acme/widget",
            "https://github.com:443/acme/widget",
            "https://ada@github.com/acme/widget",
            "git@github.com:acme/widget.git",
            "https://github.com/acme/widget.git",
            "git+https://github.com/acme/widget",
        ] {
            let parsed = provider.parse(Some(url)).unwrap();
            assert_eq!(
                (parsed.owner.as_str(), parsed.repo.as_str()),
                ("acme", "widget")
            );
        }
        for url in [
            "https://github.com.evil.example/acme/widget",
            "https://github.com@evil.example/acme/widget",
            "https://gitlab.com/acme/widget",
            "https://github.com/acme/widget?inject=1",
            "https://github.com/acme/widget#f",
            "https://evil.example?@github.com/acme/widget",
            "https://evil.example#@github.com/acme/widget",
            "https://github.com/acme",
        ] {
            assert!(provider.parse(Some(url)).is_none(), "{url}");
        }
    }

    #[test]
    fn resolve_actor_refuses_a_url_outside_its_own_api() {
        let mut fixture = Fixture::new(vec![(
            "/repos/acme/widget/issues/7",
            serde_json::json!({"user": {"login": "ada", "type": "User"}, "author_association": "MEMBER"}),
        )]);
        fixture.fail_with = None;
        let provider = GitHubProvider::new(fixture);
        let actor = provider
            .resolve_actor("https://api.github.com/repos/acme/widget/issues/7")
            .unwrap()
            .unwrap();
        assert_eq!(actor.login.as_deref(), Some("ada"));
        assert_eq!(actor.association.as_deref(), Some("MEMBER"));
        // A payload must not be able to steer the token elsewhere.
        assert!(provider
            .resolve_actor("https://evil.example/repos/acme/widget")
            .unwrap()
            .is_none());
        assert!(provider
            .resolve_actor("https://api.github.com/repos/../secrets")
            .unwrap()
            .is_none());
        let seen = provider.transport.seen.borrow().clone();
        assert_eq!(seen, vec!["/repos/acme/widget/issues/7".to_owned()]);
    }

    #[test]
    fn a_write_reports_the_subject_key_and_the_endpoint() {
        let mut fixture = Fixture::new(vec![]);
        fixture.writes.push((
            "/repos/acme/widget/issues".to_owned(),
            serde_json::json!({"number": 4, "html_url": "https://github.com/acme/widget/issues/4"}),
        ));
        let result = GitHubProvider::new(fixture)
            .create_issue(&repo(), "Title", "Body", Some(&[]))
            .unwrap();
        assert_eq!(result.outcome, WriteBackOutcome::Succeeded);
        assert_eq!(result.subject_key.as_deref(), Some("gh:acme/widget#4"));
        let detail = result.detail.unwrap();
        assert!(detail.contains("\"method\":\"POST\""), "{detail}");
        assert!(detail.contains("/repos/acme/widget/issues"), "{detail}");
    }

    #[test]
    fn a_write_failure_is_a_failed_result_not_an_error() {
        let mut fixture = Fixture::new(vec![]);
        fixture.fail_with = Some("GitHub answered 500".to_owned());
        let result = GitHubProvider::new(fixture)
            .comment(&repo(), 4, "hello")
            .unwrap();
        assert_eq!(result.outcome, WriteBackOutcome::Failed);
        assert!(result.detail.unwrap().contains("500"));
    }

    #[test]
    fn a_404_on_a_write_is_a_failed_result() {
        let result = provider().comment(&repo(), 4, "hello").unwrap();
        assert_eq!(result.outcome, WriteBackOutcome::Failed);
        assert!(result.detail.unwrap().contains("404?"));
    }

    #[test]
    fn set_state_refuses_an_unknown_state() {
        let error = provider().set_state(&repo(), 4, "merged").unwrap_err();
        assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    }

    #[test]
    fn create_repo_omits_an_empty_description() {
        let mut fixture = Fixture::new(vec![]);
        fixture.writes.push((
            "/user/repos".to_owned(),
            serde_json::json!({"name": "widget", "full_name": "ada/widget", "owner": {"login": "ada"}}),
        ));
        let provider = GitHubProvider::new(fixture);
        provider.create_repo("widget", true, None).unwrap();
        let sent = provider.transport.sent.borrow();
        assert!(
            sent[0].get("description").is_none(),
            "no description: {}",
            sent[0]
        );
        assert_eq!(sent[0]["auto_init"], false);
        let provider = GitHubProvider::new({
            let mut fixture = Fixture::new(vec![]);
            fixture.writes.push((
                "/user/repos".to_owned(),
                serde_json::json!({"name": "widget", "owner": {"login": "ada"}}),
            ));
            fixture
        });
        provider
            .create_repo("widget", false, Some("A widget"))
            .unwrap();
        assert_eq!(
            provider.transport.sent.borrow()[0]["description"],
            "A widget"
        );
    }

    #[test]
    fn create_repo_turns_a_422_into_a_typed_refusal() {
        let mut fixture = Fixture::new(vec![]);
        fixture.fail_with = Some("GitHub answered 422 Unprocessable Entity".to_owned());
        let provider = GitHubProvider::new(fixture);
        let error = provider.create_repo("widget", true, None).unwrap_err();
        assert!(matches!(error, VogtError::RemoteRepoExists(_)), "{error:?}");
    }

    #[test]
    fn a_merged_pull_reads_merged_from_either_field() {
        let provider = GitHubProvider::new(Fixture::new(vec![(
            "/repos/acme/widget/pulls",
            serde_json::json!([
                {"number": 3, "title": "Landed", "state": "closed", "merged_at": "2026-02-01T00:00:00Z",
                 "updated_at": "2026-02-01T00:00:00Z", "head": {"sha": "abc", "ref": "wi-3"}, "base": {"ref": "main"}},
                {"number": 2, "title": "Older", "state": "closed", "updated_at": "2026-01-01T00:00:00Z"}
            ]),
        )]));
        let pulls = provider
            .pulls_updated_since(&repo(), Some("2026-01-15T00:00:00Z"))
            .unwrap();
        assert_eq!(pulls.len(), 1);
        assert!(pulls[0].merged);
        assert_eq!(pulls[0].head_ref.as_deref(), Some("wi-3"));
    }

    #[test]
    fn failed_jobs_drop_the_ones_that_did_not_fail() {
        let provider = GitHubProvider::new(Fixture::new(vec![(
            "/repos/acme/widget/actions/runs/5/jobs",
            serde_json::json!({"jobs": [
                {"name": "test", "conclusion": "failure", "html_url": "https://github.com/acme/widget/runs/5"},
                {"name": "lint", "conclusion": "success"},
                {"name": "still-going", "conclusion": null}
            ]}),
        )]));
        let jobs = provider.failed_jobs(&repo(), 5).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "test");
    }

    fn repo() -> RepoRef {
        RepoRef {
            host: HOST.to_owned(),
            owner: "acme".to_owned(),
            repo: "widget".to_owned(),
        }
    }
}
