//! The graft source provider (WI-3392, docs/design/source-providers.md).
//!
//! Graft is a provider, not a session concept: everything that checks out,
//! inspects, lands or mints credentials for a graft source lives here, behind
//! [`SourceProvider`]. The CLI verbs run the `graft` binary with `--json`; the
//! credential verbs use the existing mint service ([`crate::graft_mint`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;
use vogt_engine_contract::source::{
    LandOutcome, SavedWork, SourceDescriptor, SourceKind, SourceLocation, SourceLogEntry,
    SourceStatus,
};

use crate::graft_mint::{GraftMinter, GraftToken, SessionGrant};
use crate::source_provider::{
    CredentialHandle, CredentialRequest, LandRequest, SourceError, SourceFuture, SourceProvider,
};

/// The graft provider's deployment settings. The `ENGINE_SOURCE_GRAFT_*` names
/// are current; the older `ENGINE_POD_GRAFT_*` / `ENGINE_GRAFT_MINT_TOKEN_FILE`
/// names are read as aliases for one release, translated here only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraftSettings {
    pub endpoint: Option<String>,
    pub token_file: Option<String>,
    pub mint_token_file: Option<String>,
    /// The deployment's default repo and line, for a session whose project
    /// names no source.
    pub repo: String,
    pub line: String,
    /// Whether a session pod gets a minted, repo- and line-scoped token.
    /// Off unless the deployment sets `ENGINE_SOURCE_GRAFT_MINT` (alias
    /// `ENGINE_POD_GRAFT_MINT`), so an upgrade does not change how existing
    /// pod sessions authenticate (WI-4124).
    pub mint_enabled: bool,
}

/// The last setting is a boolean, so it does not fit the name/alias table.
const MINT_SETTING: (&str, &str) = ("ENGINE_SOURCE_GRAFT_MINT", "ENGINE_POD_GRAFT_MINT");

fn is_true(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

const DEFAULT_REPO: &str = "vogt";
const DEFAULT_LINE: &str = "rust-2";

/// Where minted session credentials are kept under the engine's state dir.
/// The name predates the provider; adopted pods' credentials are found here.
const CREDENTIAL_DIR: &str = "pod-graft-tokens";

const SETTINGS: [(&str, &str); 5] = [
    ("ENGINE_SOURCE_GRAFT_ENDPOINT", "ENGINE_POD_GRAFT_ENDPOINT"),
    (
        "ENGINE_SOURCE_GRAFT_TOKEN_FILE",
        "ENGINE_POD_GRAFT_TOKEN_FILE",
    ),
    (
        "ENGINE_SOURCE_GRAFT_MINT_TOKEN_FILE",
        "ENGINE_GRAFT_MINT_TOKEN_FILE",
    ),
    ("ENGINE_SOURCE_GRAFT_REPO", "ENGINE_POD_GRAFT_REPO"),
    ("ENGINE_SOURCE_GRAFT_LINE", "ENGINE_POD_GRAFT_LINE"),
];

impl GraftSettings {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read settings through `get`; a current name wins over its alias, and an
    /// alias in use is warned about once per load.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let nonempty = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let mut values = SETTINGS.iter().map(|(current, alias)| {
            nonempty(current).or_else(|| {
                let v = nonempty(alias);
                if v.is_some() {
                    tracing::warn!(alias, current, "deprecated source setting; rename it");
                }
                v
            })
        });
        let (mint_current, mint_alias) = MINT_SETTING;
        let mint_enabled = nonempty(mint_current)
            .or_else(|| {
                let v = nonempty(mint_alias);
                if v.is_some() {
                    tracing::warn!(
                        alias = mint_alias,
                        current = mint_current,
                        "deprecated source setting; rename it"
                    );
                }
                v
            })
            .is_some_and(|v| is_true(&v));
        Self {
            endpoint: values.next().flatten(),
            token_file: values.next().flatten(),
            mint_token_file: values.next().flatten(),
            repo: values
                .next()
                .flatten()
                .unwrap_or_else(|| DEFAULT_REPO.into()),
            line: values
                .next()
                .flatten()
                .unwrap_or_else(|| DEFAULT_LINE.into()),
            mint_enabled,
        }
    }
}

