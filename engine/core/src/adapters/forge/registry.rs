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
//! What this does not do is build the provider. The HTTP client that turns a
//! token file into a [`ForgeTransport`](super::transport::ForgeTransport) is
//! WI-1064's, and the providers are generic over that trait, so this layer
//! resolves the host, the repository and the token file and stops. The
//! [`ForgeDirectory`](super::sync::ForgeDirectory) the collectors ask is the
//! thin wrapper a caller builds once it holds the transports.

use std::path::{Path, PathBuf};

use super::forgejo::parse_repo_url;
use super::github::{repo_of, HOST as GITHUB_HOST};
use super::models::RepoRef;
use crate::config::VogtConfig;

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

/// The token a file holds, or `None` when there is nothing usable.
///
/// `None` is the ordinary case, not an error: no file, a missing file or an
/// empty one all mean the host's subjects are simply not collected. The path
/// is expanded the way the Python client expands it, so `~` names the home
/// directory rather than a literal directory.
pub fn read_token(path: Option<&Path>) -> Option<String> {
    let resolved = expand_user(path?);
    let token = std::fs::read_to_string(resolved).ok()?;
    let token = token.trim().to_owned();
    (!token.is_empty()).then_some(token)
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

/// Whether any registered forge has a usable token file.
///
/// The conditional-registration gate for the per-project sync collectors: an
/// always-registered collector that fails on the network would pin the whole
/// estate's freshness to `partial`, so "no forge configured" means "not
/// registered", not "registered and failing".
pub fn has_configured_forge(config: &VogtConfig) -> bool {
    supported_hosts(Some(config))
        .iter()
        .any(|host| read_token(token_file_for(config, host)).is_some())
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
        assert_eq!(read_token(Some(&file)).as_deref(), Some("secret-token"));
        assert!(has_configured_forge(&present));

        let absent = config_with(vec![("forge.example", "/no/such/file")], None);
        assert!(read_token(Some(Path::new("/no/such/file"))).is_none());
        assert!(!has_configured_forge(&absent));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
