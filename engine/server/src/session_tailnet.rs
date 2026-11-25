//! A tailnet identity of its own for each opted-in session pod (WI-4713,
//! WI-4715).
//!
//! Tailscale SSH on the estate's servers answers only tailnet identities its
//! policy names, and a session pod egressing through its node has none. With
//! `ENGINE_POD_TAILNET=1` and `tailnet: true` in a template's pod policy, the
//! engine gives each of that template's pods its own ephemeral node:
//!
//! - Before the pod is created, the engine mints one auth key through the
//!   Tailscale API with its OAuth client (`TAILSCALE_VOGT_SESSION_OAUTH_SECRET`,
//!   read from the engine's environment only, never logged or put in argv).
//!   The key is single-use, ephemeral, preauthorized, carries
//!   `tag:vogt-session` and nothing else, expires in 600 s and is described
//!   `vogt <session-id>`, so the audit log names the session.
//! - The key goes into an immutable Secret owned by the pod (created after the
//!   pod, like the bootstrap Secret), mounted 0400 in the sidecar only. The
//!   agent container never mounts it, and the pod spec holds only its name.
//! - The sidecar is a native sidecar (an init container with `restartPolicy:
//!   Always`) running `tailscaled` in userspace mode as uid 1001, with every
//!   capability dropped and a read-only root filesystem. It serves SOCKS5 on
//!   127.0.0.1:1055 and an HTTP proxy on 127.0.0.1:1056, as node `vs-<id>`.
//!   It has no probes, so the agent never waits on it, and no socket or CLI is
//!   shared with the agent, so a session cannot reconfigure or re-tag its node.
//!   On TERM it logs the node out, so the node leaves with the pod.
//!
//! A mint that fails (no OAuth secret, the API refusing) never stops the
//! session: the pod starts without the sidecar and the session's output says
//! why. Errors carry HTTP statuses only, never a key or the client secret.
use std::{collections::BTreeMap, future::Future, pin::Pin, time::Duration};

use k8s_openapi::api::core::v1::{
    Capabilities, Container, EmptyDirVolumeSource, EnvVar, Pod, ResourceRequirements, Secret,
    SecretVolumeSource, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::{apimachinery::pkg::apis::meta::v1::OwnerReference, ByteString};
use kube::api::ObjectMeta;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ApiError, Result};
use crate::pod_backend::{pod_name, PodConfig, ENGINE_LABEL, SESSION_CONTAINER};

/// The only tag a session pod's key may carry.
pub const SESSION_TAG: &str = "tag:vogt-session";
/// How long a minted key stays usable. The sidecar uses it within seconds of
/// the pod starting; a key a failed create left behind dies on its own.
pub const KEY_EXPIRY: Duration = Duration::from_secs(600);
/// The pinned sidecar image (`ENGINE_POD_TAILNET_IMAGE` overrides it).
pub const DEFAULT_IMAGE: &str = "ghcr.io/tailscale/tailscale:v1.104.7";
/// The Tailscale API (`ENGINE_POD_TAILNET_API_URL` overrides it, for tests).
pub const DEFAULT_API_URL: &str = "https://api.tailscale.com";
/// The engine's OAuth client secret. Read from the engine's own environment
/// when the factory is built; it is never part of [`PodConfig`].
pub const OAUTH_SECRET_ENV: &str = "TAILSCALE_VOGT_SESSION_OAUTH_SECRET";
/// The OAuth client id, optional: Tailscale identifies the client by its
/// secret and ignores the id in a client-credentials grant.
pub const OAUTH_CLIENT_ID_ENV: &str = "TAILSCALE_VOGT_SESSION_OAUTH_CLIENT_ID";
/// Where the agent reaches the tailnet: the sidecar's proxies, on the pod's
/// loopback.
pub const SOCKS5_ADDR: &str = "127.0.0.1:1055";
pub const HTTP_PROXY_ADDR: &str = "127.0.0.1:1056";
/// The pod annotation that carries the tailnet state: `requested` from the
/// template, then `sidecar` or `unavailable` once the engine has tried.
pub const TAILNET_ANNOTATION: &str = "vogt.dev/tailnet";
pub const TAILNET_REQUESTED: &str = "requested";
pub const TAILNET_SIDECAR: &str = "sidecar";
pub const TAILNET_UNAVAILABLE: &str = "unavailable";

