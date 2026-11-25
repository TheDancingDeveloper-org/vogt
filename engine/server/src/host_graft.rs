//! Delivery and lifetime of graft credentials for host-backed sessions.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::source_provider::{
    read_credential, CredentialHandle, CredentialRequest, SourceError, SourceProvider,
};
use vogt_engine_contract::source::SourceDescriptor;

/// A host credential the registry owns until the session ends.
pub struct HostGraftCredential {
    provider: Arc<dyn SourceProvider>,
    minted: CredentialHandle,
    delivered: PathBuf,
}

impl HostGraftCredential {
    /// Mint and place one credential at graft's documented per-session path.
    pub async fn provision(
        provider: Arc<dyn SourceProvider>,
        source: &SourceDescriptor,
        request: &CredentialRequest,
        config_home: &Path,
    ) -> Result<Self, SourceError> {
        let session = request
            .session_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| request.session.to_string());
        if !safe_session_name(&session) {
            return Err(SourceError::InvalidSource(
                "the graft session id is not a safe file name".into(),
            ));
        }

        let minted = provider.mint_credential(source, request).await?;
        let delivered = config_home
            .join("graft/sessions")
            .join(&session)
            .join("trial-1.token");
        let token = match read_credential(&minted) {
            Ok(Some(value)) => value,
            Ok(None) => {
                let _ = provider.revoke_credential(&minted).await;
                return Err(SourceError::Unavailable(
                    "the graft mint service returned an empty credential".into(),
                ));
            }
            Err(_) => {
                let _ = provider.revoke_credential(&minted).await;
                return Err(SourceError::Unavailable(
                    "the minted graft credential could not be read".into(),
                ));
            }
        };
        if write_private(&delivered, &token).is_err() {
            let _ = provider.revoke_credential(&minted).await;
            return Err(SourceError::Unavailable(
                "the host graft credential could not be delivered".into(),
            ));
        }

        Ok(Self {
            provider,
            minted,
            delivered,
        })
    }

    pub fn path(&self) -> &Path {
        &self.delivered
    }

    /// Withdraw the local copy now, without touching graft.
    pub fn remove_delivered(&self) {
        let _ = std::fs::remove_file(&self.delivered);
    }

    /// Withdraw the local copy first, then ask graft to revoke the token.
    pub async fn revoke(self) -> Result<(), SourceError> {
        self.into_remote_revoke().revoke().await
    }

    /// Take the delivered copy away now and keep only what is needed to
    /// revoke the token at graft later (WI-4216).
    ///
    /// Call this synchronously where the run ends, then hand the result to a
    /// task. Two things must not happen late: removing the delivered file, and
    /// reading the provider's copy. A wake mints under the same paths (they
    /// are keyed by session), so a task that did either would delete or revoke
    /// the new run's token. The provider's copy is moved aside here, so the
    /// task revokes the old run's token and nothing else.
    pub fn into_remote_revoke(self) -> RemoteRevoke {
        self.remove_delivered();
        // If the copy cannot be set aside, the only path left is the shared
        // one a wake rewrites. Revoking through it could revoke the new run's
        // token, so skip the remote revoke; the token's TTL is the backstop.
        let minted = set_aside(&self.minted);
        if minted.is_none() {
            tracing::warn!(
                "host graft credential could not be set aside; skipping its remote revoke"
            );
        }
        RemoteRevoke {
            provider: self.provider,
            minted,
        }
    }
}

/// What is left of a host credential once its local copy is gone: the remote
/// revoke only. It never touches the delivered path.
pub struct RemoteRevoke {
    provider: Arc<dyn SourceProvider>,
    /// `None` when the copy could not be set aside: nothing is revoked.
    minted: Option<CredentialHandle>,
}

