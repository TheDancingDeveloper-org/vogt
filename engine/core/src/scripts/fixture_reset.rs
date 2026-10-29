//! `scripts/fixture_reset.py` in Rust.
//!
//! Rebuilds the forge fixture repository to the manifest through the GitHub
//! API. Idempotent: find by a stable key and create only when absent. Additive
//! with respect to history: close, reopen, relabel, comment, and create refs.
//! There is no delete method on the client — GET, POST, PATCH and PUT only —
//! and nothing force-pushes or rewrites a commit.
//!
//! `--dry-run` prints the plan from the manifest and touches no network.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use serde_json::{Map, Value};

const API_ROOT: &str = "https://api.github.com";
const USER_AGENT: &str = "vogt-fixture-reset";
const TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_REPO: &str = "TheDancingDeveloper-org/vogt-fixture";

const FAILING_WORKFLOW: &str = "\
name: fixture-fail
on: [push, pull_request]
jobs:
  fail:
    runs-on: ubuntu-latest
    steps:
      - run: exit 1
";

/// Methods this rebuild is allowed to send. No delete variant exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Get,
    Post,
    Patch,
    Put,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
        }
    }

    fn from_write(name: &str) -> Result<Self, String> {
        match name {
            "POST" => Ok(Method::Post),
            "PATCH" => Ok(Method::Patch),
            "PUT" => Ok(Method::Put),
            other => Err(format!(
                "refusing {other}: fixture-reset only sends GET, POST, PATCH and PUT"
            )),
        }
    }
}

struct BuiltRequest {
    method: Method,
    url: String,
    body: Option<String>,
}

/// Build one GitHub request. Rejects anything that is not GET/POST/PATCH/PUT.
fn build_request(
    method: Method,
    path_or_url: &str,
    payload: Option<&Value>,
) -> Result<BuiltRequest, String> {
    let url = if path_or_url.starts_with("http") {
        path_or_url.to_string()
    } else {
        format!("{API_ROOT}{path_or_url}")
    };
    let body = match payload {
        Some(value) => Some(
            serde_json::to_string(value)
                .map_err(|err| format!("could not encode request body: {err}"))?,
        ),
        None => None,
    };
    Ok(BuiltRequest { method, url, body })
}

struct Options {
    repo: String,
    repo_set: bool,
    token: String,
    token_file: String,
    dry_run: bool,
    manifest: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        repo: String::new(),
        repo_set: false,
        token: String::new(),
        token_file: String::new(),
        dry_run: false,
        manifest: None,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--dry-run" {
            opts.dry_run = true;
            index += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let needs_value = matches!(
            name.as_str(),
            "--repo" | "--token" | "--token-file" | "--manifest"
        );
        if !needs_value {
            return Err(format!("unrecognised argument: {arg}"));
        }
        let value = if let Some(value) = inline {
            value
        } else {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))?
        };
        match name.as_str() {
            "--repo" => {
                opts.repo = value;
                opts.repo_set = true;
            }
            "--token" => opts.token = value,
            "--token-file" => opts.token_file = value,
            "--manifest" => opts.manifest = Some(value),
            _ => return Err(format!("unrecognised argument: {arg}")),
        }
        index += 1;
    }
    Ok(opts)
}

fn load_manifest(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let data: Value =
        serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    if !data.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }
    Ok(data)
}

fn plan_text(manifest: &Value) -> String {
    let expected = manifest.get("expected").cloned().unwrap_or(Value::Null);
    let labels = arr_len(manifest, "labels");
    let milestones = arr_len(manifest, "milestones");
    let issues = arr_len(manifest, "issues");
    let pulls = arr_len(manifest, "pulls");
    let branches = arr_len(manifest, "branches");
    let open = expected.get("issues_open").unwrap_or(&Value::Null);
    let closed = expected.get("issues_closed").unwrap_or(&Value::Null);
    let draft = expected.get("pulls_draft").unwrap_or(&Value::Null);
    let closes = expected.get("pulls_with_closes").unwrap_or(&Value::Null);
    format!(
        "\
plan (no network):
  labels:     {labels}
  milestones: {milestones}
  issues:     {issues} ({open} open, {closed} closed)
  pulls:      {pulls} ({draft} draft, {closes} closes-linked)
  branches:   {branches} (declared-branch pattern)
  content:    markers, dependency manifest, posture
"
    )
}

