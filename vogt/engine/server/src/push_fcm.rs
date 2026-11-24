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

/// Mint the RS256 service-account assertion that FCM exchanges for an access
/// token. Kept standalone (sync, no network) so a unit test can exercise the
/// exact crypto path that regressed: `jsonwebtoken` 10.x aborts an RS256
/// `encode` unless a crypto backend is selected (the `rust_crypto` feature) or
/// a process `CryptoProvider` is installed — a bare `jsonwebtoken = "10.4"`
/// panicked here on every FCM send.
fn sign_service_account_jwt(sa: &ServiceAccount, now: u64) -> Result<String> {
    let claims = Claims {
        iss: sa.client_email.clone(),
        scope: "https://www.googleapis.com/auth/firebase.messaging".into(),
        aud: sa.token_uri.clone(),
        iat: now,
        exp: now + 3600,
    };
    let key = EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
        .map_err(|e| ApiError::Config(format!("fcm private_key: {e}")))?;
    encode(&Header::new(Algorithm::RS256), &claims, &key)
        .map_err(|e| ApiError::Internal(format!("jwt encode: {e}")))
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
        let jwt = sign_service_account_jwt(&self.sa, now)?;

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
                "android": {
                    "priority": "HIGH",
                    "notification": { "channel_id": "vogt-alerts" }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway 2048-bit RSA key generated only for this test — it protects
    // nothing. Its sole job is to drive the RS256 signing path so a future
    // `jsonwebtoken` bump that drops the crypto backend fails this test instead
    // of panicking in production on the first FCM send (see #651).
    const TEST_RSA_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCwU2YdZrx4eRtn
jTMCXIkDykCruarfCUskyF7YiOmjqaK4SH7pD+HCyBxqwxvz2Npz9XMU5QSM3z5I
MQC6FsVVVND7J960Bjn7xleXh+hLn0t3fgMTKfxCeh4lBxBbw5cDf9hKSdwwaGlI
7HE2VF2GorP6AM/5xOGdLd/8MFWUtxWIglJjKecXaY2aeqd7qUHZxxMUf9ORRt2s
1sA6mHqjyS1FaN+Er80zsUGZf+yi+nv6s6vTBH1dOROG4FyLGQgAO7OtphP/MpIm
IgKqZaW/dig2+STPE75aZc8iAI1KdgZHLSee2aO7R/lWSS0P9aDJ1zjKtgcqnklZ
gGRN5yLtAgMBAAECggEAFlS5ngeg3vEXk2nCiA4IHD7FKfp9tXmL1sC5olrUnDGj
tgsYZ5PnV6/YSX0kUSGpqP3w8Th/9wde7+2k1eyEWWZAgU5vWgVf2o8oELzZopou
CJgg85Bhrlmg77eRTs3alkreranlBQx0KRQo0mSk46Q/fu4TaOKSYEGrGFlWvVqs
T+lCll1MapYIi9XKrJ/F4M2jg5Dk2FyQ5PM16+97Nc7sry3Nluha3lTPRNCyu+Tx
ma7+W/Xmf6ENvL2Qe+DuRQ6X+4eIYGocuf3C2WQ4Z2vWTKOivClHWLZ21fC9se95
4NScbLGyMs+uyOJRq8lLjqJ00BPQYOWJerkHvhw9AQKBgQDxuIXeU+HM6Y6Ke57D
qJfp48hdfE1VUWo2zRTwoB8N7VtK5K7M6pMbA06CJynAkPIWjuwYxuTpCMoiusyK
Wuzs06PBWPtIXzRim+knePNZj/4TP3W9cmPR80bJkAH5OMouapwGtUbn2v02YKWg
sUWZ2uD5opgbPSl48EyiJeKlHQKBgQC6vezUy8zEMPzX4dcf1TaGdHe6nZzzqYyq
xnZXVzFHCz6OLBfQFO6fegy1CHuxDH2towhYG2nl1SAvqY3sTIEq55njdd6dfCNJ
MswDzyWBABYR/b7jmv2KYgHL0asQ1yf2cuHmRQ0H+2BNfY/5++W2/VWhp2tDQQth
PlBtrAccEQKBgQCiK9tck0h90wB39oJI4Nay68ikt92mbQ3moGb5HWRYUrOaJp0E
DCkPyYMlnSLM+DIDYnYSFXbExcnfzRWniWNFbKSD9q/4GS+rLNEbU3Fo2EttHHlg
1dsUo7QNTRmV3D0BMTNm6L4elfSmQ+c7TVPV6lxf/28vbzRg2E+BxgsuHQKBgD48
ojvhmGMiuIHyoNHZK4zwMB8f/Hkw3tLjxfkh9ChrVPPpOh59e3HnKWkEZMUDFrfS
CCvlJwwUVd/MsKI6dSopeh5W4FS4VMJGjbwPQ76UmsZwPTh6JVoDg/m77Tl1m7oF
W8h05vzsCJKK8HsuOscb83gm5hbKDNLiA8gpd00hAoGADXWzjbTUcY3XUN+C8p19
vclvKPGvKdcMHgwl3de3Aeh5t7/v6G8RAXkHrQSkpp8icU/EcOW9IimrxX4OBRRz
2FHAVfyZtH5NqBFocwHIU4RSWBU31w2dnE7vn1bD6EZpRccVA8uhIPV8tFRS+Pn+
+BxHGVbe+hwuiC/SMdg6iMc=
-----END PRIVATE KEY-----
"#;

    fn test_sa() -> ServiceAccount {
        ServiceAccount {
            project_id: "test-project".into(),
            private_key: TEST_RSA_PEM.into(),
            client_email: "svc@test-project.iam.gserviceaccount.com".into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        }
    }

    #[test]
    fn mints_rs256_service_account_assertion() {
        // Must not panic (the jsonwebtoken 10.x CryptoProvider regression) and
        // must yield a well-formed, signed three-segment JWT.
        let jwt = sign_service_account_jwt(&test_sa(), 1_700_000_000)
            .expect("RS256 service-account JWT must sign");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must be header.payload.signature");
        assert!(!parts[2].is_empty(), "signature segment must be non-empty");
    }
}