impl Default for GraftSettings {
    fn default() -> Self {
        Self {
            endpoint: None,
            token_file: None,
            mint_token_file: None,
            repo: DEFAULT_REPO.into(),
            line: DEFAULT_LINE.into(),
            mint_enabled: false,
        }
    }
}

/// Graft behind the provider interface.
pub struct GraftProvider {
    binary: PathBuf,
    /// Where minted credentials are kept (mode 0600, one file per session).
    credential_dir: PathBuf,
    minter: Option<Arc<dyn GraftMinter>>,
    settings: GraftSettings,
}

impl GraftProvider {
    pub fn new(credential_dir: impl Into<PathBuf>, minter: Option<Arc<dyn GraftMinter>>) -> Self {
        Self {
            binary: PathBuf::from("graft"),
            credential_dir: credential_dir.into(),
            minter,
            settings: GraftSettings::default(),
        }
    }

    /// The provider for a deployment: its settings, credentials kept under
    /// `state_dir`, minting through the gRPC mint service when a mint token is
    /// configured.
    pub fn for_deployment(state_dir: &Path, settings: GraftSettings) -> Result<Self, SourceError> {
        let minter = match settings.mint_token_file.as_deref() {
            None => None,
            Some(file) => {
                let endpoint = settings.endpoint.as_deref().ok_or_else(|| {
                    SourceError::InvalidSource(
                        "ENGINE_SOURCE_GRAFT_MINT_TOKEN_FILE needs ENGINE_SOURCE_GRAFT_ENDPOINT"
                            .into(),
                    )
                })?;
                let m: Arc<dyn GraftMinter> =
                    Arc::new(crate::graft_mint::GrpcGraftMinter::new(endpoint, file));
                Some(m)
            }
        };
        Ok(Self::new(state_dir.join(CREDENTIAL_DIR), minter).with_settings(settings))
    }

    /// The provider over `state_dir` with an explicit minter (tests mint
    /// without a graft server). Naming a minter explicitly enables minting:
    /// the deployment flag only gates the environment-driven constructor.
    pub fn with_minter(state_dir: &Path, minter: Arc<dyn GraftMinter>) -> Self {
        let mut provider = Self::new(state_dir.join(CREDENTIAL_DIR), Some(minter));
        provider.settings.mint_enabled = true;
        provider
    }

    pub fn with_settings(mut self, settings: GraftSettings) -> Self {
        self.settings = settings;
        self
    }

    fn credential_path(&self, session: Uuid) -> PathBuf {
        self.credential_dir.join(session.to_string())
    }

    /// Use another `graft` binary (tests use a fake).
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    fn run(
        &self,
        dir: Option<&Path>,
        endpoint: Option<&str>,
        credential: Option<&CredentialHandle>,
        args: &[&str],
    ) -> Result<Value, SourceError> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--json");
        if let Some(e) = endpoint {
            cmd.arg("--endpoint").arg(e);
        }
        if let Some(c) = credential {
            cmd.arg("--token-file").arg(&c.path);
        }
        if let Some(d) = dir {
            cmd.current_dir(d);
        }
        let out = cmd
            .args(args)
            .output()
            .map_err(|_| SourceError::Unavailable("graft could not be run".into()))?;
        let body: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        if out.status.success() {
            return Ok(body);
        }
        // Exit codes from the graft agent guide: 3 conflict, 6 unauthorized.
        Err(match out.status.code() {
            Some(3) => SourceError::Conflict,
            Some(6) => SourceError::Unauthorized,
            _ => SourceError::Unavailable(format!("graft {} failed", args[0])),
        })
    }
}