fn arr_len(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

/// Token order: `--token-file`, then `--token`, then `$VOGT_FIXTURE_TOKEN`,
/// then `$GH_TOKEN`. An empty file falls through.
fn resolve_token_from(
    token_file: &str,
    token: &str,
    fixture_env: &str,
    gh_env: &str,
) -> Result<String, String> {
    if !token_file.is_empty() {
        let path = Path::new(token_file);
        let expanded = if let Some(rest) = token_file.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                Path::new(&home).join(rest)
            } else {
                path.to_path_buf()
            }
        } else {
            path.to_path_buf()
        };
        let read = std::fs::read_to_string(&expanded)
            .map_err(|err| format!("{}: {err}", expanded.display()))?;
        let trimmed = read.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    if !token.is_empty() {
        return Ok(token.to_string());
    }
    for value in [fixture_env, gh_env] {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    Err(
        "no token: pass --token-file/--token or set VOGT_FIXTURE_TOKEN / GH_TOKEN \
         (needs 'repo', plus 'workflow' for the failing-checks pull request)"
            .to_string(),
    )
}

fn choose_repo(opts: &Options, manifest: &Value) -> Result<String, String> {
    if opts.repo_set {
        if opts.repo.is_empty() {
            return Err("no repo: pass --repo or set `repo` in the manifest".to_string());
        }
        return Ok(opts.repo.clone());
    }
    if let Ok(from_env) = std::env::var("VOGT_FIXTURE_REPO") {
        let trimmed = from_env.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    if let Some(repo) = manifest.get("repo").and_then(Value::as_str) {
        if !repo.is_empty() {
            return Ok(repo.to_string());
        }
    }
    Ok(DEFAULT_REPO.to_string())
}

struct Api {
    repo: String,
    token: String,
    agent: ureq::Agent,
}

impl Api {
    fn new(repo: String, token: String) -> Self {
        let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
        Self { repo, token, agent }
    }

    fn repo_path(&self, suffix: &str) -> String {
        format!("/repos/{}{suffix}", self.repo)
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<(u16, Value), String> {
        let built = build_request(method, path, payload)?;
        let mut req = self
            .agent
            .request(built.method.as_str(), &built.url)
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28")
            .set("User-Agent", USER_AGENT)
            .set("Authorization", &format!("Bearer {}", self.token));
        let response = if let Some(body) = &built.body {
            req = req.set("Content-Type", "application/json");
            req.send_string(body)
        } else {
            req.call()
        };
        match response {
            Ok(resp) => read_response(resp.status(), resp.into_reader()),
            Err(ureq::Error::Status(code, resp)) => read_response(code, resp.into_reader()),
            Err(err) => Err(format!(
                "{} {path} could not reach GitHub: {err}",
                method.as_str()
            )),
        }
    }

    fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Option<Value>, String> {
        let full = with_query(path, params);
        let (status, body) = self.request(Method::Get, &full, None)?;
        if status == 404 {
            return Ok(None);
        }
        if status >= 400 {
            return Err(format!("GET {full} -> {status}: {body}"));
        }
        Ok(Some(body))
    }

    fn get_all(&self, path: &str, extra: &[(&str, String)]) -> Result<Vec<Value>, String> {
        let mut collected = Vec::new();
        let mut page = 1u32;
        loop {
            let mut params = vec![("per_page", "100".to_string()), ("page", page.to_string())];
            params.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
            let batch = self.get(path, &params)?;
            let Some(Value::Array(items)) = batch else {
                break;
            };
            if items.is_empty() {
                break;
            }
            let len = items.len();
            collected.extend(items.into_iter().filter(|value| value.is_object()));
            if len < 100 {
                break;
            }
            page += 1;
        }
        Ok(collected)
    }

    fn write(&self, method: &str, path: &str, payload: &Value) -> Result<Value, String> {
        let method = Method::from_write(method)?;
        let (status, body) = self.request(method, path, Some(payload))?;
        if status >= 400 {
            return Err(format!("{} {path} -> {status}: {body}", method.as_str()));
        }
        Ok(body)
    }
}

fn read_response(status: u16, mut reader: impl Read) -> Result<(u16, Value), String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|err| format!("reading GitHub response: {err}"))?;
    Ok((status, parse_body(&bytes)))
}

fn parse_body(body: &[u8]) -> Value {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(trimmed.to_string()))
}