impl RemoteRevoke {
    pub async fn revoke(self) -> Result<(), SourceError> {
        let Some(minted) = self.minted else {
            return Ok(());
        };
        let result = self.provider.revoke_credential(&minted).await;
        // The provider has had its chance to revoke at graft. A copy it kept
        // (no minter) would sit in this directory where the token sweep does
        // not look, so delete it here; the token is dead after its TTL. Then
        // remove the set-aside directory. Never rename it back to the shared
        // name: that is the race this type exists to avoid.
        if let Some(dir) = minted.path.parent() {
            if dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(SET_ASIDE_PREFIX))
            {
                let _ = std::fs::remove_file(&minted.path);
                let _ = std::fs::remove_dir(dir);
            }
        }
        result
    }
}

const SET_ASIDE_PREFIX: &str = ".revoking-";

/// Move the provider's copy under a directory only this revoke uses, keeping
/// its file name (a provider reads the session id from it).
fn set_aside(minted: &CredentialHandle) -> Option<CredentialHandle> {
    use std::os::unix::fs::DirBuilderExt;

    let name = minted.path.file_name()?;
    let dir = minted
        .path
        .parent()?
        .join(format!("{SET_ASIDE_PREFIX}{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
    let path = dir.join(name);
    if std::fs::rename(&minted.path, &path).is_err() {
        let _ = std::fs::remove_dir(&dir);
        return None;
    }
    Some(CredentialHandle { path })
}

/// The config directory inherited by host children.
pub fn config_home() -> Result<PathBuf, SourceError> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    std::env::var_os("HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".config"))
        .ok_or_else(|| SourceError::Unavailable("the host has no config home".into()))
}

fn safe_session_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn write_private(path: &Path, value: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let dir = path.parent().expect("token path has a parent");
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let tmp = dir.join(format!(".trial-1.token.{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use uuid::Uuid;
    use vogt_engine_contract::source::{
        LandOutcome, LandingMode, SavedWork, SourceKind, SourceLocation, SourceLogEntry,
        SourceStatus,
    };

    struct FakeProvider {
        root: PathBuf,
        requests: Mutex<Vec<CredentialRequest>>,
        revoked: Mutex<usize>,
        /// The token each revoke found in the copy it was handed.
        revoked_tokens: Mutex<Vec<String>>,
        mints: Mutex<usize>,
    }

    impl FakeProvider {
        fn new(root: &Path) -> Self {
            Self {
                root: root.to_owned(),
                requests: Mutex::new(Vec::new()),
                revoked: Mutex::new(0),
                revoked_tokens: Mutex::new(Vec::new()),
                mints: Mutex::new(0),
            }
        }
    }

    impl SourceProvider for FakeProvider {
        fn kind(&self) -> SourceKind {
            SourceKind::Graft
        }
        fn checkout(
            &self,
            _: &SourceDescriptor,
            _: &Path,
            _: Option<&CredentialHandle>,
        ) -> Result<(), SourceError> {
            unreachable!()
        }
        fn mint_credential<'a>(
            &'a self,
            _: &'a SourceDescriptor,
            request: &'a CredentialRequest,
        ) -> crate::source_provider::SourceFuture<'a, CredentialHandle> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request.clone());
                let path = self.root.join(request.session.to_string());
                // The first mint is "minted-secret"; a wake's is a new value.
                let mut mints = self.mints.lock().unwrap();
                *mints += 1;
                let token = match *mints {
                    1 => "minted-secret".to_owned(),
                    n => format!("minted-secret-{n}"),
                };
                std::fs::write(&path, token).unwrap();
                Ok(CredentialHandle { path })
            })
        }
        fn revoke_credential<'a>(
            &'a self,
            credential: &'a CredentialHandle,
        ) -> crate::source_provider::SourceFuture<'a, ()> {
            Box::pin(async move {
                *self.revoked.lock().unwrap() += 1;
                if let Ok(token) = std::fs::read_to_string(&credential.path) {
                    self.revoked_tokens.lock().unwrap().push(token);
                }
                let _ = std::fs::remove_file(&credential.path);
                Ok(())
            })
        }
        fn status(&self, _: &Path) -> Result<SourceStatus, SourceError> {
            unreachable!()
        }
        fn diff(&self, _: &Path) -> Result<String, SourceError> {
            unreachable!()
        }
        fn log(&self, _: &Path, _: usize) -> Result<Vec<SourceLogEntry>, SourceError> {
            unreachable!()
        }
        fn save_unlanded(&self, _: &Path, _: &str) -> Result<SavedWork, SourceError> {
            unreachable!()
        }
        fn land(
            &self,
            _: &SourceDescriptor,
            _: &Path,
            _: &crate::source_provider::LandRequest,
        ) -> Result<LandOutcome, SourceError> {
            unreachable!()
        }
        fn report_landed(
            &self,
            _: &SourceDescriptor,
            _: &str,
        ) -> Result<Vec<SourceLogEntry>, SourceError> {
            unreachable!()
        }
    }

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

    #[tokio::test]
    async fn delivery_uses_the_bound_session_path_at_mode_0600_and_revoke_removes_it() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeProvider::new(tmp.path()));
        let request = CredentialRequest {
            session: Uuid::new_v4(),
            session_ref: Some("ses_TEST".into()),
            work_item: Some("WI-3986".into()),
            budget: None,
            oversight: false,
        };
        let credential = HostGraftCredential::provision(
            provider.clone(),
            &source(),
            &request,
            &tmp.path().join("config"),
        )
        .await
        .unwrap();
        assert_eq!(
            credential.path(),
            tmp.path()
                .join("config/graft/sessions/ses_TEST/trial-1.token")
        );
        assert_eq!(
            std::fs::read_to_string(credential.path()).unwrap(),
            "minted-secret"
        );
        assert_eq!(
            std::fs::metadata(credential.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        {
            let seen = provider.requests.lock().unwrap();
            assert_eq!(seen[0].session_ref.as_deref(), Some("ses_TEST"));
            assert_eq!(seen[0].work_item.as_deref(), Some("WI-3986"));
        }
        let delivered = credential.path().to_owned();
        credential.revoke().await.unwrap();
        assert!(!delivered.exists());
        assert_eq!(*provider.revoked.lock().unwrap(), 1);
    }

    /// A wake that follows a stop quickly mints under the same paths before
    /// the stop's revoke task runs. That late revoke must neither delete the
    /// new delivered file nor revoke the new token (WI-4216). Before the fix
    /// the task called `revoke()`, which removed the delivered path again and
    /// whose provider read the shared copy, so this fails on both counts.
    #[tokio::test]
    async fn a_late_revoke_of_the_old_run_leaves_the_new_runs_token_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeProvider::new(tmp.path()));
        let request = CredentialRequest {
            session: Uuid::new_v4(),
            session_ref: Some("ses_WAKE".into()),
            work_item: None,
            budget: None,
            oversight: false,
        };
        let home = tmp.path().join("config");
        let first = HostGraftCredential::provision(provider.clone(), &source(), &request, &home)
            .await
            .unwrap();
        let delivered = first.path().to_owned();

        // The stop: the exit hook takes the files away and spawns the task.
        let late = first.into_remote_revoke();
        assert!(!delivered.exists());

        // The wake mints a new token before the task has run.
        let second = HostGraftCredential::provision(provider.clone(), &source(), &request, &home)
            .await
            .unwrap();
        assert_eq!(second.path(), delivered);

        // The late task now runs.
        late.revoke().await.unwrap();

        assert_eq!(
            std::fs::read_to_string(&delivered).unwrap(),
            "minted-secret-2",
            "the new run's delivered token was removed"
        );
        assert_eq!(
            *provider.revoked_tokens.lock().unwrap(),
            vec!["minted-secret".to_owned()],
            "only the old run's token may be revoked"
        );
        assert!(
            provider.root.join(request.session.to_string()).is_file(),
            "the new run's provider copy was consumed"
        );
        let leftovers = std::fs::read_dir(&provider.root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(SET_ASIDE_PREFIX)
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    /// A provider with no minter keeps the file it is asked to revoke. The
    /// set-aside copy must not be stranded where the token sweep never looks.
    struct KeepingProvider(FakeProvider);

    impl SourceProvider for KeepingProvider {
        fn kind(&self) -> SourceKind {
            SourceKind::Graft
        }
        fn checkout(
            &self,
            s: &SourceDescriptor,
            p: &Path,
            c: Option<&CredentialHandle>,
        ) -> Result<(), SourceError> {
            self.0.checkout(s, p, c)
        }
        fn mint_credential<'a>(
            &'a self,
            s: &'a SourceDescriptor,
            r: &'a CredentialRequest,
        ) -> crate::source_provider::SourceFuture<'a, CredentialHandle> {
            self.0.mint_credential(s, r)
        }
        fn revoke_credential<'a>(
            &'a self,
            _: &'a CredentialHandle,
        ) -> crate::source_provider::SourceFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn status(&self, p: &Path) -> Result<SourceStatus, SourceError> {
            self.0.status(p)
        }
        fn diff(&self, p: &Path) -> Result<String, SourceError> {
            self.0.diff(p)
        }
        fn log(&self, p: &Path, n: usize) -> Result<Vec<SourceLogEntry>, SourceError> {
            self.0.log(p, n)
        }
        fn save_unlanded(&self, p: &Path, m: &str) -> Result<SavedWork, SourceError> {
            self.0.save_unlanded(p, m)
        }
        fn land(
            &self,
            s: &SourceDescriptor,
            p: &Path,
            r: &crate::source_provider::LandRequest,
        ) -> Result<LandOutcome, SourceError> {
            self.0.land(s, p, r)
        }
        fn report_landed(
            &self,
            s: &SourceDescriptor,
            r: &str,
        ) -> Result<Vec<SourceLogEntry>, SourceError> {
            self.0.report_landed(s, r)
        }
    }

    fn request(session_ref: &str) -> CredentialRequest {
        CredentialRequest {
            session: Uuid::new_v4(),
            session_ref: Some(session_ref.into()),
            work_item: None,
            budget: None,
            oversight: false,
        }
    }

    #[tokio::test]
    async fn a_revoke_whose_provider_keeps_the_file_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = Arc::new(KeepingProvider(FakeProvider::new(tmp.path())));
        let credential = HostGraftCredential::provision(
            provider.clone(),
            &source(),
            &request("ses_KEEP"),
            &tmp.path().join("config"),
        )
        .await
        .unwrap();
        credential.revoke().await.unwrap();
        let left: Vec<_> = std::fs::read_dir(&provider.0.root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().starts_with(SET_ASIDE_PREFIX))
            .collect();
        assert!(left.is_empty(), "stranded: {left:?}");
    }

    #[tokio::test]
    async fn when_set_aside_fails_the_remote_revoke_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeProvider::new(tmp.path()));
        let req = request("ses_FAIL");
        let home = tmp.path().join("config");
        let first = HostGraftCredential::provision(provider.clone(), &source(), &req, &home)
            .await
            .unwrap();
        // The provider's copy vanishes, so the rename fails.
        std::fs::remove_file(&first.minted.path).unwrap();
        let late = first.into_remote_revoke();
        // A wake writes the shared path.
        let second = HostGraftCredential::provision(provider.clone(), &source(), &req, &home)
            .await
            .unwrap();
        late.revoke().await.unwrap();
        assert_eq!(*provider.revoked.lock().unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(provider.root.join(req.session.to_string())).unwrap(),
            "minted-secret-2"
        );
        drop(second);
    }

    #[test]
    fn unsafe_bound_session_cannot_escape_the_config_directory() {
        for value in ["", ".", "../escape", "a/b", "two words"] {
            assert!(!safe_session_name(value), "{value:?}");
        }
        assert!(safe_session_name("ses_01M79PCC3YXSN2797X0YMAETSB"));
    }
}