pub const SIDECAR_CONTAINER: &str = "tailnet";
pub const SIDECAR_UID: i64 = 1001;
const KEY_VOLUME: &str = "tailnet-key";
const KEY_MOUNT_DIR: &str = "/run/vogt-tailnet";
const KEY_SECRET_KEY: &str = "authkey";
const STATE_VOLUME: &str = "tailnet-state";
const STATE_DIR: &str = "/var/lib/tailscale";
const API_TIMEOUT: Duration = Duration::from_secs(10);

/// The sidecar's entrypoint. It starts `tailscaled` in userspace mode with
/// its state on the sidecar's own emptyDir, joins once with the key file
/// (`TS_AUTH_ONCE`: a restarted sidecar whose node is still logged in does
/// not try the spent key again), and logs the node out on TERM. `tailscale
/// up` runs in the background so the TERM trap fires at once even while it
/// is still joining. A failed join leaves the session running without the
/// tailnet; the key is only ever named by its path.
const SIDECAR_SCRIPT: &str = r#"set -u
sock="$TS_STATE_DIR/tailscaled.sock"
tailscaled --tun=userspace-networking --statedir="$TS_STATE_DIR" --socket="$sock" \
  --socks5-server="$TS_SOCKS5_SERVER" \
  --outbound-http-proxy-listen="$TS_OUTBOUND_HTTP_PROXY_LISTEN" &
pid=$!
leave() {
  tailscale --socket="$sock" logout >/dev/null 2>&1 || true
  kill -TERM "$pid" 2>/dev/null
  wait "$pid"
  exit 0
}
trap leave TERM INT
state() {
  tailscale --socket="$sock" status --json 2>/dev/null \
    | sed -n 's/^[[:space:]]*"BackendState":[[:space:]]*"\([A-Za-z]*\)".*/\1/p'
}
s=""
i=0
while [ "$i" -lt 100 ]; do
  s=$(state)
  case "$s" in
    ""|NoState) sleep 0.1; i=$((i+1)) ;;
    *) break ;;
  esac
done
if [ "${TS_AUTH_ONCE:-}" = true ] && [ -n "$s" ] && [ "$s" != NeedsLogin ] && [ "$s" != NoState ]; then
  echo "vogt-tailnet: node is $s; not using the auth key again"
else
  tailscale --socket="$sock" up --auth-key="file:$TS_AUTHKEY_FILE" --hostname="$TS_HOSTNAME" \
    --accept-dns=false --timeout=60s &
  up=$!
  if ! wait "$up"; then
    echo "vogt-tailnet: joining the tailnet failed; the session continues without it" >&2
  fi
fi
wait "$pid"
"#;

/// The deployment's tailnet settings (`ENGINE_POD_TAILNET=1`). Holds no
/// credential: the OAuth secret stays in the engine's environment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PodTailnet {
    /// The sidecar image, pinned by tag.
    pub image: String,
    /// The Tailscale API base URL, without a trailing slash.
    pub api_url: String,
}
impl Default for PodTailnet {
    fn default() -> Self {
        Self {
            image: DEFAULT_IMAGE.into(),
            api_url: DEFAULT_API_URL.into(),
        }
    }
}

/// One minted auth key. `Debug` never prints the key.
#[derive(Clone, PartialEq, Eq)]
pub struct TailnetKey {
    /// The key's id, which names it to the API (for revoking) and in the
    /// tailnet's audit log. Not a secret.
    pub id: String,
    key: String,
}
impl TailnetKey {
    pub fn new(id: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            key: key.into(),
        }
    }
    /// The key itself: written into the pod's Secret and nowhere else.
    pub fn expose(&self) -> &str {
        &self.key
    }
}
impl std::fmt::Debug for TailnetKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TailnetKey")
            .field("id", &self.id)
            .field("key", &"<redacted>")
            .finish()
    }
}