fn with_query(path: &str, params: &[(&str, String)]) -> String {
    if params.is_empty() {
        return path.to_string();
    }
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", encode_query(key), encode_query(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{query}")
}

/// `urllib.parse.urlencode`: quote_plus, so `/` stays and a space is `+`.
fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `urllib.parse.quote(value, safe="")`.
fn quote(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn ensure_labels(api: &Api, manifest: &Value) -> Result<(), String> {
    let existing = api.get_all(&api.repo_path("/labels"), &[])?;
    let mut names = BTreeMap::new();
    for item in &existing {
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            if !name.is_empty() {
                names.insert(name.to_string(), item.clone());
            }
        }
    }
    let labels = as_array(manifest, "labels")?;
    for label in labels {
        let name = required_str(label, "name")?;
        let body = json_obj([
            ("name", Value::String(name.clone())),
            ("color", Value::String(required_str(label, "color")?)),
            (
                "description",
                Value::String(optional_str(label, "description")),
            ),
        ]);
        if names.contains_key(&name) {
            api.write(
                "PATCH",
                &api.repo_path(&format!("/labels/{}", quote(&name))),
                &body,
            )?;
        } else {
            api.write("POST", &api.repo_path("/labels"), &body)?;
        }
    }
    println!("labels: {} ensured", labels.len());
    Ok(())
}

fn ensure_milestones(api: &Api, manifest: &Value) -> Result<BTreeMap<String, i64>, String> {
    let existing = api.get_all(
        &api.repo_path("/milestones"),
        &[("state", "all".to_string())],
    )?;
    let mut by_title = BTreeMap::new();
    for item in &existing {
        if let Some(title) = item.get("title").and_then(Value::as_str) {
            if !title.is_empty() {
                if let Some(number) = json_i64(item.get("number")) {
                    by_title.insert(title.to_string(), number);
                }
            }
        }
    }
    let mut numbers = BTreeMap::new();
    let milestones = as_array(manifest, "milestones")?;
    for milestone in milestones {
        let title = required_str(milestone, "title")?;
        let state = if milestone.get("state").and_then(Value::as_str).is_some() {
            optional_str(milestone, "state")
        } else {
            "open".to_string()
        };
        let body = json_obj([
            ("title", Value::String(title.clone())),
            ("state", Value::String(state)),
            (
                "description",
                Value::String(optional_str(milestone, "description")),
            ),
        ]);
        let number = if let Some(number) = by_title.get(&title).copied() {
            api.write(
                "PATCH",
                &api.repo_path(&format!("/milestones/{number}")),
                &body,
            )?;
            number
        } else {
            let created = api.write("POST", &api.repo_path("/milestones"), &body)?;
            json_i64(created.get("number")).unwrap_or(0)
        };
        numbers.insert(title, number);
    }
    println!("milestones: {} ensured", milestones.len());
    Ok(numbers)
}

