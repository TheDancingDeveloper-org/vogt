//! Host → provider resolution. Ports `adapters/forge/registry.py`.
//!
//! Two kinds of host live here. github.com is a constant: one host, known
//! without configuration. A Forgejo host is data: any host named under
//! `forge_token_files` that is not github.com is read as a Forgejo/Gitea
//! installation, so the set of supported hosts is not knowable without the
//! config. A call that omits it gets the constant half of the answer only.
//!
//! A provider resolves to nothing for two different reasons, and the caller
//! that cares about the difference asks [`unsupported_reason`] first:
//!
//! - **Unsupported host** — no forge here reads it. A permanent fact about the
//!   URL, which is why an empty consolidation must never read as "there is
//!   nothing".
//! - **Not configured** — the host is supported but has no usable token file,
//!   so it is "not collected". Registering only when a token exists is the
//!   honesty mechanism: an always-present collector that fails on the network
//!   would pin the whole estate's freshness to `partial`.
//!
//! What this layer builds is a [`ForgeClient`]: the two concrete credentialed
//! providers, owned, so a directory can hand one out and a collector can
//! borrow it through the object-safe [`ForgeProvider`] trait. A caller that
//! needs a transport fake passes one; the real path passes `None` and the
//! client talks HTTP.

use std::path::{Path, PathBuf};

use super::forgejo::parse_repo_url;
use super::forgejo::ForgejoProvider;
use super::github::{repo_of, GitHubProvider, HOST as GITHUB_HOST};
use super::models::RepoRef;
use super::provider::ForgeProvider;
use crate::adapters::github::GitHubClient;
use crate::adapters::{forgejo::ForgejoClient, github::Transport};
use crate::config::VogtConfig;
use crate::errors::VogtError;

/// The two credentialed providers this build can speak.
///
/// Owned, so a directory can hand one out and a collector can borrow it. The
/// transport is the real client; a test that needs a fake builds its own
/// provider rather than substituting one in here.
pub enum ForgeClient {
    GitHub(GitHubProvider<GitHubClient>),
    Forgejo(ForgejoProvider<ForgejoClient>),
}

/// Every host some registered provider can read under `config`.
///
/// Without a config only the constant hosts answer — github.com — because a
/// Forgejo host exists as a fact about a configuration, not this build.
pub fn supported_hosts(config: Option<&VogtConfig>) -> Vec<String> {
    let mut hosts = vec![GITHUB_HOST.to_owned()];
    hosts.extend(forgejo_hosts(config));
    hosts
}

/// Every Forgejo host this configuration names.
///
/// The `forge_token_files` map is the declaration: a host in it is a Forgejo
/// installation unless it is github.com, whose entry is only ever a token
/// override for the constant host. No config, no Forgejo hosts — the honest
/// answer, not a default.
pub fn forgejo_hosts(config: Option<&VogtConfig>) -> Vec<String> {
    config
        .map(|config| {
            config
                .forge_token_files
                .keys()
                .filter(|host| host.as_str() != GITHUB_HOST)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Which kind of forge a repository belongs to, if any does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    GitHub,
    Forgejo,
}

/// A repository matched to the forge that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedRepo {
    pub kind: ForgeKind,
    pub repo: RepoRef,
}

/// The forge a URL belongs to, or `None` when no registered forge reads it.
///
/// GitHub is always a candidate. Forgejo matches only the hosts the config
/// declares, so the same URL answers differently under two configurations.
pub fn match_repo(repo_url: Option<&str>, config: Option<&VogtConfig>) -> Option<MatchedRepo> {
    if let Some((owner, repo)) = repo_of(repo_url) {
        return Some(MatchedRepo {
            kind: ForgeKind::GitHub,
            repo: RepoRef {
                host: GITHUB_HOST.to_owned(),
                owner,
                repo,
            },
        });
    }
    let hosts = forgejo_hosts(config);
    let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
    parse_repo_url(repo_url, &host_refs).map(|repo| MatchedRepo {
        kind: ForgeKind::Forgejo,
        repo,
    })
}

/// The token file configured for `host`, honouring the github.com alias.
///
/// `forge_token_files` is the general map; `github_token_file` remains the name
/// for github.com, so an existing deployment keeps working with no TOML change
/// and a new host is one line under `[forge_token_files]`. A host set in the
/// map wins over the alias.
pub fn token_file_for<'a>(config: &'a VogtConfig, host: &str) -> Option<&'a Path> {
    config
        .forge_token_files
        .get(host)
        .map(PathBuf::as_path)
        .or_else(|| {
            (host == GITHUB_HOST)
                .then_some(config.github_token_file.as_deref())
                .flatten()
        })
}

