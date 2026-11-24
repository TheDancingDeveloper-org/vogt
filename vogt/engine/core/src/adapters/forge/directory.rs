//! An owned directory of credentialed forge providers, one per configured host.
//!
//! The sweep's collectors hold a [`ForgeDirectory`] for a whole pass and ask it
//! for a provider per project. Resolving credentials on every ask would re-read
//! token files and rebuild clients mid-sweep, so [`ForgeClients`] resolves every
//! configured host once, at [`ForgeClients::open`], and hands out a borrow of
//! the client it already holds.

use std::collections::BTreeMap;

use super::github::HOST as GITHUB_HOST;
use super::provider::ForgeProvider;
use super::registry::{forgejo_hosts, provider_for_host, unsupported_reason, ForgeClient};
use super::sync::ForgeDirectory;
use crate::adapters::github::Transport;
use crate::config::VogtConfig;
use crate::errors::VogtError;

/// One credentialed client per host this configuration can actually use.
///
/// A host with no usable token is absent, not an error: the collector's receipt
/// says "not collected". An unreadable token file is an error and builds
/// nothing.
///
/// `transport` is the stand-in the resolvers take. It is a boxed closure, so it
/// can ride exactly one host; a sweep passes `None` and every host talks HTTP.
pub struct ForgeClients {
    config: VogtConfig,
    clients: BTreeMap<String, ForgeClient>,
}

impl ForgeClients {
    /// Resolve every configured host once.
    ///
    /// `transport` rides the first host that resolves to a client and no other,
    /// because a stand-in cannot be shared. Pass `None` unless the test has
    /// exactly one configured host.
    pub fn open(config: VogtConfig, mut transport: Option<Transport>) -> Result<Self, VogtError> {
        let mut hosts = forgejo_hosts(Some(&config));
        hosts.push(GITHUB_HOST.to_owned());
        let mut clients = BTreeMap::new();
        for host in hosts {
            // A host with no token must not swallow the stand-in: it can ride
            // exactly one host, so it is only spent on one that resolves.
            let client = if transport.is_some() {
                match provider_for_host(&host, &config, None)? {
                    Some(_) => provider_for_host(&host, &config, transport.take())?,
                    None => None,
                }
            } else {
                provider_for_host(&host, &config, None)?
            };
            if let Some(client) = client {
                clients.insert(host, client);
            }
        }
        Ok(Self { config, clients })
    }

    /// How many hosts resolved to a client. A sweep with none registered
    /// collects nothing, which is the honest answer rather than a failure.
    pub fn len(&self) -> usize {
        self.clients.len()
    }
}

impl ForgeDirectory for ForgeClients {
    fn provider_for(&self, repo_url: Option<&str>) -> Option<&dyn ForgeProvider> {
        let host = super::registry::match_repo(repo_url, Some(&self.config))?
            .repo
            .host;
        self.clients
            .get(&host)
            .map(|client| client as &dyn ForgeProvider)
    }

    fn unsupported_reason(&self, repo_url: Option<&str>) -> String {
        unsupported_reason(repo_url, Some(&self.config)).unwrap_or_else(|| {
            "the forge for this repository has no usable token, so nothing was \
             collected here — which is 'not collected', not 'there is nothing'"
                .to_owned()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::sync::ForgeDirectory;
    use super::*;
    use crate::config::VogtConfig;

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
    fn a_directory_keeps_one_client_per_host() {
        let dir = std::env::temp_dir().join(format!("vogt-forge-directory-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let github_file = dir.join("github");
        let forgejo_file = dir.join("forgejo");
        std::fs::write(&github_file, "gh-token\n").unwrap();
        std::fs::write(&forgejo_file, "fj-token\n").unwrap();
        let config = config_with(
            vec![("forge.example", forgejo_file.to_str().unwrap())],
            Some(github_file.to_str().unwrap()),
        );

        let clients = ForgeClients::open(config, None).unwrap();
        assert_eq!(clients.len(), 2);

        // Two repositories on one host borrow the same client.
        let first = ForgeDirectory::provider_for(&clients, Some("https://github.com/acme/widget"))
            .unwrap() as *const dyn ForgeProvider;
        let again = ForgeDirectory::provider_for(&clients, Some("https://github.com/acme/other"))
            .unwrap() as *const dyn ForgeProvider;
        assert_eq!(first, again, "a second repository reuses the host's client");

        // The other host is a different client.
        let forgejo =
            ForgeDirectory::provider_for(&clients, Some("https://forge.example/acme/widget"))
                .unwrap() as *const dyn ForgeProvider;
        assert_ne!(first, forgejo);

        // An unknown host is nothing, and says why.
        assert!(
            ForgeDirectory::provider_for(&clients, Some("https://gitlab.com/acme/widget"))
                .is_none()
        );
        let reason =
            ForgeDirectory::unsupported_reason(&clients, Some("https://gitlab.com/acme/widget"));
        assert!(reason.contains("gitlab.com"), "{reason}");

        // A declared host whose token file is missing is not collected.
        let bare = ForgeClients::open(config_with(vec![("forge.example", "/no/such")], None), None)
            .unwrap();
        assert_eq!(bare.len(), 0);
        assert!(
            ForgeDirectory::provider_for(&bare, Some("https://forge.example/acme/widget"))
                .is_none()
        );
        let missing =
            ForgeDirectory::unsupported_reason(&bare, Some("https://forge.example/acme/widget"));
        assert!(missing.contains("not collected"), "{missing}");

        // The stand-in rides the one host that resolves, so its client answers
        // through the fixture rather than the network.
        let scripted = ForgeClients::open(
            config_with(
                vec![("forge.example", forgejo_file.to_str().unwrap())],
                None,
            ),
            Some(Box::new(|_url, headers, _body, _method| {
                let authed =
                    headers.get("Authorization").map(String::as_str) == Some("token fj-token");
                let body: Vec<u8> = if authed {
                    br#"{"login":"ada"}"#.to_vec()
                } else {
                    b"{}".to_vec()
                };
                (
                    if authed { 200 } else { 401 },
                    body,
                    std::collections::BTreeMap::new(),
                )
            })),
        )
        .unwrap();
        assert_eq!(scripted.len(), 1);
        let provider =
            ForgeDirectory::provider_for(&scripted, Some("https://forge.example/acme/widget"))
                .unwrap();
        assert_eq!(provider.identity().unwrap().unwrap().0, "ada");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
