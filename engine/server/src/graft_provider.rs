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
use crate::source_provider::{CredentialHandle, LandRequest, SourceError, SourceProvider};

/// The graft provider's deployment settings. The `ENGINE_SOURCE_GRAFT_*` names
/// are current; the older `ENGINE_POD_GRAFT_*` / `ENGINE_GRAFT_MINT_TOKEN_FILE`
/// names are read as aliases for one release, translated here only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraftSettings {
    pub endpoint: Option<String>,
    pub token_file: Option<String>,
    pub mint_token_file: Option<String>,
}

const SETTINGS: [(&str, &str); 3] = [
    ("ENGINE_SOURCE_GRAFT_ENDPOINT", "ENGINE_POD_GRAFT_ENDPOINT"),
    (
        "ENGINE_SOURCE_GRAFT_TOKEN_FILE",
        "ENGINE_POD_GRAFT_TOKEN_FILE",
    ),
    (
        "ENGINE_SOURCE_GRAFT_MINT_TOKEN_FILE",
        "ENGINE_GRAFT_MINT_TOKEN_FILE",
    ),
];

impl GraftSettings {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read settings through `get`; a current name wins over its alias, and an
    /// alias in use is warned about once per load.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut values = SETTINGS.iter().map(|(current, alias)| {
            let nonempty = |k: &str| get(k).filter(|v| !v.trim().is_empty());
            nonempty(current).or_else(|| {
                let v = nonempty(alias);
                if v.is_some() {
                    tracing::warn!(alias, current, "deprecated source setting; rename it");
                }
                v
            })
        });
        Self {
            endpoint: values.next().flatten(),
            token_file: values.next().flatten(),
            mint_token_file: values.next().flatten(),
        }
    }
}

/// Graft behind the provider interface.
pub struct GraftProvider {
    binary: PathBuf,
    /// Where minted credentials are kept (mode 0600, one file per session).
    credential_dir: PathBuf,
    minter: Option<Arc<dyn GraftMinter>>,
}

impl GraftProvider {
    pub fn new(credential_dir: impl Into<PathBuf>, minter: Option<Arc<dyn GraftMinter>>) -> Self {
        Self {
            binary: PathBuf::from("graft"),
            credential_dir: credential_dir.into(),
            minter,
        }
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

/// Run an async mint call to completion from sync code, inside or outside a
/// tokio runtime.
fn block_on<T: Send>(fut: impl std::future::Future<Output = T> + Send) -> Result<T, SourceError> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|rt| rt.block_on(fut))
        })
        .join()
    })
    .map_err(|_| SourceError::Unavailable("mint task panicked".into()))?
    .map_err(|_| SourceError::Unavailable("no runtime for the mint call".into()))
}

impl SourceProvider for GraftProvider {
    fn kind(&self) -> SourceKind {
        SourceKind::Graft
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

    fn mint_credential(
        &self,
        source: &SourceDescriptor,
        session: &str,
    ) -> Result<CredentialHandle, SourceError> {
        let (_, repo) = location(source)?;
        let minter = self.minter.as_ref().ok_or(SourceError::Unsupported(
            "graft credential minting without a mint token",
        ))?;
        let id = Uuid::parse_str(session)
            .map_err(|_| SourceError::InvalidSource("session is not a uuid".into()))?;
        let grant = SessionGrant::new(id, repo, &source.reference, None, None)
            .map_err(|e| SourceError::InvalidSource(e.to_string()))?;
        let token = block_on(minter.mint(&grant))?.map_err(|_| SourceError::Unauthorized)?;
        let path = self.credential_dir.join(id.to_string());
        write_private(&self.credential_dir, &path, token.expose())
            .map_err(|_| SourceError::Unavailable("cannot keep the credential".into()))?;
        Ok(CredentialHandle { path })
    }

    fn revoke_credential(&self, credential: &CredentialHandle) -> Result<(), SourceError> {
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
            (Some(m), Some(t)) => block_on(m.revoke(id, &t))?
                .map_err(|_| SourceError::Unavailable("revoke failed".into())),
            _ => Ok(()),
        }
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

    #[test]
    fn mint_keeps_a_private_file_and_revoke_withdraws_it() {
        let tmp = tempfile::tempdir().unwrap();
        let minter = Arc::new(FakeMinter::default());
        let p = GraftProvider::new(tmp.path().join("creds"), Some(minter.clone()));
        let id = Uuid::new_v4();
        let h = p.mint_credential(&source(), &id.to_string()).unwrap();
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

        p.revoke_credential(&h).unwrap();
        assert!(!h.path.exists());
        assert_eq!(
            *minter.revoked.lock().unwrap(),
            vec![(id, "minted".to_string())]
        );
    }

    #[tokio::test]
    async fn mint_works_from_inside_a_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let p = GraftProvider::new(tmp.path(), Some(Arc::new(FakeMinter::default())));
        assert!(p
            .mint_credential(&source(), &Uuid::new_v4().to_string())
            .is_ok());
    }

    #[test]
    fn mint_without_a_minter_is_unsupported() {
        let p = GraftProvider::new("/x", None);
        assert!(matches!(
            p.mint_credential(&source(), &Uuid::new_v4().to_string()),
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
}