fn location(source: &SourceDescriptor) -> Result<(&str, &str), SourceError> {
    let SourceLocation::Graft { endpoint, repo } = &source.location else {
        return Err(SourceError::InvalidSource("not a graft location".into()));
    };
    if [endpoint, repo, &source.reference]
        .iter()
        .any(|v| v.starts_with('-') || v.is_empty())
    {
        return Err(SourceError::InvalidSource(
            "endpoint, repo or ref is empty or starts with '-'".into(),
        ));
    }
    Ok((endpoint, repo))
}

/// A changeset id from a commit or land result, wherever graft puts it.
fn changeset_id(v: &Value) -> Option<String> {
    ["id", "changeset", "head", "landed"]
        .iter()
        .find_map(|k| match v.get(k)? {
            Value::String(s) => Some(s.clone()),
            obj => obj.get("id")?.as_str().map(str::to_owned),
        })
}

fn log_entries(v: &Value) -> Vec<SourceLogEntry> {
    v.get("changesets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(SourceLogEntry {
                id: c.get("id")?.as_str()?.into(),
                summary: c.get("subject")?.as_str().unwrap_or_default().into(),
            })
        })
        .collect()
}

impl SourceProvider for GraftProvider {
    fn kind(&self) -> SourceKind {
        SourceKind::Graft
    }

    fn validate(&self, source: &SourceDescriptor) -> Result<(), SourceError> {
        let SourceLocation::Graft { endpoint, repo } = &source.location else {
            return Err(SourceError::InvalidSource("not a graft location".into()));
        };
        if repo.trim().is_empty() || source.reference.trim().is_empty() {
            return Err(SourceError::InvalidSource(
                "project source names no graft repo or line".into(),
            ));
        }
        if [repo, &source.reference]
            .iter()
            .any(|v| v.starts_with('-') || v.chars().any(char::is_whitespace))
        {
            return Err(SourceError::InvalidSource(
                "project source repo or line is not a valid name".into(),
            ));
        }
        // One deployment, one graft server: the engine's minter and the pod's
        // GRAFT_ENDPOINT are the deployment's, so a source on another server
        // would be checked out with a token that server never issued.
        if !endpoint.is_empty()
            && self
                .settings
                .endpoint
                .as_deref()
                .is_some_and(|e| e != endpoint)
        {
            return Err(SourceError::InvalidSource(format!(
                "project source is on graft server {endpoint}, not this deployment's"
            )));
        }
        Ok(())
    }

    fn default_source(&self) -> Option<SourceDescriptor> {
        Some(SourceDescriptor {
            location: SourceLocation::Graft {
                endpoint: self.settings.endpoint.clone().unwrap_or_default(),
                repo: self.settings.repo.clone(),
            },
            reference: self.settings.line.clone(),
            landing: vogt_engine_contract::source::LandingMode::Line,
        })
    }

    /// `GRAFT_ENDPOINT`: the source's server, else the deployment's.
    fn pod_env(&self, source: &SourceDescriptor) -> Vec<(String, String)> {
        let from_source = match &source.location {
            SourceLocation::Graft { endpoint, .. } if !endpoint.is_empty() => {
                Some(endpoint.clone())
            }
            _ => None,
        };
        from_source
            .or_else(|| self.settings.endpoint.clone())
            .map(|e| vec![("GRAFT_ENDPOINT".to_string(), e)])
            .unwrap_or_default()
    }

    fn mints_credentials(&self) -> bool {
        self.settings.mint_enabled && self.minter.is_some()
    }

    fn shared_credential(&self) -> Option<CredentialHandle> {
        self.settings
            .token_file
            .as_ref()
            .map(|p| CredentialHandle { path: p.into() })
    }

    fn held_credential(&self, session: Uuid) -> Option<CredentialHandle> {
        let path = self.credential_path(session);
        path.is_file().then_some(CredentialHandle { path })
    }

