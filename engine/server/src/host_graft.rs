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

    /// Withdraw the local copy first, then ask graft to revoke the token.
    pub async fn revoke(self) -> Result<(), SourceError> {
        let _ = std::fs::remove_file(&self.delivered);
        self.provider.revoke_credential(&self.minted).await
    }
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
    }

    impl FakeProvider {
        fn new(root: &Path) -> Self {
            Self {
                root: root.to_owned(),
                requests: Mutex::new(Vec::new()),
                revoked: Mutex::new(0),
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
                std::fs::write(&path, "minted-secret").unwrap();
                Ok(CredentialHandle { path })
            })
        }
        fn revoke_credential<'a>(
            &'a self,
            credential: &'a CredentialHandle,
        ) -> crate::source_provider::SourceFuture<'a, ()> {
            Box::pin(async move {
                *self.revoked.lock().unwrap() += 1;
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

    #[test]
    fn unsafe_bound_session_cannot_escape_the_config_directory() {
        for value in ["", ".", "../escape", "a/b", "two words"] {
            assert!(!safe_session_name(value), "{value:?}");
        }
        assert!(safe_session_name("ses_01M79PCC3YXSN2797X0YMAETSB"));
    }
}