pub type MintFuture<'a> = Pin<Box<dyn Future<Output = Result<TailnetKey>> + Send + 'a>>;
pub type RevokeFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Mints and revokes session keys. Tests substitute a fake.
pub trait TailnetMinter: Send + Sync {
    /// A new single-use key for session `id`'s pod.
    fn mint(&self, id: Uuid) -> MintFuture<'_>;
    /// Revoke an unused key (the pod it was for was never made). Unknown
    /// (404) is success.
    fn revoke<'a>(&'a self, key_id: &'a str) -> RevokeFuture<'a>;
}

/// The key's description: `vogt <session-id>` (Tailscale allows 50
/// characters: letters, digits, spaces and hyphens).
pub fn key_description(id: Uuid) -> String {
    format!("vogt {id}")
}

/// The pod's node name on the tailnet.
pub fn node_hostname(id: Uuid) -> String {
    format!("vs-{}", id.simple())
}

/// The Secret that carries one pod's key. Like the bootstrap Secret it has a
/// fresh random part, so a wake right after a teardown can never collide with
/// the Secret the garbage collector has not removed yet, and the engine never
/// deletes a Secret (its Role grants `create` only).
pub fn tailnet_secret_name(id: Uuid) -> String {
    let nonce = Uuid::new_v4().simple().to_string();
    format!("vogt-ts-{}-{}", id.simple(), &nonce[..8])
}

/// A key id is used in a URL path, so it must be a short alphanumeric token.
fn valid_key_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The Tailscale API over HTTPS with the engine's OAuth client.
pub struct HttpTailnetMinter {
    api_url: String,
    client_id: String,
    client_secret: String,
    client: reqwest::Client,
}
impl std::fmt::Debug for HttpTailnetMinter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTailnetMinter")
            .field("api_url", &self.api_url)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
}
#[derive(Deserialize)]
struct KeyReply {
    id: String,
    key: String,
}

impl HttpTailnetMinter {
    pub fn new(api_url: &str, client_id: Option<String>, client_secret: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(API_TIMEOUT)
            .build()
            .map_err(|e| ApiError::Config(format!("tailscale API client: {e}")))?;
        Ok(Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            client_id: client_id
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| "vogt-engine".into()),
            client_secret,
            client,
        })
    }

    /// The minter for `cfg`, with the OAuth client read through `get` (the
    /// process environment in production). `None` when the secret is unset:
    /// every tailnet pod then starts without the sidecar and says why.
    pub fn from_lookup(cfg: &PodTailnet, get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>> {
        let Some(secret) = get(OAUTH_SECRET_ENV)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        Self::new(&cfg.api_url, get(OAUTH_CLIENT_ID_ENV), secret).map(Some)
    }

    /// A short-lived API token from the OAuth client. The secret travels in
    /// the form body only, never in the URL.
    async fn access_token(&self) -> Result<String> {
        let resp = self
            .client
            .post(format!("{}/api/v2/oauth/token", self.api_url))
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Internal(format!("tailscale OAuth: {}", e.without_url())))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ApiError::Internal(format!(
                "tailscale OAuth refused the client: HTTP {status}"
            )));
        }
        let reply: TokenReply = resp
            .json()
            .await
            .map_err(|_| ApiError::Internal("tailscale OAuth reply carries no token".into()))?;
        if reply.access_token.is_empty() {
            return Err(ApiError::Internal(
                "tailscale OAuth reply carries no token".into(),
            ));
        }
        Ok(reply.access_token)
    }
}