fn ensure_issues(
    api: &Api,
    manifest: &Value,
    milestones: &BTreeMap<String, i64>,
) -> Result<(), String> {
    let listed = api.get_all(&api.repo_path("/issues"), &[("state", "all".to_string())])?;
    let mut by_title = BTreeMap::new();
    for item in listed {
        let title = item.get("title").and_then(Value::as_str).unwrap_or("");
        if title.is_empty() || item.get("pull_request").is_some() {
            continue;
        }
        by_title.insert(title.to_string(), item);
    }
    let issues = as_array(manifest, "issues")?;
    for issue in issues {
        let title = required_str(issue, "title")?;
        let milestone_title = issue.get("milestone").and_then(Value::as_str);
        let milestone_number = milestone_title.and_then(|title| milestones.get(title).copied());
        let milestone_number = milestone_number.filter(|number| *number != 0);
        let mut current = by_title.get(&title).cloned();
        if current.is_none() {
            let mut body = Map::new();
            body.insert("title".to_string(), Value::String(title.clone()));
            body.insert(
                "body".to_string(),
                Value::String(optional_str(issue, "body")),
            );
            body.insert(
                "labels".to_string(),
                issue
                    .get("labels")
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
            );
            if let Some(number) = milestone_number {
                body.insert("milestone".to_string(), Value::from(number));
            }
            current = Some(api.write("POST", &api.repo_path("/issues"), &Value::Object(body))?);
        }
        let Some(number) = current.as_ref().and_then(number_of) else {
            continue;
        };
        if let Some(Value::Array(labels)) = issue.get("labels") {
            if !labels.is_empty() {
                api.write(
                    "POST",
                    &api.repo_path(&format!("/issues/{number}/labels")),
                    &json_obj([("labels", Value::Array(labels.clone()))]),
                )?;
            }
        }
        let mut patch = Map::new();
        let state = issue.get("state").and_then(Value::as_str).unwrap_or("open");
        patch.insert("state".to_string(), Value::String(state.to_string()));
        if let Some(milestone) = milestone_number {
            patch.insert("milestone".to_string(), Value::from(milestone));
        }
        api.write(
            "PATCH",
            &api.repo_path(&format!("/issues/{number}")),
            &Value::Object(patch),
        )?;
    }
    println!("issues: {} ensured", issues.len());
    Ok(())
}

