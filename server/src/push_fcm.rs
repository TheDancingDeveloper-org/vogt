//! Minimal Firebase Cloud Messaging HTTP v1 sender.
//!
//! Service-account flow:
//!   1. Read the JSON (downloaded from Firebase console → Service Accounts).
//!   2. Mint an RS256 JWT scoped to `firebasecloudmessaging`.
//!   3. Exchange it at `oauth2.googleapis.com/token` for an access token.
//!      Cache until ~30s before expiry.
//!   4. POST a message to
//!      `fcm.googleapis.com/v1/projects/{project_id}/messages:send`.

use std::time::{Duration, Instant};

use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, Result};

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccount {
    pub project_id: String,
    pub private_key: String,
    pub client_email: String,
    pub token_uri: String,
}

impl ServiceAccount {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| ApiError::Config(format!("fcm service-account JSON: {e}")))
    }
}

#[derive(Debug, Serialize)]
struct Claims {
    iss: String,
    scope: String,
    aud: String,
    iat: u64,
    exp: u64,
}

#[derive(Debug, Deserialize)]
struct OauthResp {
    access_token: String,
    expires_in: u64,
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    /// Wall clock at which the cached token expires (we refresh a bit before).
    expires_at: Instant,
}

pub struct FcmSender {
    sa: ServiceAccount,
    http: reqwest::Client,
    cache: Mutex<Option<CachedToken>>,
}

impl FcmSender {
    pub fn new(sa: ServiceAccount) -> Self {
        Self {
            sa,
            http: reqwest::Client::new(),
            cache: Mutex::new(None),
        }
    }

    pub fn project_id(&self) -> &str {
        &self.sa.project_id
    }

    async fn access_token(&self) -> Result<String> {
        if let Some(t) = self.cache.lock().clone() {
            if Instant::now() + Duration::from_secs(30) < t.expires_at {
                return Ok(t.token);
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| ApiError::Internal(format!("clock: {e}")))?
            .as_secs();
        let claims = Claims {
            iss: self.sa.client_email.clone(),
            scope: "https://www.googleapis.com/auth/firebase.messaging".into(),
            aud: self.sa.token_uri.clone(),
            iat: now,
            exp: now + 3600,
        };
        let key = EncodingKey::from_rsa_pem(self.sa.private_key.as_bytes())
            .map_err(|e| ApiError::Config(format!("fcm private_key: {e}")))?;
        let jwt = encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|e| ApiError::Internal(format!("jwt encode: {e}")))?;

        let resp = self
            .http
            .post(&self.sa.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Internal(format!("oauth2 request: {e}")))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Internal(format!("oauth2 {body}")));
        }
        let oauth: OauthResp = resp
            .json()
            .await
            .map_err(|e| ApiError::Internal(format!("oauth2 parse: {e}")))?;

        let cached = CachedToken {
            token: oauth.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(oauth.expires_in),
        };
        *self.cache.lock() = Some(cached);
        Ok(oauth.access_token)
    }

    /// Send a notification to a single FCM device token.
    /// `title` and `body` populate the notification panel; `data` is the
    /// JSON payload the app sees on click.
    pub async fn send(
        &self,
        device_token: &str,
        title: &str,
        body: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        let access = self.access_token().await?;
        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.sa.project_id
        );

        // FCM `data` values must be strings — stringify whatever was passed.
        let data_map: serde_json::Map<String, serde_json::Value> = match data {
            serde_json::Value::Object(m) => m
                .into_iter()
                .map(|(k, v)| {
                    let s = match v {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (k, serde_json::Value::String(s))
                })
                .collect(),
            _ => serde_json::Map::new(),
        };

        let msg = serde_json::json!({
            "message": {
                "token": device_token,
                "notification": { "title": title, "body": body },
                "data": data_map,
                "android": { "priority": "HIGH" }
            }
        });

        let resp = self
            .http
            .post(&url)
            .bearer_auth(access)
            .json(&msg)
            .send()
            .await
            .map_err(|e| ApiError::Internal(format!("fcm send: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Internal(format!("fcm {status}: {body}")));
        }
        Ok(())
    }
}

/// urlsafe base64 helper used by VAPID/web-push too; lives here because it's
/// the only place we need it outside the web-push crate.
#[allow(dead_code)]
pub fn b64_url_no_pad(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}