/// The key request: single-use, ephemeral, preauthorized, the session tag
/// only, a short expiry and the session's description.
fn key_request(id: Uuid) -> serde_json::Value {
    serde_json::json!({
        "capabilities": {
            "devices": {
                "create": {
                    "reusable": false,
                    "ephemeral": true,
                    "preauthorized": true,
                    "tags": [SESSION_TAG],
                }
            }
        },
        "expirySeconds": KEY_EXPIRY.as_secs(),
        "description": key_description(id),
    })
}

impl TailnetMinter for HttpTailnetMinter {
    fn mint(&self, id: Uuid) -> MintFuture<'_> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let resp = self
                .client
                .post(format!("{}/api/v2/tailnet/-/keys", self.api_url))
                .bearer_auth(&token)
                .json(&key_request(id))
                .send()
                .await
                .map_err(|e| {
                    ApiError::Internal(format!("tailscale API: {}", e.without_url()))
                })?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(ApiError::Internal(format!(
                    "tailscale API refused the session key: HTTP {status}"
                )));
            }
            let reply: KeyReply = resp.json().await.map_err(|_| {
                ApiError::Internal("tailscale API reply is not an auth key".into())
            })?;
            if !valid_key_id(&reply.id) || !reply.key.starts_with("tskey-auth-") {
                return Err(ApiError::Internal(
                    "tailscale API reply is not an auth key".into(),
                ));
            }
            Ok(TailnetKey::new(reply.id, reply.key))
        })
    }

    fn revoke<'a>(&'a self, key_id: &'a str) -> RevokeFuture<'a> {
        Box::pin(async move {
            if !valid_key_id(key_id) {
                return Err(ApiError::BadRequest("invalid tailscale key id".into()));
            }
            let token = self.access_token().await?;
            let resp = self
                .client
                .delete(format!("{}/api/v2/tailnet/-/keys/{key_id}", self.api_url))
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| {
                    ApiError::Internal(format!("tailscale API: {}", e.without_url()))
                })?;
            match resp.status().as_u16() {
                200..=299 | 404 => Ok(()),
                status => Err(ApiError::Internal(format!(
                    "tailscale API refused to revoke the key: HTTP {status}"
                ))),
            }
        })
    }
}

/// Whether the template asked for a tailnet sidecar on this pod.
pub fn requested(pod: &Pod) -> bool {
    pod.metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(TAILNET_ANNOTATION))
        .is_some_and(|v| v == TAILNET_REQUESTED)
}

/// Record on the pod what became of its tailnet request.
pub fn mark(pod: &mut Pod, value: &str) {
    pod.metadata
        .annotations
        .get_or_insert_with(BTreeMap::new)
        .insert(TAILNET_ANNOTATION.into(), value.into());
}

/// The name of the key Secret the pod's sidecar mounts, if it has one.
pub fn secret_name_in(pod: &Pod) -> Option<String> {
    pod.spec
        .as_ref()?
        .volumes
        .as_ref()?
        .iter()
        .find(|v| v.name == KEY_VOLUME)?
        .secret
        .as_ref()?
        .secret_name
        .clone()
}

fn quantity(v: &str) -> Quantity {
    Quantity(v.into())
}

fn env(name: &str, value: impl Into<String>) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: Some(value.into()),
        ..Default::default()
    }
}

