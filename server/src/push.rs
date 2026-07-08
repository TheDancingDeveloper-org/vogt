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
use time::OffsetDateTime;
use tracing::{info, warn};
use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, SubscriptionKeys, VapidSignatureBuilder,
    WebPushClient as _, WebPushError, WebPushMessageBuilder,
};

use crate::{
    error::{ApiError, Result},
    push_fcm::{FcmSender, ServiceAccount},
};

fn default_true() -> bool {
    true
}

fn default_quiet_digest_keep() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationKind {
    WaitingForInput,
    AgentTaskStarted,
    AgentTaskNotify,
    Test,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuietHours {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub start_minute: u16,
    #[serde(default)]
    pub end_minute: u16,
    #[serde(default)]
    pub utc_offset_minutes: i16,
    #[serde(default = "default_quiet_digest_keep")]
    pub digest: bool,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start_minute: 22 * 60,
            end_minute: 7 * 60,
            utc_offset_minutes: 0,
            digest: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPreferences {
    #[serde(default = "default_true")]
    pub waiting_for_input: bool,
    #[serde(default = "default_true")]
    pub agent_task_started: bool,
    #[serde(default = "default_true")]
    pub agent_task_notify: bool,
    #[serde(default)]
    pub quiet_hours: QuietHours,
}

impl Default for PushPreferences {
    fn default() -> Self {
        Self {
            waiting_for_input: true,
            agent_task_started: true,
            agent_task_notify: true,
            quiet_hours: QuietHours::default(),
        }
    }
}

impl PushPreferences {
    fn allows(&self, kind: NotificationKind) -> bool {
        match kind {
            NotificationKind::WaitingForInput => self.waiting_for_input,
            NotificationKind::AgentTaskStarted => self.agent_task_started,
            NotificationKind::AgentTaskNotify => self.agent_task_notify,
            NotificationKind::Test => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDigest {
    pub total_count: u32,
    pub waiting_for_input_count: u32,
    pub agent_task_started_count: u32,
    pub agent_task_notify_count: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub queued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_event_at: OffsetDateTime,
    pub latest_title: String,
    pub latest_body: String,
    #[serde(default)]
    pub latest_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DispatchCounts {
    pub ok: usize,
    pub fail: usize,
    pub queued: usize,
}

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
    #[serde(default)]
    pub prefs: PushPreferences,
    #[serde(default)]
    pub pending_digest: Option<PendingDigest>,
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
        let mut st = self.state.lock();
        let existing = st.subs.get(&id).cloned();
        let stored = StoredSubscription {
            id: id.clone(),
            sub,
            created_at: existing
                .as_ref()
                .map(|sub| sub.created_at)
                .unwrap_or_else(time::OffsetDateTime::now_utc),
            label: label.or_else(|| existing.as_ref().and_then(|sub| sub.label.clone())),
            prefs: existing
                .as_ref()
                .map(|sub| sub.prefs.clone())
                .unwrap_or_default(),
            pending_digest: existing.and_then(|sub| sub.pending_digest),
        };
        st.subs.insert(id, stored.clone());
        persist(&self.store_path, &st)?;
        Ok(stored)
    }

    pub fn update(
        &self,
        id: &str,
        label: Option<Option<String>>,
        prefs: Option<PushPreferences>,
    ) -> Result<StoredSubscription> {
        let mut st = self.state.lock();
        let stored = st.subs.get_mut(id).ok_or(ApiError::NotFound)?;
        if let Some(label) = label {
            stored.label = label.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        }
        if let Some(prefs) = prefs {
            stored.prefs = prefs;
        }
        let updated = stored.clone();
        persist(&self.store_path, &st)?;
        Ok(updated)
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
    pub async fn notify(
        &self,
        kind: NotificationKind,
        title: &str,
        body: &str,
        data: serde_json::Value,
    ) -> DispatchCounts {
        let now = OffsetDateTime::now_utc();
        let (subs, queued) = {
            let mut st = self.state.lock();
            let mut deliver = Vec::new();
            let mut queued = 0usize;
            let mut changed = false;
            for stored in st.subs.values_mut() {
                if !stored.prefs.allows(kind) {
                    continue;
                }
                if quiet_hours_active(&stored.prefs.quiet_hours, now) {
                    if stored.prefs.quiet_hours.digest {
                        queue_digest(stored, kind, title, body, &data, now);
                        queued += 1;
                        changed = true;
                    }
                    continue;
                }
                deliver.push(stored.clone());
            }
            if changed {
                let _ = persist(&self.store_path, &st);
            }
            (deliver, queued)
        };

        let mut counts = DispatchCounts {
            queued,
            ..DispatchCounts::default()
        };
        let mut prune: Vec<String> = Vec::new();
        for s in subs {
            match self.send_one(&s, title, body, &data).await {
                Ok(()) => counts.ok += 1,
                Err(PushSendErr::Gone) => {
                    prune.push(s.id);
                    counts.fail += 1;
                }
                Err(PushSendErr::Other(e)) => {
                    warn!(id = s.id, error = %e, "push send failed");
                    counts.fail += 1;
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
        counts
    }

    pub async fn flush_ready_digests(&self) -> DispatchCounts {
        let now = OffsetDateTime::now_utc();
        let ready = {
            let st = self.state.lock();
            st.subs
                .values()
                .filter_map(|stored| {
                    let digest = stored.pending_digest.clone()?;
                    (!quiet_hours_active(&stored.prefs.quiet_hours, now)).then_some((stored.clone(), digest))
                })
                .collect::<Vec<_>>()
        };

        let mut counts = DispatchCounts::default();
        let mut prune = Vec::new();
        let mut clear = Vec::new();
        for (stored, digest) in ready {
            let (title, body, data) = digest_notification_payload(&digest);
            match self.send_one(&stored, &title, &body, &data).await {
                Ok(()) => {
                    counts.ok += 1;
                    clear.push(stored.id);
                }
                Err(PushSendErr::Gone) => {
                    counts.fail += 1;
                    prune.push(stored.id);
                }
                Err(PushSendErr::Other(e)) => {
                    warn!(id = stored.id, error = %e, "digest push send failed");
                    counts.fail += 1;
                }
            }
        }
        if !prune.is_empty() || !clear.is_empty() {
            let mut st = self.state.lock();
            for id in clear {
                if let Some(stored) = st.subs.get_mut(&id) {
                    stored.pending_digest = None;
                }
            }
            for id in prune {
                st.subs.remove(&id);
            }
            let _ = persist(&self.store_path, &st);
        }
        counts
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

fn quiet_hours_active(quiet_hours: &QuietHours, now: OffsetDateTime) -> bool {
    if !quiet_hours.enabled {
        return false;
    }
    let start = quiet_hours.start_minute.min((24 * 60) - 1);
    let end = quiet_hours.end_minute.min((24 * 60) - 1);
    if start == end {
        return false;
    }
    let local = now + time::Duration::minutes(i64::from(quiet_hours.utc_offset_minutes));
    let minute = (u16::from(local.hour()) * 60) + u16::from(local.minute());
    if start < end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}

fn queue_digest(
    stored: &mut StoredSubscription,
    kind: NotificationKind,
    title: &str,
    body: &str,
    data: &serde_json::Value,
    now: OffsetDateTime,
) {
    let latest_url = data
        .get("url")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let digest = stored.pending_digest.get_or_insert_with(|| PendingDigest {
        total_count: 0,
        waiting_for_input_count: 0,
        agent_task_started_count: 0,
        agent_task_notify_count: 0,
        queued_at: now,
        last_event_at: now,
        latest_title: title.to_string(),
        latest_body: body.to_string(),
        latest_url: latest_url.clone(),
    });
    digest.total_count = digest.total_count.saturating_add(1);
    digest.last_event_at = now;
    digest.latest_title = title.to_string();
    digest.latest_body = body.to_string();
    digest.latest_url = latest_url;
    match kind {
        NotificationKind::WaitingForInput => {
            digest.waiting_for_input_count = digest.waiting_for_input_count.saturating_add(1)
        }
        NotificationKind::AgentTaskStarted => {
            digest.agent_task_started_count = digest.agent_task_started_count.saturating_add(1)
        }
        NotificationKind::AgentTaskNotify => {
            digest.agent_task_notify_count = digest.agent_task_notify_count.saturating_add(1)
        }
        NotificationKind::Test => {}
    }
}

fn digest_notification_payload(digest: &PendingDigest) -> (String, String, serde_json::Value) {
    let mut parts = Vec::new();
    if digest.waiting_for_input_count > 0 {
        parts.push(format!(
            "{} waiting-for-input",
            digest.waiting_for_input_count
        ));
    }
    if digest.agent_task_started_count > 0 {
        parts.push(format!(
            "{} task started",
            digest.agent_task_started_count
        ));
    }
    if digest.agent_task_notify_count > 0 {
        parts.push(format!(
            "{} task alert",
            digest.agent_task_notify_count
        ));
    }
    let summary = if parts.is_empty() {
        format!("{} queued notifications", digest.total_count)
    } else {
        parts.join(" • ")
    };
    let body = format!("{summary}. Latest: {}", digest.latest_title);
    let data = serde_json::json!({
        "kind": "digest",
        "queued_count": digest.total_count,
        "queued_at": digest.queued_at.to_string(),
        "last_event_at": digest.last_event_at.to_string(),
        "url": digest.latest_url,
    });
    ("MyDevEnv2 digest".to_string(), body, data)
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

    #[test]
    fn quiet_hours_handle_same_day_and_overnight_windows() {
        let base = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();

        let same_day = QuietHours {
            enabled: true,
            start_minute: 9 * 60,
            end_minute: 17 * 60,
            utc_offset_minutes: 0,
            digest: true,
        };
        assert!(!quiet_hours_active(
            &same_day,
            base.replace_time(time::Time::from_hms(8, 59, 0).unwrap())
        ));
        assert!(quiet_hours_active(
            &same_day,
            base.replace_time(time::Time::from_hms(12, 0, 0).unwrap())
        ));

        let overnight = QuietHours {
            enabled: true,
            start_minute: 22 * 60,
            end_minute: 6 * 60,
            utc_offset_minutes: 0,
            digest: true,
        };
        assert!(quiet_hours_active(
            &overnight,
            base.replace_time(time::Time::from_hms(23, 0, 0).unwrap())
        ));
        assert!(quiet_hours_active(
            &overnight,
            base.replace_time(time::Time::from_hms(2, 0, 0).unwrap())
        ));
        assert!(!quiet_hours_active(
            &overnight,
            base.replace_time(time::Time::from_hms(12, 0, 0).unwrap())
        ));
    }

    #[test]
    fn digest_payload_summarizes_counts() {
        let digest = PendingDigest {
            total_count: 4,
            waiting_for_input_count: 2,
            agent_task_started_count: 1,
            agent_task_notify_count: 1,
            queued_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            last_event_at: OffsetDateTime::from_unix_timestamp(1_700_000_100).unwrap(),
            latest_title: "Task wants attention".into(),
            latest_body: "Latest body".into(),
            latest_url: Some("/#/tasks".into()),
        };
        let (title, body, data) = digest_notification_payload(&digest);
        assert_eq!(title, "MyDevEnv2 digest");
        assert!(body.contains("2 waiting-for-input"));
        assert!(body.contains("1 task started"));
        assert!(body.contains("Latest: Task wants attention"));
        assert_eq!(data["kind"], "digest");
        assert_eq!(data["queued_count"], 4);
    }
}
