//! Push notification orchestrator.
//!
//! Two transports:
//! - **Web Push** (RFC 8030 + VAPID) for browser PushManager subscriptions.
//!   Works on desktop Chrome/Firefox/Edge, Android Chrome, and installed-PWA
//!   iOS Safari 16.4+.
//! - **FCM HTTP v1** for native Capacitor / Android tokens obtained via
//!   `@capacitor/push-notifications`.
//!
//! Subscriptions persist to a JSON file under `state_dir/push.json` so they
//! survive container restarts (the home dir is bind-mounted).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, SubscriptionKeys, VapidSignatureBuilder,
    WebPushClient as _, WebPushError, WebPushMessageBuilder,
};

use crate::{
    error::{ApiError, Result},
    push_fcm::{FcmSender, ServiceAccount},
};

/// What the client sent up to /api/push/subscribe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Subscription {
    /// Standard browser PushManager subscription (W3C Push API).
    WebPush {
        endpoint: String,
        p256dh: String,
        auth: String,
    },
    /// Native FCM device token from `@capacitor/push-notifications`.
    Fcm { token: String },
}

impl Subscription {
    /// Deterministic ID — endpoint or token. Re-subscriptions are idempotent.
    fn id(&self) -> String {
        let mut h = Sha256::new();
        match self {
            Subscription::WebPush { endpoint, .. } => h.update(endpoint.as_bytes()),
            Subscription::Fcm { token } => h.update(token.as_bytes()),
        }
        format!("{:x}", h.finalize())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSubscription {
    pub id: String,
    pub sub: Subscription,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    /// Optional client-supplied tag so the user can recognise their devices
    /// in a future settings UI ("Pixel 7", "Work laptop", etc.).
    pub label: Option<String>,
}

/// VAPID identity used to authenticate the server to push services.
/// Generated on first use and persisted; the public key is exposed to
/// clients so they can include it in their PushManager.subscribe call.
#[derive(Clone, Serialize, Deserialize)]
struct Vapid {
    /// PEM-encoded EC P-256 private key (`-----BEGIN PRIVATE KEY-----`).
    private_pem: String,
    /// Base64url public key, no padding — the form a browser expects.
    public_b64url: String,
    /// `mailto:` URI the push service can contact in an outage.
    subject: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    vapid: Option<Vapid>,
    subs: HashMap<String, StoredSubscription>,
}

pub struct PushManager {
    store_path: PathBuf,
    state: Mutex<Store>,
    web: IsahcWebPushClient,
    fcm: Option<Arc<FcmSender>>,
}

impl PushManager {
    pub fn new(state_dir: &Path, fcm_sa_json: Option<&str>) -> Result<Self> {
        Self::with_subject(state_dir, fcm_sa_json, "mailto:admin@example.invalid")
    }

    pub fn with_subject(
        state_dir: &Path,
        fcm_sa_json: Option<&str>,
        vapid_subject: &str,
    ) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| ApiError::Config(format!("state_dir {}: {e}", state_dir.display())))?;
        let store_path = state_dir.join("push.json");

        let mut state: Store = if store_path.exists() {
            let raw = std::fs::read_to_string(&store_path)
                .map_err(|e| ApiError::Config(format!("read {}: {e}", store_path.display())))?;
            serde_json::from_str(&raw)
                .map_err(|e| ApiError::Config(format!("parse {}: {e}", store_path.display())))?
        } else {
            Store::default()
        };

        // Materialise a VAPID identity on first use.
        if state.vapid.is_none() {
            state.vapid = Some(generate_vapid(vapid_subject)?);
            persist(&store_path, &state)?;
            info!("generated new VAPID keypair under {}", state_dir.display());
        } else if let Some(v) = state.vapid.as_mut() {
            // Allow operators to update the subject without regenerating keys.
            if v.subject != vapid_subject {
                v.subject = vapid_subject.to_string();
                persist(&store_path, &state)?;
            }
        }

        let web = IsahcWebPushClient::new()
            .map_err(|e| ApiError::Config(format!("web-push client: {e}")))?;

        let fcm = match fcm_sa_json {
            Some(j) => match ServiceAccount::parse(j) {
                Ok(sa) => {
                    info!(project = sa.project_id, "FCM enabled");
                    Some(Arc::new(FcmSender::new(sa)))
                }
                Err(e) => {
                    warn!(error = %e, "FCM disabled (service account parse failed)");
                    None
                }
            },
            None => {
                info!("FCM disabled (no service account configured)");
                None
            }
        };

        Ok(Self {
            store_path,
            state: Mutex::new(state),
            web,
            fcm,
        })
    }