/// The native sidecar container for session `id`.
fn sidecar(cfg: &PodTailnet, id: Uuid) -> Container {
    Container {
        name: SIDECAR_CONTAINER.into(),
        image: Some(cfg.image.clone()),
        command: Some(vec!["/bin/sh".into(), "-c".into(), SIDECAR_SCRIPT.into()]),
        env: Some(vec![
            env("TS_HOSTNAME", node_hostname(id)),
            env("TS_AUTH_ONCE", "true"),
            env("TS_USERSPACE", "true"),
            env("TS_SOCKS5_SERVER", SOCKS5_ADDR),
            env("TS_OUTBOUND_HTTP_PROXY_LISTEN", HTTP_PROXY_ADDR),
            env("TS_STATE_DIR", STATE_DIR),
            env("TS_AUTHKEY_FILE", format!("{KEY_MOUNT_DIR}/{KEY_SECRET_KEY}")),
            env("HOME", STATE_DIR),
        ]),
        volume_mounts: Some(vec![
            VolumeMount {
                name: KEY_VOLUME.into(),
                mount_path: KEY_MOUNT_DIR.into(),
                read_only: Some(true),
                ..Default::default()
            },
            VolumeMount {
                name: STATE_VOLUME.into(),
                mount_path: STATE_DIR.into(),
                ..Default::default()
            },
        ]),
        // A native sidecar: started before the session container and stopped
        // after it. With no probe the kubelet starts the session container
        // as soon as this one is running, joined or not.
        restart_policy: Some("Always".into()),
        resources: Some(ResourceRequirements {
            requests: Some(BTreeMap::from([
                ("cpu".into(), quantity("10m")),
                ("memory".into(), quantity("32Mi")),
            ])),
            limits: Some(BTreeMap::from([
                ("cpu".into(), quantity("250m")),
                ("memory".into(), quantity("128Mi")),
            ])),
            claims: None,
        }),
        security_context: Some(SecurityContext {
            run_as_user: Some(SIDECAR_UID),
            run_as_group: Some(SIDECAR_UID),
            run_as_non_root: Some(true),
            allow_privilege_escalation: Some(false),
            read_only_root_filesystem: Some(true),
            privileged: Some(false),
            capabilities: Some(Capabilities {
                drop: Some(vec!["ALL".into()]),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Give the pod its tailnet sidecar, reading the key from `secret_name`
/// (created after the pod by [`tailnet_secret`]). The key volume is mounted in
/// the sidecar alone; the session container gets only the proxy addresses.
pub fn attach(pod: &mut Pod, cfg: &PodTailnet, id: Uuid, secret_name: &str) {
    mark(pod, TAILNET_SIDECAR);
    let Some(spec) = pod.spec.as_mut() else {
        return;
    };
    spec.volumes.get_or_insert_with(Vec::new).extend([
        Volume {
            name: KEY_VOLUME.into(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(secret_name.into()),
                default_mode: Some(0o400),
                optional: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: STATE_VOLUME.into(),
            // The node key lives here: in memory, gone with the pod, and kept
            // across a sidecar restart so the spent auth key is not needed.
            empty_dir: Some(EmptyDirVolumeSource {
                medium: Some("Memory".into()),
                size_limit: Some(quantity("16Mi")),
            }),
            ..Default::default()
        },
    ]);
    // Last, so the existing init containers run as before and the sidecar
    // starts just ahead of the session container.
    spec.init_containers
        .get_or_insert_with(Vec::new)
        .push(sidecar(cfg, id));
    if let Some(agent) = spec
        .containers
        .iter_mut()
        .find(|c| c.name == SESSION_CONTAINER)
    {
        agent.env.get_or_insert_with(Vec::new).extend([
            env("VOGT_TAILNET_SOCKS5", SOCKS5_ADDR),
            env("VOGT_TAILNET_HTTP_PROXY", HTTP_PROXY_ADDR),
        ]);
    }
}

/// The immutable Secret holding the pod's key, owned by `owner` (the pod as
/// the API server returned it) so it goes with the pod.
pub fn tailnet_secret(
    cfg: &PodConfig,
    id: Uuid,
    name: &str,
    key: &TailnetKey,
    owner: &Pod,
) -> Secret {
    Secret {
        metadata: ObjectMeta {
            name: Some(name.into()),
            namespace: Some(cfg.namespace.clone()),
            labels: Some(BTreeMap::from([
                ("vogt.session".into(), id.to_string()),
                ("vogt.kind".into(), "tailnet".into()),
                (ENGINE_LABEL.into(), cfg.engine_id.clone()),
            ])),
            owner_references: Some(vec![OwnerReference {
                api_version: "v1".into(),
                kind: "Pod".into(),
                name: owner.metadata.name.clone().unwrap_or_else(|| pod_name(id)),
                uid: owner.metadata.uid.clone().unwrap_or_default(),
                ..Default::default()
            }]),
            ..Default::default()
        },
        type_: Some("Opaque".into()),
        immutable: Some(true),
        data: Some(BTreeMap::from([(
            KEY_SECRET_KEY.to_string(),
            ByteString(key.expose().as_bytes().to_vec()),
        )])),
        ..Default::default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub const KEY: &str = "tskey-auth-kTEST1CNTRL-s3cretTailnetKeyValue";
    pub const KEY_ID: &str = "kTEST1CNTRL";
    const CLIENT_SECRET: &str = "tskey-client-kCLIENT-oauthS3cretValue";

    /// Records every call; `mint` answers `reply`.
    pub struct FakeMinter {
        pub reply: Mutex<Option<Result<TailnetKey>>>,
        pub minted: Mutex<Vec<Uuid>>,
        pub revoked: Mutex<Vec<String>>,
    }
    impl FakeMinter {
        pub fn answering(reply: Result<TailnetKey>) -> Arc<Self> {
            Arc::new(Self {
                reply: Mutex::new(Some(reply)),
                minted: Mutex::default(),
                revoked: Mutex::default(),
            })
        }
        pub fn minting() -> Arc<Self> {
            Self::answering(Ok(TailnetKey::new(KEY_ID, KEY)))
        }
    }
    impl TailnetMinter for FakeMinter {
        fn mint(&self, id: Uuid) -> MintFuture<'_> {
            self.minted.lock().unwrap().push(id);
            let reply = self
                .reply
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Err(ApiError::Internal("no reply".into())));
            Box::pin(async move { reply })
        }
        fn revoke<'a>(&'a self, key_id: &'a str) -> RevokeFuture<'a> {
            self.revoked.lock().unwrap().push(key_id.to_string());
            Box::pin(async { Ok(()) })
        }
    }

    type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

    /// A fake Tailscale API: the token endpoint answers `token_status`, the
    /// key endpoints `key_status` with `key_reply`. Records (method, path,
    /// authorization, body).
    async fn serve(token_status: u16, key_status: u16, key_reply: serde_json::Value) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method,
                  uri: axum::http::Uri,
                  headers: axum::http::HeaderMap,
                  body: axum::body::Bytes| {
                let (log, key_reply) = (log.clone(), key_reply.clone());
                async move {
                    log.lock().unwrap().push((
                        method.to_string(),
                        uri.to_string(),
                        headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string(),
                        String::from_utf8_lossy(&body).into_owned(),
                    ));
                    if uri.path() == "/api/v2/oauth/token" {
                        // A refusal that echoes what it was sent must still
                        // not reach an error message.
                        let reply = if token_status == 200 {
                            serde_json::json!({"access_token": "api-access-token", "token_type": "Bearer", "expires_in": 3600})
                        } else {
                            serde_json::json!({"message": String::from_utf8_lossy(&body)})
                        };
                        return (axum::http::StatusCode::from_u16(token_status).unwrap(), axum::Json(reply));
                    }
                    (axum::http::StatusCode::from_u16(key_status).unwrap(), axum::Json(key_reply))
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    fn key_reply() -> serde_json::Value {
        serde_json::json!({
            "id": KEY_ID,
            "key": KEY,
            "created": "2026-11-25T00:00:00Z",
            "expires": "2026-11-25T00:10:00Z",
        })
    }

    fn minter(url: &str) -> HttpTailnetMinter {
        HttpTailnetMinter::new(url, None, CLIENT_SECRET.into()).unwrap()
    }

    #[tokio::test]
    async fn the_key_is_single_use_ephemeral_preauthorized_session_tagged_and_short_lived() {
        let id = Uuid::new_v4();
        let (url, seen) = serve(200, 200, key_reply()).await;
        let key = minter(&format!("{url}/")).mint(id).await.unwrap();
        assert_eq!(key.id, KEY_ID);
        assert_eq!(key.expose(), KEY);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        // The OAuth secret is in the token request's form body, not its URL.
        let (method, path, _, body) = &seen[0];
        assert_eq!((method.as_str(), path.as_str()), ("POST", "/api/v2/oauth/token"));
        assert!(body.contains("grant_type=client_credentials"), "{body}");
        assert!(body.contains("client_secret="), "{body}");
        assert!(!path.contains("oauthS3cret"));
        let (method, path, auth, body) = &seen[1];
        assert_eq!((method.as_str(), path.as_str()), ("POST", "/api/v2/tailnet/-/keys"));
        assert_eq!(auth, "Bearer api-access-token");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let create = &body["capabilities"]["devices"]["create"];
        assert_eq!(create["reusable"], false);
        assert_eq!(create["ephemeral"], true);
        assert_eq!(create["preauthorized"], true);
        assert_eq!(create["tags"], serde_json::json!(["tag:vogt-session"]));
        assert_eq!(body["expirySeconds"], 600);
        assert_eq!(body["description"], format!("vogt {id}"));
        assert!(body["description"].as_str().unwrap().len() <= 50);
    }

    #[tokio::test]
    async fn a_refused_mint_fails_without_a_secret_in_the_error() {
        // The OAuth client refused, its reply echoing the request.
        let (url, _) = serve(401, 200, key_reply()).await;
        let err = minter(&url).mint(Uuid::new_v4()).await.expect_err("refused");
        let text = format!("{err} {err:?}");
        assert!(text.contains("HTTP 401"), "{text}");
        assert!(!text.contains("oauthS3cret"), "{text}");
        // The key endpoint refused, or answered with something not a key.
        for (status, reply) in [
            (403, key_reply()),
            (500, key_reply()),
            (200, serde_json::json!({"id": KEY_ID, "key": "not-a-key"})),
            (200, serde_json::json!({"id": "../x", "key": KEY})),
        ] {
            let (url, _) = serve(200, status, reply).await;
            let err = minter(&url).mint(Uuid::new_v4()).await.expect_err("refused");
            let text = format!("{err} {err:?}");
            assert!(!text.contains("s3cretTailnetKeyValue"), "{text}");
            assert!(!text.contains("oauthS3cret"), "{text}");
        }
    }

    #[tokio::test]
    async fn revoke_deletes_by_id_and_unknown_is_done() {
        let (url, seen) = serve(200, 404, serde_json::json!({})).await;
        minter(&url).revoke(KEY_ID).await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[1].1,
            format!("/api/v2/tailnet/-/keys/{KEY_ID}")
        );
        assert!(minter(&url).revoke("../../x").await.is_err());
    }

    #[test]
    fn no_secret_means_no_minter_and_debug_never_prints_one() {
        let cfg = PodTailnet::default();
        assert!(HttpTailnetMinter::from_lookup(&cfg, |_| None).unwrap().is_none());
        assert!(HttpTailnetMinter::from_lookup(&cfg, |_| Some("  ".into()))
            .unwrap()
            .is_none());
        let m = HttpTailnetMinter::from_lookup(&cfg, |name| {
            (name == OAUTH_SECRET_ENV).then(|| CLIENT_SECRET.to_string())
        })
        .unwrap()
        .unwrap();
        assert!(!format!("{m:?}").contains("oauthS3cret"));
        let key = TailnetKey::new(KEY_ID, KEY);
        assert!(!format!("{key:?}").contains("s3cretTailnetKeyValue"));
    }

    #[test]
    fn names_fit_their_limits() {
        let id = Uuid::new_v4();
        assert!(node_hostname(id).len() <= 63);
        assert!(node_hostname(id).starts_with("vs-"));
        let a = tailnet_secret_name(id);
        assert!(a.starts_with(&format!("vogt-ts-{}-", id.simple())), "{a}");
        assert!(a.len() <= 253);
        assert_ne!(a, tailnet_secret_name(id), "each pod gets a fresh name");
    }
}