    fn checkout(
        &self,
        source: &SourceDescriptor,
        dest: &Path,
        credential: Option<&CredentialHandle>,
    ) -> Result<(), SourceError> {
        let (endpoint, repo) = location(source)?;
        if dest.exists()
            && std::fs::read_dir(dest)
                .map_err(|_| SourceError::InvalidSource("destination unreadable".into()))?
                .next()
                .is_some()
        {
            return Err(SourceError::InvalidSource(
                "destination is not empty".into(),
            ));
        }
        let dest_s = dest
            .to_str()
            .ok_or_else(|| SourceError::InvalidSource("destination is not utf-8".into()))?;
        self.run(
            None,
            Some(endpoint),
            credential,
            &["clone", repo, dest_s, "--line", &source.reference],
        )
        .map(|_| ())
    }

    fn mint_credential<'a>(
        &'a self,
        source: &'a SourceDescriptor,
        request: &'a CredentialRequest,
    ) -> SourceFuture<'a, CredentialHandle> {
        Box::pin(async move {
            let SourceLocation::Graft { repo, .. } = &source.location else {
                return Err(SourceError::InvalidSource("not a graft location".into()));
            };
            let minter = self.minter.as_ref().ok_or(SourceError::Unsupported(
                "graft credential minting without a mint token",
            ))?;
            let id = request.session;
            // The token is bound to the core session id the worker's brief
            // names; the engine UUID is only the local handle its file is
            // kept under.
            let grant = SessionGrant::new(
                request
                    .session_ref
                    .clone()
                    .unwrap_or_else(|| id.to_string()),
                repo,
                &source.reference,
                request.work_item.as_deref(),
                request.budget.as_ref(),
            )
            .map_err(|e| SourceError::InvalidSource(e.to_string()))?;
            // The mint service's error names what failed (unreachable,
            // refused); it never carries a token.
            let token = minter
                .mint(&grant)
                .await
                .map_err(|e| SourceError::Unavailable(e.to_string()))?;
            tracing::info!(
                session = %id,
                scopes = grant.scopes().len(),
                ttl_s = grant.ttl_s,
                "minted the session's graft token"
            );
            let path = self.credential_path(id);
            write_private(&self.credential_dir, &path, token.expose())
                .map_err(|_| SourceError::Unavailable("cannot keep the credential".into()))?;
            Ok(CredentialHandle { path })
        })
    }

    fn revoke_credential<'a>(&'a self, credential: &'a CredentialHandle) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let id = credential
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| Uuid::parse_str(n).ok())
                .ok_or_else(|| SourceError::InvalidSource("not a graft credential".into()))?;
            let token = std::fs::read_to_string(&credential.path)
                .ok()
                .map(|t| GraftToken::new(t.trim()));
            // The file goes whether or not revocation succeeds; the TTL is the
            // backstop, as at session stop today.
            let _ = std::fs::remove_file(&credential.path);
            match (&self.minter, token) {
                (Some(m), Some(t)) => m
                    .revoke(id, &t)
                    .await
                    .map_err(|e| SourceError::Unavailable(e.to_string())),
                _ => Ok(()),
            }
        })
    }

    fn status(&self, checkout: &Path) -> Result<SourceStatus, SourceError> {
        let v = self.run(Some(checkout), None, None, &["status"])?;
        Ok(SourceStatus {
            reference: v.get("line").and_then(Value::as_str).map(str::to_owned),
            clean: v.get("clean").and_then(Value::as_bool).unwrap_or(false),
            changed: v
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| c.get("path")?.as_str().map(str::to_owned))
                .collect(),
        })
    }

    fn diff(&self, checkout: &Path) -> Result<String, SourceError> {
        let out = Command::new(&self.binary)
            .current_dir(checkout)
            .arg("diff")
            .output()
            .map_err(|_| SourceError::Unavailable("graft could not be run".into()))?;
        if !out.status.success() {
            return Err(SourceError::Unavailable("graft diff failed".into()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn log(&self, checkout: &Path, limit: usize) -> Result<Vec<SourceLogEntry>, SourceError> {
        let n = limit.to_string();
        let v = self.run(Some(checkout), None, None, &["log", "--limit", &n])?;
        Ok(log_entries(&v))
    }

    /// A draft on the server; the working copy is untouched.
    fn save_unlanded(&self, checkout: &Path, reason: &str) -> Result<SavedWork, SourceError> {
        let v = self.run(
            Some(checkout),
            None,
            None,
            &["commit", "-m", reason, "--meta", "vogt.saved=unlanded"],
        )?;
        changeset_id(&v)
            .map(|id| SavedWork { id })
            .ok_or_else(|| SourceError::Unavailable("graft commit returned no id".into()))
    }

    fn land(
        &self,
        source: &SourceDescriptor,
        checkout: &Path,
        request: &LandRequest,
    ) -> Result<LandOutcome, SourceError> {
        location(source)?;
        let meta = request
            .work_item
            .as_ref()
            .map(|w| format!("agent.work_item={w}"));
        let mut commit = vec!["commit", "-m", request.message.as_str()];
        if let Some(m) = &meta {
            commit.extend(["--meta", m.as_str()]);
        }
        self.run(Some(checkout), None, None, &commit)?;
        self.run(Some(checkout), None, None, &["sync"])?;
        let v = self.run(Some(checkout), None, None, &["land"])?;
        changeset_id(&v)
            .map(|id| LandOutcome::Landed { id })
            .ok_or(SourceError::NotLanded)
    }

    /// Changesets on the source's line from `selector` (a changeset id or ref).
    fn report_landed(
        &self,
        source: &SourceDescriptor,
        selector: &str,
    ) -> Result<Vec<SourceLogEntry>, SourceError> {
        let (endpoint, repo) = location(source)?;
        if selector.starts_with('-') {
            return Err(SourceError::InvalidSource(
                "selector starts with '-'".into(),
            ));
        }
        let v = self.run(
            None,
            Some(endpoint),
            None,
            &["log", "--repo", repo, "--at", selector],
        )?;
        Ok(log_entries(&v))
    }
}

fn write_private(dir: &Path, path: &Path, value: &str) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    b.mode(0o700);
    b.create(dir)?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    o.mode(0o600);
    o.open(path)?.write_all(value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graft_mint::{MintFuture, RevokeFuture};
    use std::sync::Mutex;
    use vogt_engine_contract::source::LandingMode;

    fn source() -> SourceDescriptor {
        SourceDescriptor {
            location: SourceLocation::Graft {
                endpoint: "http://graft.test:17888".into(),
                repo: "vogt".into(),
            },
            reference: "rust-2".into(),
            landing: LandingMode::Line,
        }
    }

    /// A fake `graft` that logs its argv and prints a canned JSON reply.
    fn fake(dir: &Path, reply: &str, code: i32) -> PathBuf {
        let bin = dir.join("graft");
        let log = dir.join("argv");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\ncat <<'EOF'\n{reply}\nEOF\nexit {code}\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    fn argv(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("argv")).unwrap_or_default()
    }

    #[derive(Default)]
    struct FakeMinter {
        revoked: Mutex<Vec<(Uuid, String)>>,
        scopes: Mutex<Vec<String>>,
    }

    impl GraftMinter for FakeMinter {
        fn mint<'a>(&'a self, grant: &'a SessionGrant) -> MintFuture<'a> {
            *self.scopes.lock().unwrap() = grant.scopes();
            Box::pin(async { Ok(GraftToken::new("minted")) })
        }
        fn revoke<'a>(&'a self, session: Uuid, token: &'a GraftToken) -> RevokeFuture<'a> {
            self.revoked
                .lock()
                .unwrap()
                .push((session, token.expose().to_owned()));
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn registry_picks_graft_by_kind() {
        let mut r = crate::source_provider::SourceRegistry::with_defaults();
        r.register(Arc::new(GraftProvider::new("/nonexistent", None)));
        assert_eq!(r.for_source(&source()).unwrap().kind(), SourceKind::Graft);
    }

    #[test]
    fn checkout_clones_the_line_from_the_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let p = GraftProvider::new(tmp.path(), None).with_binary(fake(tmp.path(), "{}", 0));
        let dest = tmp.path().join("work");
        let cred = CredentialHandle {
            path: tmp.path().join("tok"),
        };
        p.checkout(&source(), &dest, Some(&cred)).unwrap();
        let a = argv(tmp.path());
        assert!(a.contains("--endpoint http://graft.test:17888"), "{a}");
        assert!(
            a.contains(&format!("--token-file {}", cred.path.display())),
            "{a}"
        );
        assert!(
            a.contains(&format!("clone vogt {} --line rust-2", dest.display())),
            "{a}"
        );
    }

    #[test]
    fn checkout_refuses_git_sources_and_option_like_values() {
        let p = GraftProvider::new("/x", None);
        let mut s = source();
        s.location = SourceLocation::Git {
            url: "u".into(),
            repo: "r".into(),
        };
        assert!(matches!(
            p.checkout(&s, Path::new("/x/w"), None),
            Err(SourceError::InvalidSource(_))
        ));
        let mut s = source();
        s.reference = "--upload".into();
        assert!(matches!(
            p.checkout(&s, Path::new("/x/w"), None),
            Err(SourceError::InvalidSource(_))
        ));
    }

    #[test]
    fn status_and_log_read_graft_json() {
        let tmp = tempfile::tempdir().unwrap();
        let status = r#"{"base":"b","changes":[{"path":"README.md","status":"modified"}],"clean":false,"conflicts":[],"head":null,"line":"rust-2","unfinished":false}"#;
        let p = GraftProvider::new(tmp.path(), None).with_binary(fake(tmp.path(), status, 0));
        let s = p.status(tmp.path()).unwrap();
        assert_eq!(s.reference.as_deref(), Some("rust-2"));
        assert!(!s.clean);
        assert_eq!(s.changed, vec!["README.md".to_string()]);

        let log = r#"{"changesets":[{"id":"ca66","subject":"first"}],"next":null}"#;
        let p = p.with_binary(fake(tmp.path(), log, 0));
        let l = p.log(tmp.path(), 1).unwrap();
        assert_eq!(
            l,
            vec![SourceLogEntry {
                id: "ca66".into(),
                summary: "first".into()
            }]
        );
        assert!(argv(tmp.path()).contains("log --limit 1"));
    }

    #[test]
    fn exit_codes_map_to_typed_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let p = GraftProvider::new(tmp.path(), None).with_binary(fake(tmp.path(), "{}", 6));
        assert_eq!(p.status(tmp.path()), Err(SourceError::Unauthorized));
        let p = p.with_binary(fake(tmp.path(), "{}", 3));
        assert_eq!(p.status(tmp.path()), Err(SourceError::Conflict));
    }

    #[test]
    fn land_commits_with_the_work_item_then_syncs_and_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let p = GraftProvider::new(tmp.path(), None).with_binary(fake(
            tmp.path(),
            r#"{"id":"cafe"}"#,
            0,
        ));
        let out = p
            .land(
                &source(),
                tmp.path(),
                &LandRequest {
                    message: "m".into(),
                    work_item: Some("WI-1".into()),
                },
            )
            .unwrap();
        assert_eq!(out, LandOutcome::Landed { id: "cafe".into() });
        let a = argv(tmp.path());
        let lines: Vec<_> = a.lines().collect();
        assert_eq!(lines.len(), 3, "{a}");
        assert!(lines[0].contains("commit -m m --meta agent.work_item=WI-1"));
        assert!(lines[1].ends_with("sync"));
        assert!(lines[2].ends_with("land"));
    }

    #[tokio::test]
    async fn mint_keeps_a_private_file_and_revoke_withdraws_it() {
        let tmp = tempfile::tempdir().unwrap();
        let minter = Arc::new(FakeMinter::default());
        let p = GraftProvider::new(tmp.path().join("creds"), Some(minter.clone()));
        let id = Uuid::new_v4();
        let h = p
            .mint_credential(&source(), &CredentialRequest::for_session(id))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&h.path).unwrap(), "minted");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&h.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(minter
            .scopes
            .lock()
            .unwrap()
            .contains(&"repo:vogt:land:rust-2".to_string()));

        p.revoke_credential(&h).await.unwrap();
        assert!(!h.path.exists());
        assert_eq!(
            *minter.revoked.lock().unwrap(),
            vec![(id, "minted".to_string())]
        );
    }

    #[tokio::test]
    async fn the_grant_uses_the_core_session_id_when_the_start_named_one() {
        let tmp = tempfile::tempdir().unwrap();
        let minter = Arc::new(FakeMinter::default());
        let p = GraftProvider::new(tmp.path(), Some(minter));
        let mut request = CredentialRequest::for_session(Uuid::new_v4());
        request.session_ref = Some("ses_01ARZ3NDEKTSV4RRFFQ69G5FAV".into());
        p.mint_credential(&source(), &request).await.unwrap();
        // The FakeMinter's scopes are all it records; the grant's own
        // session id is asserted through the real minter in graft_mint's
        // tests and through the bootstrap response in the integration tests.
        assert!(p.held_credential(request.session).is_some());
    }

    #[test]
    fn minting_is_off_until_the_deployment_enables_it() {
        let tmp = tempfile::tempdir().unwrap();
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        // A mint token alone does not turn minting on: the batch behaviour is
        // unchanged until the flag is set.
        let off = GraftProvider::for_deployment(
            tmp.path(),
            GraftSettings::from_lookup(env(&[
                ("ENGINE_POD_GRAFT_ENDPOINT", "http://graft.test:17888"),
                ("ENGINE_GRAFT_MINT_TOKEN_FILE", "/run/mint"),
            ])),
        )
        .unwrap();
        assert!(!off.mints_credentials());
        // Any of the usual true spellings enables it, under either name.
        let lookup = |pairs: Vec<(&'static str, &'static str)>| {
            GraftSettings::from_lookup(move |k| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            })
        };
        for (name, value) in [
            ("ENGINE_POD_GRAFT_MINT", "1"),
            ("ENGINE_SOURCE_GRAFT_MINT", "true"),
        ] {
            let settings = lookup(vec![
                ("ENGINE_POD_GRAFT_ENDPOINT", "http://graft.test:17888"),
                ("ENGINE_GRAFT_MINT_TOKEN_FILE", "/run/mint"),
                (name, value),
            ]);
            let p = GraftProvider::for_deployment(tmp.path(), settings).unwrap();
            assert!(p.mints_credentials(), "{name}={value}");
        }
        // A flag with no mint token cannot mint: there is nothing to mint with.
        let no_token = GraftProvider::for_deployment(
            tmp.path(),
            GraftSettings::from_lookup(env(&[("ENGINE_POD_GRAFT_MINT", "1")])),
        )
        .unwrap();
        assert!(!no_token.mints_credentials());
    }

    #[tokio::test]
    async fn mint_works_from_inside_a_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let p = GraftProvider::new(tmp.path(), Some(Arc::new(FakeMinter::default())));
        assert!(p
            .mint_credential(&source(), &CredentialRequest::for_session(Uuid::new_v4()))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn mint_without_a_minter_is_unsupported() {
        let p = GraftProvider::new("/x", None);
        assert!(matches!(
            p.mint_credential(&source(), &CredentialRequest::for_session(Uuid::new_v4()))
                .await,
            Err(SourceError::Unsupported(_))
        ));
    }

    #[test]
    fn settings_prefer_current_names_over_aliases() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let s = GraftSettings::from_lookup(env(&[
            ("ENGINE_POD_GRAFT_ENDPOINT", "http://old"),
            ("ENGINE_GRAFT_MINT_TOKEN_FILE", "/old/mint"),
            ("ENGINE_SOURCE_GRAFT_MINT_TOKEN_FILE", "/new/mint"),
        ]));
        assert_eq!(s.endpoint.as_deref(), Some("http://old"));
        assert_eq!(s.mint_token_file.as_deref(), Some("/new/mint"));
        assert_eq!(s.token_file, None);
    }

    fn deployment(endpoint: Option<&str>) -> GraftProvider {
        GraftProvider::new("/nonexistent", None).with_settings(GraftSettings {
            endpoint: endpoint.map(str::to_owned),
            token_file: Some("/run/shared-token".into()),
            ..Default::default()
        })
    }

    #[test]
    fn the_default_source_is_the_deployments_repo_and_line() {
        let s = GraftSettings::from_lookup(|k| match k {
            "ENGINE_POD_GRAFT_REPO" => Some("farmeggs".into()),
            "ENGINE_SOURCE_GRAFT_LINE" => Some("backlog-1".into()),
            _ => None,
        });
        let d = GraftProvider::new("/x", None)
            .with_settings(s)
            .default_source()
            .unwrap();
        assert_eq!(d.kind(), SourceKind::Graft);
        assert_eq!(d.location.repo(), "farmeggs");
        assert_eq!(d.reference, "backlog-1");
        let d = GraftProvider::new("/x", None).default_source().unwrap();
        assert_eq!(
            (d.location.repo(), d.reference.as_str()),
            ("vogt", "rust-2")
        );
    }

    #[test]
    fn validate_refuses_bad_names_other_servers_and_git_sources() {
        let p = deployment(Some("http://graft-dev:17888"));
        let named = |repo: &str, line: &str, endpoint: &str| SourceDescriptor {
            location: SourceLocation::Graft {
                endpoint: endpoint.into(),
                repo: repo.into(),
            },
            reference: line.into(),
            landing: LandingMode::Line,
        };
        assert!(p.validate(&named("farmeggs", "backlog-1", "")).is_ok());
        assert!(p
            .validate(&named("r", "l", "http://graft-dev:17888"))
            .is_ok());
        for bad in [
            named("", "l", ""),
            named("r", "", ""),
            named("-r", "l", ""),
            named("r", "a b", ""),
            named("r", "l", "http://other:1"),
        ] {
            assert!(p.validate(&bad).is_err(), "{bad:?}");
        }
        let git = SourceDescriptor {
            location: SourceLocation::Git {
                url: "https://example.test/o/r.git".into(),
                repo: "r".into(),
            },
            reference: "main".into(),
            landing: LandingMode::PushBranch,
        };
        assert!(p.validate(&git).is_err());
    }

    #[test]
    fn pod_env_passes_the_endpoint_only_when_known() {
        let mut s = source();
        let env = |p: &GraftProvider, s: &SourceDescriptor| p.pod_env(s);
        assert_eq!(
            env(&deployment(None), &s),
            vec![("GRAFT_ENDPOINT".into(), "http://graft.test:17888".into())]
        );
        s.location = SourceLocation::Graft {
            endpoint: String::new(),
            repo: "vogt".into(),
        };
        assert!(env(&deployment(None), &s).is_empty());
        assert_eq!(
            env(&deployment(Some("http://dep:1")), &s),
            vec![("GRAFT_ENDPOINT".into(), "http://dep:1".into())]
        );
    }

    #[tokio::test]
    async fn shared_and_held_credentials_come_from_settings_and_the_kept_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = deployment(None);
        assert!(!p.mints_credentials());
        assert_eq!(
            p.shared_credential().unwrap().path,
            PathBuf::from("/run/shared-token")
        );
        let p = GraftProvider::with_minter(tmp.path(), Arc::new(FakeMinter::default()));
        assert!(p.mints_credentials());
        let id = Uuid::new_v4();
        assert!(p.held_credential(id).is_none());
        let h = p
            .mint_credential(&source(), &CredentialRequest::for_session(id))
            .await
            .unwrap();
        assert!(h.path.starts_with(tmp.path().join(CREDENTIAL_DIR)));
        assert_eq!(p.held_credential(id), Some(h.clone()));
        p.revoke_credential(&h).await.unwrap();
        assert!(p.held_credential(id).is_none());
    }
}