/// The token a file holds.
///
/// `Ok(None)` is the ordinary case, not an error: no file, a missing file, a
/// path that is not a file, or an empty file all mean the host's subjects are
/// simply not collected. A file that exists but cannot be read, or is not valid
/// UTF-8, is an error: reporting it as unconfigured would hide a deployment
/// that is configured but unreadable. The path is expanded the way the Python
/// client expands it, so `~` names the home directory rather than a literal
/// directory.
pub fn read_token(path: Option<&Path>) -> Result<Option<String>, VogtError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let resolved = expand_user(path);
    if !resolved.is_file() {
        return Ok(None);
    }
    let token = std::fs::read_to_string(&resolved).map_err(|error| {
        VogtError::GitHubUnavailable(format!(
            "cannot read token file {}: {error}",
            resolved.display()
        ))
    })?;
    let token = token.trim().to_owned();
    Ok((!token.is_empty()).then_some(token))
}

/// Why no forge can read this repository, or `None` if one can.
///
/// An empty success is the failure mode this exists to prevent. The reader of
/// an empty consolidation treats it as a signal, so the signal carries its own
/// cause. "Supported" is partly configuration: a Forgejo host is readable
/// exactly when `forge_token_files` names it, so pass the config wherever one
/// exists — without it a configured Forgejo host is misreported.
pub fn unsupported_reason(repo_url: Option<&str>, config: Option<&VogtConfig>) -> Option<String> {
    if repo_url.map(str::trim).unwrap_or("").is_empty() {
        return Some(
            "this project declares no repository URL, so there is no forge to \
             read — which is 'not collected', not 'there is nothing'"
                .to_owned(),
        );
    }
    if match_repo(repo_url, config).is_some() {
        return None;
    }
    let host = repo_url
        .unwrap_or("")
        .rsplit("://")
        .next()
        .unwrap_or("")
        .split('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(repo_url.unwrap_or(""));
    let readable = supported_hosts(config).join(", ");
    Some(format!(
        "no configured forge reads {host}; this build reads {readable} only, \
         so nothing was collected here and no conclusion should be drawn from \
         the counts"
    ))
}

/// The provider for `repo_url`, or `None` when there is not one.
///
/// `None` folds "no forge hosts this" and "the forge is not configured"
/// together on purpose — both mean "do not read". The caller that needs to
/// tell a person why reaches for [`unsupported_reason`], which answers the
/// first case without a token.
///
/// `transport` is the caller's stand-in for the network, the same seam
/// `provider_for_token` and `identity_for` already take. `None` is the real
/// path: the client talks HTTP. A service test passes its fixture so the
/// credential resolved from the token file is exercised without a socket.
pub fn provider_for(
    repo_url: Option<&str>,
    config: &VogtConfig,
    transport: Option<Transport>,
) -> Result<Option<ForgeClient>, VogtError> {
    let Some(matched) = match_repo(repo_url, Some(config)) else {
        return Ok(None);
    };
    build(matched.kind, &matched.repo.host, config, transport)
}

/// Resolve a configured host to its credentialed provider, if available.
///
/// A host named in `forge_token_files` is supported even when its token file
/// is absent; `None` then means not configured rather than unsupported.
/// `transport` is the caller's stand-in, as for [`provider_for`]. `None`
/// talks HTTP.
pub fn provider_for_host(
    host: &str,
    config: &VogtConfig,
    transport: Option<Transport>,
) -> Result<Option<ForgeClient>, VogtError> {
    let Some(kind) = kind_for_host(host, Some(config)) else {
        return Ok(None);
    };
    build(kind, host, config, transport)
}

/// Build a provider from an explicitly resolved actor token.
///
/// This is what `forge.account_link` uses to validate a pasted token: the
/// token is the caller's, not a file's. `None` means the host is not one this
/// build reads.
pub fn provider_for_token(
    host: &str,
    token: &str,
    config: Option<&VogtConfig>,
    transport: Option<Transport>,
) -> Option<ForgeClient> {
    let kind = kind_for_host(host, config)?;
    Some(build_token(kind, host, token, transport))
}

/// Resolve repository identity without requiring a forge token.
///
/// GitHub is always an identity host. Forgejo identities are available for
/// hosts declared in `forge_token_files`. The token-less provider builds
/// canonical URLs and clones public repositories; the credentialed provider
/// remains the gate for consolidation.
pub fn identity_for(
    repo_url: Option<&str>,
    config: &VogtConfig,
    transport: Option<Transport>,
) -> Option<(ForgeClient, RepoRef)> {
    let matched = match_repo(repo_url, Some(config))?;
    Some((
        identity(matched.kind, &matched.repo.host, transport),
        matched.repo,
    ))
}

/// A token-less GitHub provider for pure identity work — parsing a reference,
/// building clone and web URLs — which needs no credential.
pub fn github_identity() -> GitHubProvider<GitHubClient> {
    GitHubProvider::new(GitHubClient::new(None, None))
}

/// The github.com provider specifically, for the GitHub-only paths.
///
/// Import and write-back ask for the concrete provider by name rather than
/// resolving one per URL. It still goes through the same token resolution as
/// everything else.
/// `transport` is the caller's stand-in, as for [`provider_for`]. `None`
/// talks HTTP.
pub fn github_provider(
    config: &VogtConfig,
    transport: Option<Transport>,
) -> Result<Option<GitHubProvider<GitHubClient>>, VogtError> {
    match build(ForgeKind::GitHub, GITHUB_HOST, config, transport)? {
        Some(ForgeClient::GitHub(provider)) => Ok(Some(provider)),
        _ => Ok(None),
    }
}

fn kind_for_host(host: &str, config: Option<&VogtConfig>) -> Option<ForgeKind> {
    if host == GITHUB_HOST {
        Some(ForgeKind::GitHub)
    } else if forgejo_hosts(config).iter().any(|named| named == host) {
        Some(ForgeKind::Forgejo)
    } else {
        None
    }
}

fn build(
    kind: ForgeKind,
    host: &str,
    config: &VogtConfig,
    transport: Option<Transport>,
) -> Result<Option<ForgeClient>, VogtError> {
    match kind {
        ForgeKind::GitHub => Ok(GitHubClient::from_token_file(
            token_file_for(config, host),
            transport,
        )?
        .map(|client| ForgeClient::GitHub(GitHubProvider::new(client)))),
        ForgeKind::Forgejo => {
            Ok(
                ForgejoClient::from_token_file(host, token_file_for(config, host), transport)?
                    .map(|client| ForgeClient::Forgejo(ForgejoProvider::new(client, host))),
            )
        }
    }
}

fn build_token(
    kind: ForgeKind,
    host: &str,
    token: &str,
    transport: Option<Transport>,
) -> ForgeClient {
    let token = Some(token.to_owned());
    match kind {
        ForgeKind::GitHub => {
            ForgeClient::GitHub(GitHubProvider::new(GitHubClient::new(token, transport)))
        }
        ForgeKind::Forgejo => ForgeClient::Forgejo(ForgejoProvider::new(
            ForgejoClient::new(host, token, transport),
            host,
        )),
    }
}

fn identity(kind: ForgeKind, host: &str, transport: Option<Transport>) -> ForgeClient {
    match kind {
        ForgeKind::GitHub => {
            ForgeClient::GitHub(GitHubProvider::new(GitHubClient::new(None, transport)))
        }
        ForgeKind::Forgejo => ForgeClient::Forgejo(ForgejoProvider::new(
            ForgejoClient::new(host, None, transport),
            host,
        )),
    }
}

impl ForgeProvider for ForgeClient {
    fn capabilities(&self) -> super::models::ForgeCapabilities {
        match self {
            Self::GitHub(provider) => provider.capabilities(),
            Self::Forgejo(provider) => provider.capabilities(),
        }
    }

    fn list_repos(&self) -> Result<Vec<super::models::ForgeRepo>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.list_repos(),
            Self::Forgejo(provider) => provider.list_repos(),
        }
    }

    fn parse(&self, repo_url: Option<&str>) -> Option<RepoRef> {
        match self {
            Self::GitHub(provider) => provider.parse(repo_url),
            Self::Forgejo(provider) => provider.parse(repo_url),
        }
    }

    fn subject_key(&self, repo: &RepoRef, number: i64) -> String {
        match self {
            Self::GitHub(provider) => provider.subject_key(repo, number),
            Self::Forgejo(provider) => provider.subject_key(repo, number),
        }
    }

    fn number_of(&self, subject_key: Option<&str>) -> Option<i64> {
        match self {
            Self::GitHub(provider) => provider.number_of(subject_key),
            Self::Forgejo(provider) => provider.number_of(subject_key),
        }
    }

    fn clone_url(&self, repo: &RepoRef) -> String {
        match self {
            Self::GitHub(provider) => provider.clone_url(repo),
            Self::Forgejo(provider) => provider.clone_url(repo),
        }
    }

    fn web_url(&self, repo: &RepoRef) -> String {
        match self {
            Self::GitHub(provider) => provider.web_url(repo),
            Self::Forgejo(provider) => provider.web_url(repo),
        }
    }

    fn describe(
        &self,
        repo: &RepoRef,
    ) -> Result<Option<serde_json::Map<String, serde_json::Value>>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.describe(repo),
            Self::Forgejo(provider) => provider.describe(repo),
        }
    }

    fn clone_token(&self) -> Option<String> {
        match self {
            Self::GitHub(provider) => provider.clone_token(),
            Self::Forgejo(provider) => provider.clone_token(),
        }
    }

    fn identity(&self) -> Result<Option<(String, String)>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.identity(),
            Self::Forgejo(provider) => provider.identity(),
        }
    }

    fn issues_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<super::models::ForgeIssue>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.issues_updated_since(repo, since),
            Self::Forgejo(provider) => provider.issues_updated_since(repo, since),
        }
    }

    fn pulls_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<super::models::ForgePull>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.pulls_updated_since(repo, since),
            Self::Forgejo(provider) => provider.pulls_updated_since(repo, since),
        }
    }

    fn releases(
        &self,
        repo: &RepoRef,
    ) -> Result<Vec<super::models::ForgeRelease>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.releases(repo),
            Self::Forgejo(provider) => provider.releases(repo),
        }
    }

    fn checks(
        &self,
        repo: &RepoRef,
    ) -> Result<Vec<super::models::ForgeCheck>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.checks(repo),
            Self::Forgejo(provider) => provider.checks(repo),
        }
    }

    fn watched_ref_checks(
        &self,
        repo: &RepoRef,
    ) -> Result<Vec<super::models::ForgeCheck>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.watched_ref_checks(repo),
            Self::Forgejo(provider) => provider.watched_ref_checks(repo),
        }
    }

    fn failed_jobs(
        &self,
        repo: &RepoRef,
        run_id: i64,
    ) -> Result<Vec<super::models::ForgeJob>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.failed_jobs(repo, run_id),
            Self::Forgejo(provider) => provider.failed_jobs(repo, run_id),
        }
    }

    fn read_file(
        &self,
        repo: &RepoRef,
        path: &str,
    ) -> Result<Option<Vec<u8>>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.read_file(repo, path),
            Self::Forgejo(provider) => provider.read_file(repo, path),
        }
    }

    fn compare(
        &self,
        repo: &RepoRef,
        base: &str,
        head: &str,
    ) -> Result<Option<super::models::ForgeComparison>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.compare(repo, base, head),
            Self::Forgejo(provider) => provider.compare(repo, base, head),
        }
    }

    fn labels(
        &self,
        repo: &RepoRef,
    ) -> Result<Vec<super::models::ForgeLabel>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.labels(repo),
            Self::Forgejo(provider) => provider.labels(repo),
        }
    }

    fn posture(
        &self,
        repo: &RepoRef,
    ) -> Result<super::models::ForgePosture, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.posture(repo),
            Self::Forgejo(provider) => provider.posture(repo),
        }
    }

    fn notifications(
        &self,
        repo: &RepoRef,
    ) -> Result<Vec<super::models::ForgeNotification>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.notifications(repo),
            Self::Forgejo(provider) => provider.notifications(repo),
        }
    }

    fn resolve_actor(
        &self,
        api_url: &str,
    ) -> Result<Option<super::models::ForgeActor>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.resolve_actor(api_url),
            Self::Forgejo(provider) => provider.resolve_actor(api_url),
        }
    }

    fn org_members(&self, owner: &str) -> Result<Option<Vec<String>>, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.org_members(owner),
            Self::Forgejo(provider) => provider.org_members(owner),
        }
    }

    fn comment(
        &self,
        repo: &RepoRef,
        number: i64,
        body: &str,
    ) -> Result<super::writeback::WriteBackResult, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.comment(repo, number, body),
            Self::Forgejo(provider) => provider.comment(repo, number, body),
        }
    }

    fn create_issue(
        &self,
        repo: &RepoRef,
        title: &str,
        body: &str,
        labels: Option<&[String]>,
    ) -> Result<super::writeback::WriteBackResult, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.create_issue(repo, title, body, labels),
            Self::Forgejo(provider) => provider.create_issue(repo, title, body, labels),
        }
    }

    fn add_labels(
        &self,
        repo: &RepoRef,
        number: i64,
        labels: &[String],
    ) -> Result<super::writeback::WriteBackResult, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.add_labels(repo, number, labels),
            Self::Forgejo(provider) => provider.add_labels(repo, number, labels),
        }
    }

    fn set_state(
        &self,
        repo: &RepoRef,
        number: i64,
        state: &str,
    ) -> Result<super::writeback::WriteBackResult, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.set_state(repo, number, state),
            Self::Forgejo(provider) => provider.set_state(repo, number, state),
        }
    }

    fn update_issue_body(
        &self,
        repo: &RepoRef,
        number: i64,
        body: &str,
    ) -> Result<super::writeback::WriteBackResult, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.update_issue_body(repo, number, body),
            Self::Forgejo(provider) => provider.update_issue_body(repo, number, body),
        }
    }

    fn create_repo(
        &self,
        name: &str,
        private: bool,
        description: Option<&str>,
    ) -> Result<super::models::ForgeRepo, crate::errors::VogtError> {
        match self {
            Self::GitHub(provider) => provider.create_repo(name, private, description),
            Self::Forgejo(provider) => provider.create_repo(name, private, description),
        }
    }
}