fn default_branch_sha(api: &Api, manifest: &Value) -> Result<String, String> {
    let branch = required_str(manifest, "default_branch")?;
    let reference = api.get(&api.repo_path(&format!("/git/ref/heads/{branch}")), &[])?;
    let Some(reference) = reference.filter(|value| value.is_object()) else {
        return Err(format!(
            "default branch {branch:?} has no ref — create the repository and \
             push an initial commit before running this script"
        ));
    };
    reference
        .get("object")
        .and_then(|object| object.get("sha"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("default branch {branch:?} ref has no sha"))
}

fn ensure_file(
    api: &Api,
    path: &str,
    contents: &str,
    branch: &str,
    message: &str,
) -> Result<(), String> {
    let existing = api.get(
        &api.repo_path(&format!("/contents/{path}")),
        &[("ref", branch.to_string())],
    )?;
    let mut body = Map::new();
    body.insert("message".to_string(), Value::String(message.to_string()));
    body.insert(
        "content".to_string(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(contents.as_bytes())),
    );
    body.insert("branch".to_string(), Value::String(branch.to_string()));
    if let Some(existing) = existing.as_ref().filter(|value| value.is_object()) {
        if let Some(sha) = existing.get("sha").and_then(Value::as_str) {
            if !sha.is_empty() {
                if let Some(current) = existing.get("content").and_then(Value::as_str) {
                    if let Some(decoded) = decode_content(current) {
                        if decoded == contents {
                            return Ok(());
                        }
                    }
                }
                body.insert("sha".to_string(), Value::String(sha.to_string()));
            }
        }
    }
    api.write(
        "PUT",
        &api.repo_path(&format!("/contents/{path}")),
        &Value::Object(body),
    )?;
    Ok(())
}

fn decode_content(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|ch| !ch.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn ensure_branch(api: &Api, name: &str, base_sha: &str) -> Result<(), String> {
    let existing = api.get(
        &api.repo_path(&format!("/git/ref/heads/{}", quote(name))),
        &[],
    )?;
    if existing
        .as_ref()
        .is_some_and(|value| value.is_object() && value.get("object").is_some())
    {
        return Ok(());
    }
    api.write(
        "POST",
        &api.repo_path("/git/refs"),
        &json_obj([
            ("ref", Value::String(format!("refs/heads/{name}"))),
            ("sha", Value::String(base_sha.to_string())),
        ]),
    )?;
    Ok(())
}

fn ensure_content_files(api: &Api, manifest: &Value) -> Result<(), String> {
    let branch = required_str(manifest, "default_branch")?;
    let markers = manifest
        .get("markers")
        .ok_or_else(|| "manifest has no markers".to_string())?;
    for spec in as_array(markers, "files")? {
        ensure_file(
            api,
            &required_str(spec, "path")?,
            &required_str(spec, "contents")?,
            &branch,
            "fixture: TODO/FIXME markers",
        )?;
    }
    let dep = manifest
        .get("dependency_manifest")
        .ok_or_else(|| "manifest has no dependency_manifest".to_string())?;
    ensure_file(
        api,
        &required_str(dep, "path")?,
        &required_str(dep, "contents")?,
        &branch,
        "fixture: deps",
    )?;
    if let Some(posture) = manifest.get("posture").filter(|value| value.is_object()) {
        ensure_file(
            api,
            &required_str(posture, "path")?,
            &required_str(posture, "contents")?,
            &branch,
            "fixture: posture",
        )?;
    }
    println!("content files: markers, dependency manifest, posture ensured");
    Ok(())
}

fn ensure_branches(api: &Api, manifest: &Value, base_sha: &str) -> Result<(), String> {
    let branches = as_array(manifest, "branches")?;
    for spec in branches {
        ensure_branch(api, &required_str(spec, "name")?, base_sha)?;
    }
    println!(
        "branches: {} ensured (declared-branch pattern)",
        branches.len()
    );
    Ok(())
}

fn open_pull_for(api: &Api, head: &str) -> Result<Option<Value>, String> {
    let owner = api
        .repo
        .split_once('/')
        .map(|(owner, _)| owner)
        .unwrap_or(api.repo.as_str());
    let listed = api.get_all(
        &api.repo_path("/pulls"),
        &[
            ("state", "all".to_string()),
            ("head", format!("{owner}:{head}")),
        ],
    )?;
    Ok(listed.into_iter().next())
}

fn ensure_pulls(api: &Api, manifest: &Value) -> Result<(), String> {
    let base = required_str(manifest, "default_branch")?;
    let pulls = as_array(manifest, "pulls")?;
    for pull in pulls {
        let head = required_str(pull, "branch")?;
        let key = required_str(pull, "key")?;
        let title = required_str(pull, "title")?;
        ensure_file(
            api,
            &format!("fixtures/{key}.txt"),
            &format!("Fixture pull request: {title}\n"),
            &head,
            &format!("fixture: {key} change"),
        )?;
        if pull.get("checks").and_then(Value::as_str) == Some("failure") {
            ensure_file(
                api,
                ".github/workflows/fixture-fail.yml",
                FAILING_WORKFLOW,
                &head,
                "fixture: a workflow that fails",
            )?;
        }
        let mut existing = open_pull_for(api, &head)?;
        if existing.is_none() {
            let pull_base = pull
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or(base.as_str());
            existing = Some(api.write(
                "POST",
                &api.repo_path("/pulls"),
                &json_obj([
                    ("title", Value::String(title)),
                    ("head", Value::String(head)),
                    ("base", Value::String(pull_base.to_string())),
                    ("body", Value::String(optional_str(pull, "body"))),
                    (
                        "draft",
                        Value::Bool(pull.get("draft").and_then(Value::as_bool).unwrap_or(false)),
                    ),
                ]),
            )?);
        }
        let Some(number) = existing.as_ref().and_then(number_of) else {
            continue;
        };
        if pull.get("merged").and_then(Value::as_bool) == Some(true) && !is_merged(api, number)? {
            api.write(
                "PUT",
                &api.repo_path(&format!("/pulls/{number}/merge")),
                &json_obj([("merge_method", Value::String("squash".to_string()))]),
            )?;
        }
    }
    println!("pulls: {} ensured", pulls.len());
    Ok(())
}

fn is_merged(api: &Api, number: i64) -> Result<bool, String> {
    let (status, _) = api.request(
        Method::Get,
        &api.repo_path(&format!("/pulls/{number}/merge")),
        None,
    )?;
    Ok(status == 204)
}

fn ensure_writeback_probe(api: &Api, manifest: &Value) -> Result<(), String> {
    let Some(target) = manifest
        .get("writeback_target")
        .filter(|value| value.is_object())
    else {
        return Ok(());
    };
    let number = json_i64(target.get("issue_number"))
        .ok_or_else(|| "writeback_target.issue_number is not an integer".to_string())?;
    let marker = required_str(target, "comment_marker")?;
    let comments = api.get_all(&api.repo_path(&format!("/issues/{number}/comments")), &[])?;
    for comment in &comments {
        let body = comment.get("body").and_then(Value::as_str).unwrap_or("");
        if body.contains(&marker) {
            println!("writeback probe: already present");
            return Ok(());
        }
    }
    api.write(
        "POST",
        &api.repo_path(&format!("/issues/{number}/comments")),
        &json_obj([(
            "body",
            Value::String(format!(
                "{marker}\n\nSeed comment so the live write-back test has a target."
            )),
        )]),
    )?;
    println!("writeback probe: seeded");
    Ok(())
}

fn number_of(payload: &Value) -> Option<i64> {
    json_i64(payload.get("number"))
}

fn json_i64(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|n| i64::try_from(n).ok())),
        _ => None,
    }
}