    pub fn vapid_public_key(&self) -> String {
        self.state
            .lock()
            .vapid
            .as_ref()
            .map(|v| v.public_b64url.clone())
            .unwrap_or_default()
    }

    pub fn list(&self) -> Vec<StoredSubscription> {
        self.state.lock().subs.values().cloned().collect()
    }

    pub fn add(&self, sub: Subscription, label: Option<String>) -> Result<StoredSubscription> {
        let id = sub.id();
        let stored = StoredSubscription {
            id: id.clone(),
            sub,
            created_at: time::OffsetDateTime::now_utc(),
            label,
        };
        let mut st = self.state.lock();
        st.subs.insert(id, stored.clone());
        persist(&self.store_path, &st)?;
        Ok(stored)
    }

    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut st = self.state.lock();
        let existed = st.subs.remove(id).is_some();
        if existed {
            persist(&self.store_path, &st)?;
        }
        Ok(existed)
    }

    /// Fan-out a notification. Returns (ok, fail) counts. Subscriptions that
    /// fail with 404/410 are removed (the push service told us they're dead).
    pub async fn notify_all(
        &self,
        title: &str,
        body: &str,
        data: serde_json::Value,
    ) -> (usize, usize) {
        let subs = self.list();
        let mut ok = 0usize;
        let mut fail = 0usize;
        let mut prune: Vec<String> = Vec::new();
        for s in subs {
            match self.send_one(&s, title, body, &data).await {
                Ok(()) => ok += 1,
                Err(PushSendErr::Gone) => {
                    prune.push(s.id);
                    fail += 1;
                }
                Err(PushSendErr::Other(e)) => {
                    warn!(id = s.id, error = %e, "push send failed");
                    fail += 1;
                }
            }
        }
        if !prune.is_empty() {
            let mut st = self.state.lock();
            for id in prune {
                st.subs.remove(&id);
            }
            let _ = persist(&self.store_path, &st);
        }
        (ok, fail)
    }

    async fn send_one(
        &self,
        s: &StoredSubscription,
        title: &str,
        body: &str,
        data: &serde_json::Value,
    ) -> std::result::Result<(), PushSendErr> {
        match &s.sub {
            Subscription::WebPush {
                endpoint,
                p256dh,
                auth,
            } => {
                let info = SubscriptionInfo {
                    endpoint: endpoint.clone(),
                    keys: SubscriptionKeys {
                        p256dh: p256dh.clone(),
                        auth: auth.clone(),
                    },
                };
                let payload = serde_json::json!({
                    "title": title,
                    "body": body,
                    "data": data,
                });
                let payload_bytes = payload.to_string().into_bytes();

                let vapid = self
                    .state
                    .lock()
                    .vapid
                    .clone()
                    .ok_or_else(|| PushSendErr::Other("no VAPID keypair".into()))?;

                let mut sb = VapidSignatureBuilder::from_pem(vapid.private_pem.as_bytes(), &info)
                    .map_err(|e| PushSendErr::Other(format!("vapid: {e}")))?;
                sb.add_claim("sub", vapid.subject.clone());
                let sig = sb
                    .build()
                    .map_err(|e| PushSendErr::Other(format!("vapid build: {e}")))?;

                let mut msg = WebPushMessageBuilder::new(&info);
                msg.set_payload(ContentEncoding::Aes128Gcm, &payload_bytes);
                msg.set_vapid_signature(sig);
                let built = msg
                    .build()
                    .map_err(|e| PushSendErr::Other(format!("build: {e}")))?;

                match self.web.send(built).await {
                    Ok(()) => Ok(()),
                    Err(WebPushError::EndpointNotValid(_))
                    | Err(WebPushError::EndpointNotFound(_)) => Err(PushSendErr::Gone),
                    Err(e) => Err(PushSendErr::Other(format!("web-push: {e}"))),
                }
            }
            Subscription::Fcm { token } => {
                let Some(fcm) = self.fcm.as_ref() else {
                    return Err(PushSendErr::Other("FCM not configured".into()));
                };
                match fcm.send(token, title, body, data.clone()).await {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        let msg = e.to_string();
                        // 404 / NOT_FOUND or UNREGISTERED → token is gone.
                        if msg.contains("404") || msg.contains("UNREGISTERED") {
                            Err(PushSendErr::Gone)
                        } else {
                            Err(PushSendErr::Other(msg))
                        }
                    }
                }
            }
        }
    }
}