/// The conditional-registration gate for the per-project sync collectors: an
/// always-registered collector that fails on the network would pin the whole
/// estate's freshness to `partial`, so "no forge configured" means "not
/// registered", not "registered and failing".
///
/// Goes through [`build`] rather than a direct file read, so a host counts as
/// configured exactly when a provider can be built for it.
pub fn has_configured_forge(config: &VogtConfig) -> Result<bool, VogtError> {
    for host in supported_hosts(Some(config)) {
        if let Some(kind) = kind_for_host(&host, Some(config)) {
            if build(kind, &host, config, None)?.is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// `~/x` to `{home}/x`; anything else unchanged. A missing home leaves the
/// tilde in place, so the later read fails honestly rather than guessing.
fn expand_user(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix("~/") else {
        return path.to_path_buf();
    };
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(rest))
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(files: Vec<(&str, &str)>, github: Option<&str>) -> VogtConfig {
        VogtConfig {
            github_token_file: github.map(PathBuf::from),
            forge_token_files: files
                .into_iter()
                .map(|(host, path)| (host.to_owned(), PathBuf::from(path)))
                .collect(),
            ..VogtConfig::default()
        }
    }

    #[test]
    fn only_github_answers_without_a_config() {
        assert_eq!(supported_hosts(None), vec!["github.com".to_owned()]);
        assert!(forgejo_hosts(None).is_empty());
    }

    #[test]
    fn a_named_host_is_forgejo_and_github_is_not() {
        let config = config_with(
            vec![
                ("forge.example", "/run/secrets/forge"),
                ("github.com", "/run/secrets/github"),
            ],
            None,
        );
        assert_eq!(forgejo_hosts(Some(&config)), vec!["forge.example"]);
        let hosts = supported_hosts(Some(&config));
        assert!(hosts.contains(&"github.com".to_owned()));
        assert!(hosts.contains(&"forge.example".to_owned()));
    }

    #[test]
    fn matching_follows_the_declared_hosts() {
        let config = config_with(vec![("forge.example", "/t")], None);
        let github = match_repo(Some("https://github.com/acme/widget"), Some(&config)).unwrap();
        assert_eq!(github.kind, ForgeKind::GitHub);
        assert_eq!(github.repo.slug(), "acme/widget");

        let forgejo =
            match_repo(Some("https://forge.example/acme/widget.git"), Some(&config)).unwrap();
        assert_eq!(forgejo.kind, ForgeKind::Forgejo);

        // The same URL is nobody's without the declaration.
        assert!(match_repo(Some("https://forge.example/acme/widget"), None).is_none());
    }

    #[test]
    fn the_github_alias_yields_to_the_map() {
        let config = config_with(vec![("github.com", "/mapped")], Some("/alias"));
        assert_eq!(
            token_file_for(&config, "github.com"),
            Some(Path::new("/mapped"))
        );
        let bare = config_with(vec![], Some("/alias"));
        assert_eq!(
            token_file_for(&bare, "github.com"),
            Some(Path::new("/alias"))
        );
        assert!(token_file_for(&bare, "forge.example").is_none());
    }

    #[test]
    fn a_missing_url_and_an_unknown_host_explain_themselves() {
        let config = config_with(vec![("forge.example", "/t")], None);
        let missing = unsupported_reason(None, Some(&config)).unwrap();
        assert!(missing.contains("not collected"), "{missing}");
        assert!(unsupported_reason(Some("https://github.com/acme/widget"), None).is_none());
        let unknown =
            unsupported_reason(Some("https://gitlab.com/acme/widget"), Some(&config)).unwrap();
        assert!(unknown.contains("gitlab.com"), "{unknown}");
        assert!(unknown.contains("forge.example"), "{unknown}");
    }

    #[test]
    fn a_usable_token_file_is_the_only_thing_that_counts_as_configured() {
        let dir = std::env::temp_dir().join(format!("vogt-forge-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, "  secret-token \n").unwrap();

        let mut present = config_with(vec![], None);
        present.github_token_file = Some(file.clone());
        assert_eq!(
            read_token(Some(&file)).unwrap().as_deref(),
            Some("secret-token")
        );
        assert!(has_configured_forge(&present).unwrap());

        let absent = config_with(vec![("forge.example", "/no/such/file")], None);
        assert!(read_token(Some(Path::new("/no/such/file")))
            .unwrap()
            .is_none());
        assert!(!has_configured_forge(&absent).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scripted(expect_auth: &'static str) -> Transport {
        Box::new(move |_url, headers, _body, _method| {
            let auth = headers.get("Authorization").map(String::as_str);
            let (status, login) = if auth == Some(expect_auth) {
                (200, br#"{"login":"ada"}"#.to_vec())
            } else {
                (401, br#"{"message":"bad credentials"}"#.to_vec())
            };
            let mut response_headers = std::collections::BTreeMap::new();
            response_headers.insert("x-oauth-scopes".to_owned(), "repo".to_owned());
            (status, login, response_headers)
        })
    }

    #[test]
    fn provider_for_builds_only_a_host_with_a_usable_token() {
        let dir = std::env::temp_dir().join(format!("vogt-forge-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, "gh-token\n").unwrap();

        let config = config_with(
            vec![("forge.example", "/no/such")],
            Some(file.to_str().unwrap()),
        );
        assert!(
            provider_for(Some("https://github.com/acme/widget"), &config, None)
                .unwrap()
                .is_some()
        );
        // Declared, but its token file is not there: not collected, not an error.
        assert!(
            provider_for(Some("https://forge.example/acme/widget"), &config, None)
                .unwrap()
                .is_none()
        );
        // Nobody reads gitlab, token or not.
        assert!(
            provider_for(Some("https://gitlab.com/acme/widget"), &config, None)
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_credentialed_resolvers_pass_the_callers_transport_through() {
        let dir = std::env::temp_dir().join(format!("vogt-forge-transport-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let github_file = dir.join("github");
        let forgejo_file = dir.join("forgejo");
        std::fs::write(&github_file, "gh-token\n").unwrap();
        std::fs::write(&forgejo_file, "fj-token\n").unwrap();
        let config = config_with(
            vec![("forge.example", forgejo_file.to_str().unwrap())],
            Some(github_file.to_str().unwrap()),
        );

        // The token comes from the file; the transport is the caller's. A
        // resolver that dropped the transport would authenticate as nobody and
        // the scripted exchange would answer 401.
        let by_url = provider_for(
            Some("https://github.com/acme/widget"),
            &config,
            Some(scripted("Bearer gh-token")),
        )
        .unwrap()
        .unwrap();
        assert_eq!(by_url.identity().unwrap().unwrap().0, "ada");

        let by_host = provider_for_host("forge.example", &config, Some(scripted("token fj-token")))
            .unwrap()
            .unwrap();
        assert_eq!(by_host.identity().unwrap().unwrap().0, "ada");

        let github = github_provider(&config, Some(scripted("Bearer gh-token")))
            .unwrap()
            .unwrap();
        assert_eq!(github.identity().unwrap().unwrap().0, "ada");

        // An unknown host is still nothing, transport or not.
        assert!(provider_for_host("gitlab.com", &config, None)
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_for_token_validates_a_pasted_token_and_refuses_an_unknown_host() {
        let github = provider_for_token(
            "github.com",
            "ghp_pasted",
            None,
            Some(scripted("Bearer ghp_pasted")),
        )
        .unwrap();
        assert_eq!(github.identity().unwrap().unwrap().0, "ada");
        assert_eq!(github.clone_token().as_deref(), Some("ghp_pasted"));

        let forgejo = provider_for_token(
            "forge.example",
            "fj_pasted",
            Some(&config_with(vec![("forge.example", "/t")], None)),
            Some(scripted("token fj_pasted")),
        )
        .unwrap();
        assert_eq!(forgejo.identity().unwrap().unwrap().0, "ada");

        // A host this build does not read resolves to nothing.
        assert!(provider_for_token("gitlab.com", "anything", None, None).is_none());
        // An empty paste is not a credential: the client drops it, so identity
        // fails rather than authenticating as nobody.
        let empty = provider_for_token("github.com", "", None, Some(scripted("Bearer "))).unwrap();
        assert!(empty.clone_token().is_none());
        assert!(empty.identity().unwrap().is_none());
    }

    #[test]
    fn identity_for_needs_no_token_and_only_declared_forgejo_hosts() {
        let config = config_with(vec![("forge.example", "/no/such")], None);
        let (provider, repo) =
            identity_for(Some("https://github.com/acme/widget"), &config, None).unwrap();
        assert_eq!(repo.slug(), "acme/widget");
        assert!(provider.clone_token().is_none());
        assert!(provider
            .parse(Some("https://github.com/acme/widget"))
            .is_some());

        let (provider, repo) =
            identity_for(Some("https://forge.example/acme/widget.git"), &config, None).unwrap();
        assert_eq!(repo.host, "forge.example");
        assert!(provider.clone_token().is_none());

        assert!(identity_for(Some("https://gitlab.com/acme/widget"), &config, None).is_none());
    }
}