fn as_array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("manifest field `{key}` is not an array"))
}

fn required_str(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("manifest field `{key}` is not a string"))
}

fn optional_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn json_obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// Rebuild the fixture. `root` is the checkout; `args` are the CLI flags
/// (without the subcommand name).
pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let opts = parse_args(args)?;
    let manifest_path = match &opts.manifest {
        Some(path) => Path::new(path).to_path_buf(),
        None => root.join("tests/fixtures/forge_fixture_manifest.json"),
    };
    let manifest = load_manifest(&manifest_path)?;
    let repo = choose_repo(&opts, &manifest)?;
    if opts.dry_run {
        println!("rebuilding {repo} (dry run)");
        print!("{}", plan_text(&manifest));
        return Ok(());
    }
    let fixture_env = std::env::var("VOGT_FIXTURE_TOKEN").unwrap_or_default();
    let gh_env = std::env::var("GH_TOKEN").unwrap_or_default();
    let token = resolve_token_from(&opts.token_file, &opts.token, &fixture_env, &gh_env)?;
    let api = Api::new(repo.clone(), token);
    println!("rebuilding {repo} (live)");
    ensure_labels(&api, &manifest)?;
    let milestones = ensure_milestones(&api, &manifest)?;
    ensure_content_files(&api, &manifest)?;
    let base_sha = default_branch_sha(&api, &manifest)?;
    ensure_branches(&api, &manifest, &base_sha)?;
    ensure_pulls(&api, &manifest)?;
    ensure_issues(&api, &manifest, &milestones)?;
    ensure_writeback_probe(&api, &manifest)?;
    println!("done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_manifest() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/forge_fixture_manifest.json");
        load_manifest(&path).expect("real fixture manifest parses")
    }

    #[test]
    fn parses_real_manifest() {
        let manifest = real_manifest();
        assert_eq!(
            manifest.get("repo").and_then(Value::as_str),
            Some("TheDancingDeveloper-org/vogt-fixture")
        );
        assert_eq!(arr_len(&manifest, "labels"), 5);
        assert_eq!(arr_len(&manifest, "issues"), 5);
        assert_eq!(arr_len(&manifest, "pulls"), 4);
        assert_eq!(arr_len(&manifest, "branches"), 4);
        let first = &manifest["issues"][0];
        assert_eq!(first["key"], "open-bug");
        assert_eq!(first["title"], "Crash on an empty config file");
        assert!(manifest["writeback_target"]["comment_marker"]
            .as_str()
            .unwrap()
            .contains("writeback probe"));
    }

    #[test]
    fn rejects_a_non_object_manifest() {
        let dir = std::env::temp_dir().join(format!(
            "fixture-reset-manifest-{}-{}",
            std::process::id(),
            "array"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("m.json");
        std::fs::write(&path, "[1, 2]").unwrap();
        let err = load_manifest(&path).unwrap_err();
        assert!(err.contains("is not a JSON object"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_resolution_order() {
        let dir = std::env::temp_dir().join(format!("fixture-reset-token-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("pat");
        std::fs::write(&file, " from-file \n").unwrap();
        let file_s = file.display().to_string();
        assert_eq!(
            resolve_token_from(&file_s, "from-flag", "from-fixture", "from-gh").unwrap(),
            "from-file"
        );
        std::fs::write(&file, "   \n").unwrap();
        assert_eq!(
            resolve_token_from(&file_s, "from-flag", "from-fixture", "from-gh").unwrap(),
            "from-flag"
        );
        assert_eq!(
            resolve_token_from("", "from-flag", "from-fixture", "from-gh").unwrap(),
            "from-flag"
        );
        assert_eq!(
            resolve_token_from("", "", "from-fixture", "from-gh").unwrap(),
            "from-fixture"
        );
        assert_eq!(
            resolve_token_from("", "", "  ", "from-gh").unwrap(),
            "from-gh"
        );
        let missing = resolve_token_from("", "", "", "  ").unwrap_err();
        assert!(missing.contains("no token"), "{missing}");
        assert!(missing.contains("VOGT_FIXTURE_TOKEN"), "{missing}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_plan_text_for_the_real_manifest() {
        let manifest = real_manifest();
        let text = format!(
            "rebuilding {} (dry run)\n{}",
            manifest["repo"].as_str().unwrap(),
            plan_text(&manifest)
        );
        assert_eq!(
            text,
            "\
rebuilding TheDancingDeveloper-org/vogt-fixture (dry run)
plan (no network):
  labels:     5
  milestones: 2
  issues:     5 (3 open, 2 closed)
  pulls:      4 (1 draft, 1 closes-linked)
  branches:   4 (declared-branch pattern)
  content:    markers, dependency manifest, posture
"
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // `run` prints the same plan and returns before any HTTP client is built.
        let err = run(
            &root,
            &[
                "--dry-run".to_string(),
                "--repo".to_string(),
                "".to_string(),
            ],
        );
        // An explicit empty --repo is the Python "no repo" error, not a network call.
        assert!(err.unwrap_err().contains("no repo"));
        let missing = run(
            Path::new("/nonexistent-fixture-root"),
            &[
                "--dry-run".to_string(),
                "--manifest".to_string(),
                "/no/such/manifest.json".to_string(),
            ],
        );
        assert!(missing.unwrap_err().contains("manifest.json"));
    }

    #[test]
    fn request_builder_never_emits_delete() {
        for method in [Method::Get, Method::Post, Method::Patch, Method::Put] {
            let payload = if method == Method::Get {
                None
            } else {
                Some(json_obj([("name", Value::String("bug".to_string()))]))
            };
            let built = build_request(method, "/repos/o/r/labels", payload.as_ref()).unwrap();
            assert_ne!(built.method.as_str(), "DELETE");
            assert!(matches!(
                built.method,
                Method::Get | Method::Post | Method::Patch | Method::Put
            ));
        }
        let refused = Method::from_write("DELETE").unwrap_err();
        assert!(refused.contains("DELETE"), "{refused}");
        assert!(Method::from_write("delete").is_err());
        // Method has no Delete variant, and from_write refuses the verb.
        let verbs = [
            Method::Get.as_str(),
            Method::Post.as_str(),
            Method::Patch.as_str(),
            Method::Put.as_str(),
        ];
        assert!(!verbs.contains(&"DELETE"));
        assert_eq!(verbs.len(), 4);
    }
}