enum PushSendErr {
    /// Push service says the subscription is permanently dead — prune it.
    Gone,
    Other(String),
}

fn persist(path: &Path, state: &Store) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| ApiError::Internal(format!("serialize push state: {e}")))?;
    std::fs::write(&tmp, bytes)
        .map_err(|e| ApiError::Internal(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| ApiError::Internal(format!("rename {}: {e}", tmp.display())))?;
    Ok(())
}

/// Mint an EC P-256 keypair for VAPID. Returns the private key as PKCS8 PEM
/// (what the web-push crate expects) and the public key as base64url-no-pad
/// (what the browser expects in `applicationServerKey`).
fn generate_vapid(subject: &str) -> Result<Vapid> {
    use base64::Engine as _;

    // The `web-push` crate gives us no key-gen helper; openssl is the obvious
    // choice but it's a heavy native dep. Instead, lean on the `ring` crate
    // which is already pulled in transitively by reqwest+rustls.
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .map_err(|e| ApiError::Internal(format!("vapid keygen: {e}")))?;
    let key_pair = ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        pkcs8.as_ref(),
        &rng,
    )
    .map_err(|e| ApiError::Internal(format!("vapid load: {e}")))?;

    // The uncompressed SEC1 public key (65 bytes: 0x04 || X || Y) is what
    // browsers want in applicationServerKey, base64url-no-pad encoded.
    use ring::signature::KeyPair;
    let pub_uncompressed = key_pair.public_key().as_ref().to_vec();
    let public_b64url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&pub_uncompressed);

    // Wrap the PKCS8 DER in PEM for web-push.
    let pem = pkcs8_pem(pkcs8.as_ref());

    Ok(Vapid {
        private_pem: pem,
        public_b64url,
        subject: subject.to_string(),
    })
}

fn pkcs8_pem(der: &[u8]) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut s = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        s.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        s.push('\n');
    }
    s.push_str("-----END PRIVATE KEY-----\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn vapid_roundtrip() {
        let v = generate_vapid("mailto:test@example.invalid").expect("keygen");
        assert!(v.private_pem.contains("BEGIN PRIVATE KEY"));
        // Decoded public key should be exactly 65 bytes (SEC1 uncompressed).
        let pk = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(v.public_b64url)
            .expect("decode pub");
        assert_eq!(pk.len(), 65, "uncompressed P-256 SEC1 is 65 bytes");
        assert_eq!(pk[0], 0x04);
    }

    #[test]
    fn subscription_id_is_stable() {
        let a = Subscription::Fcm {
            token: "abc".into(),
        };
        let b = Subscription::Fcm {
            token: "abc".into(),
        };
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn corrupt_store_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("push.json"), "{not-json").expect("write corrupt store");

        let err = match PushManager::new(dir.path(), None) {
            Ok(_) => panic!("corrupt push store should fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("parse"), "unexpected error: {err}");
    }
}
