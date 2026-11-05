//! Per-session kache write credentials (WI-3242, ops PR #288
//! `apps/vogt-sessions/docs/kache-per-session-keys.md`).
//!
//! Every session pod used to get one read/write key on a shared Garage bucket,
//! so a prompt-injected session could upload a poisoned artefact under a hot
//! key for the next session to run. Now:
//!
//! - the shared `ENGINE_POD_CACHE_SECRET` holds only a **read-only** key for
//!   the shared `kache` bucket (ops' side: the key itself is read-only);
//! - when `ENGINE_POD_CACHE_PROVISIONER_URL` is set, the engine asks the
//!   trusted provisioner for a session allocation before it creates the pod:
//!   a bucket and a read/write key that reach that bucket only. The engine
//!   never holds the Garage admin token.
//!
//! The request carries no identifier: the provisioner mints the
//! `allocation_id`, and the engine only stamps it on the pod label
//! [`ALLOCATION_LABEL`] and hands it back to `DELETE` at teardown. The key
//! reaches the pod through an immutable Secret owned by the pod (created after
//! it, like the bootstrap Secret), referenced by `envFrom`; its values are
//! never in the pod spec, a log or an error. A provisioner at its allocation
//! cap answers 429 and the session runs with the shared read-only key only.
//! Anything else that fails fails the pod create, closed.
use std::{future::Future, path::PathBuf, pin::Pin, time::Duration};

use k8s_openapi::api::core::v1::{EnvFromSource, Pod, Secret, SecretEnvSource};
use k8s_openapi::{apimachinery::pkg::apis::meta::v1::OwnerReference, ByteString};
use kube::api::ObjectMeta;
use serde::Deserialize;
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::error::{ApiError, Result};
use crate::pod_backend::{pod_name, PodConfig, ENGINE_LABEL};

/// The pod label carrying the provisioner's allocation id. The provisioner's
/// sweep compares it with live pods to find abandoned allocations.
pub const ALLOCATION_LABEL: &str = "vogt.dev/session-cache-id";
/// How long the engine asks an allocation to live: a 12 h session clock plus
/// the design's 24 h slack. The provisioner clamps it; teardown is the real
/// end, expiry only the backstop.
pub const ALLOCATION_TTL: Duration = Duration::from_secs(36 * 3600);
const PROVISIONER_TIMEOUT: Duration = Duration::from_secs(15);

/// Secret keys, as the design names them.
pub const SESSION_ACCESS_KEY_ID: &str = "KACHE_SESSION_ACCESS_KEY_ID";
pub const SESSION_SECRET_KEY: &str = "KACHE_SESSION_SECRET_KEY";
pub const SESSION_BUCKET: &str = "KACHE_SESSION_BUCKET";

/// One session's bucket and key. `Debug` never prints the secret.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct CacheAllocation {
    pub allocation_id: String,
    pub bucket_name: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}
impl std::fmt::Debug for CacheAllocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheAllocation")
            .field("allocation_id", &self.allocation_id)
            .field("bucket_name", &self.bucket_name)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .finish()
    }
}

/// An allocation id is used in a label value and a Secret name, so it must be
/// a short lowercase token. The provisioner mints 128-bit base32 (26 chars).
pub fn valid_allocation_id(id: &str) -> bool {
    (16..=52).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn check(alloc: CacheAllocation) -> Result<CacheAllocation> {
    if !valid_allocation_id(&alloc.allocation_id) {
        return Err(ApiError::Internal(
            "cache provisioner returned an invalid allocation id".into(),
        ));
    }
    if alloc.bucket_name != format!("vogt-session-{}", alloc.allocation_id) {
        return Err(ApiError::Internal(
            "cache provisioner returned a bucket that is not the allocation's own".into(),
        ));
    }
    if alloc.access_key_id.is_empty() || alloc.secret_access_key.is_empty() {
        return Err(ApiError::Internal(
            "cache provisioner returned an empty key".into(),
        ));
    }
    Ok(alloc)
}

pub type AllocateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<CacheAllocation>>> + Send + 'a>>;
pub type ReleaseFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// The trusted provisioner, seen from the engine. Tests substitute a fake.
pub trait CacheProvisioner: Send + Sync {
    /// A new allocation, or `None` when the provisioner is at its cap (the
    /// session then runs with the shared read-only key only).
    fn allocate(&self) -> AllocateFuture<'_>;
    /// Tear the allocation down. Unknown (404) is success.
    fn release<'a>(&'a self, allocation_id: &'a str) -> ReleaseFuture<'a>;
}

/// The provisioner's HTTP API: `POST {url}/internal/cache/sessions` and
/// `DELETE {url}/internal/cache/sessions/<id>`, bearer from `token_file`
/// (read per request, so a projected ServiceAccount token can rotate).
pub struct HttpCacheProvisioner {
    url: String,
    token_file: Option<PathBuf>,
    client: reqwest::Client,
}
impl HttpCacheProvisioner {
    pub fn new(url: &str, token_file: Option<PathBuf>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(PROVISIONER_TIMEOUT)
            .build()
            .map_err(|e| ApiError::Config(format!("cache provisioner client: {e}")))?;
        Ok(Self {
            url: url.trim_end_matches('/').to_string(),
            token_file,
            client,
        })
    }
    pub fn from_config(cfg: &PodConfig) -> Result<Option<Self>> {
        cfg.cache_provisioner_url
            .as_deref()
            .map(|url| {
                Self::new(
                    url,
                    cfg.cache_provisioner_token_file.clone().map(PathBuf::from),
                )
            })
            .transpose()
    }
    fn authed(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let Some(path) = &self.token_file else {
            return Ok(req);
        };
        let token = std::fs::read_to_string(path)
            .map_err(|e| ApiError::Internal(format!("reading the cache provisioner token: {e}")))?;
        Ok(req.bearer_auth(token.trim()))
    }
}
impl CacheProvisioner for HttpCacheProvisioner {
    fn allocate(&self) -> AllocateFuture<'_> {
        Box::pin(async move {
            let expires = time::OffsetDateTime::now_utc() + ALLOCATION_TTL;
            let expires = expires
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|e| ApiError::Internal(format!("formatting expiry: {e}")))?;
            let req = self
                .client
                .post(format!("{}/internal/cache/sessions", self.url))
                .json(&serde_json::json!({ "expires_at": expires }));
            let resp = self.authed(req)?.send().await.map_err(|e| {
                ApiError::Internal(format!("cache provisioner: {}", e.without_url()))
            })?;
            match resp.status().as_u16() {
                201 | 200 => {
                    let alloc: CacheAllocation = resp.json().await.map_err(|_| {
                        ApiError::Internal("cache provisioner reply is not an allocation".into())
                    })?;
                    check(alloc).map(Some)
                }
                429 => Ok(None),
                status => Err(ApiError::Internal(format!(
                    "cache provisioner refused the allocation: HTTP {status}"
                ))),
            }
        })
    }
    fn release<'a>(&'a self, allocation_id: &'a str) -> ReleaseFuture<'a> {
        Box::pin(async move {
            if !valid_allocation_id(allocation_id) {
                return Err(ApiError::BadRequest("invalid cache allocation id".into()));
            }
            let req = self.client.delete(format!(
                "{}/internal/cache/sessions/{allocation_id}",
                self.url
            ));
            let resp = self.authed(req)?.send().await.map_err(|e| {
                ApiError::Internal(format!("cache provisioner: {}", e.without_url()))
            })?;
            match resp.status().as_u16() {
                200..=299 | 404 => Ok(()),
                status => Err(ApiError::Internal(format!(
                    "cache provisioner refused the release: HTTP {status}"
                ))),
            }
        })
    }
}

/// The name of the Secret carrying one allocation's key.
pub fn session_cache_secret_name(allocation_id: &str) -> String {
    format!("vogt-session-cache-{allocation_id}")
}

/// Stamp the allocation on the pod: the label the sweep reads, and an
/// `envFrom` of the session's Secret, last so it wins over any shared source.
pub fn attach(pod: &mut Pod, allocation_id: &str) {
    pod.metadata
        .labels
        .get_or_insert_with(BTreeMap::new)
        .insert(ALLOCATION_LABEL.into(), allocation_id.into());
    if let Some(container) = pod.spec.as_mut().and_then(|s| s.containers.first_mut()) {
        container
            .env_from
            .get_or_insert_with(Vec::new)
            .push(EnvFromSource {
                secret_ref: Some(SecretEnvSource {
                    name: session_cache_secret_name(allocation_id),
                    optional: Some(false),
                }),
                ..Default::default()
            });
    }
}

/// The allocation id a pod carries, if it is a valid one.
pub fn allocation_of(pod: &Pod) -> Option<String> {
    pod.metadata
        .labels
        .as_ref()?
        .get(ALLOCATION_LABEL)
        .filter(|v| valid_allocation_id(v))
        .cloned()
}

/// The immutable Secret holding the session's key, owned by `owner` (the pod
/// as the API server returned it) so it goes with the pod.
pub fn session_cache_secret(
    cfg: &PodConfig,
    id: Uuid,
    alloc: &CacheAllocation,
    owner: &Pod,
) -> Secret {
    Secret {
        metadata: ObjectMeta {
            name: Some(session_cache_secret_name(&alloc.allocation_id)),
            namespace: Some(cfg.namespace.clone()),
            labels: Some(BTreeMap::from([
                ("vogt.session".into(), id.to_string()),
                ("vogt.kind".into(), "session-cache".into()),
                (ENGINE_LABEL.into(), cfg.engine_id.clone()),
                (ALLOCATION_LABEL.into(), alloc.allocation_id.clone()),
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
        data: Some(BTreeMap::from([
            (
                SESSION_ACCESS_KEY_ID.to_string(),
                ByteString(alloc.access_key_id.clone().into_bytes()),
            ),
            (
                SESSION_SECRET_KEY.to_string(),
                ByteString(alloc.secret_access_key.clone().into_bytes()),
            ),
            (
                SESSION_BUCKET.to_string(),
                ByteString(alloc.bucket_name.clone().into_bytes()),
            ),
        ])),
        ..Default::default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub const ALLOC_ID: &str = "abcdefghijklmnopqrstuvwxyz";

    pub fn allocation(id: &str) -> CacheAllocation {
        CacheAllocation {
            allocation_id: id.into(),
            bucket_name: format!("vogt-session-{id}"),
            access_key_id: "GKsessionkey".into(),
            secret_access_key: "s3ssion-secret".into(),
        }
    }

    /// Records every call; `allocate` answers from `replies` in turn.
    #[derive(Default)]
    pub struct FakeProvisioner {
        pub replies: Mutex<Vec<Result<Option<CacheAllocation>>>>,
        pub released: Mutex<Vec<String>>,
        pub allocations: Mutex<usize>,
    }
    impl FakeProvisioner {
        pub fn answering(reply: Result<Option<CacheAllocation>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(vec![reply]),
                ..Default::default()
            })
        }
    }
    impl CacheProvisioner for FakeProvisioner {
        fn allocate(&self) -> AllocateFuture<'_> {
            *self.allocations.lock().unwrap() += 1;
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| Ok(Some(allocation(ALLOC_ID))));
            Box::pin(async move { reply })
        }
        fn release<'a>(&'a self, allocation_id: &'a str) -> ReleaseFuture<'a> {
            self.released.lock().unwrap().push(allocation_id.into());
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn allocation_ids_must_be_short_lowercase_tokens() {
        assert!(valid_allocation_id(ALLOC_ID));
        assert!(valid_allocation_id("mfrggzdfmztwq2lknnwg23tpob"));
        for bad in [
            "",
            "short",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "abcdefghijklmnop/../x",
            "abcdefghijklmnop-q",
        ] {
            assert!(!valid_allocation_id(bad), "{bad}");
        }
        assert!(!valid_allocation_id(&"a".repeat(53)));
    }

    #[test]
    fn a_reply_for_another_bucket_or_an_empty_key_is_refused() {
        let mut other = allocation(ALLOC_ID);
        other.bucket_name = "kache".into();
        assert!(check(other).is_err());
        let mut empty = allocation(ALLOC_ID);
        empty.secret_access_key.clear();
        assert!(check(empty).is_err());
        let mut bad = allocation("UPPER");
        bad.bucket_name = "vogt-session-UPPER".into();
        assert!(check(bad).is_err());
        assert!(check(allocation(ALLOC_ID)).is_ok());
    }

    #[test]
    fn debug_never_prints_the_key() {
        let printed = format!("{:?}", allocation(ALLOC_ID));
        assert!(!printed.contains("s3ssion-secret"), "{printed}");
        assert!(!printed.contains("GKsessionkey"), "{printed}");
    }

    type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

    /// A one-route provisioner on loopback: records (method, path, auth,
    /// body) and answers with `status` and `reply`.
    async fn serve(status: u16, reply: serde_json::Value) -> (String, Seen) {
        use axum::{body::Bytes, http::HeaderMap, routing::any, Router};
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let app = Router::new().fallback(any(
            move |method: axum::http::Method,
                  uri: axum::http::Uri,
                  headers: HeaderMap,
                  body: Bytes| {
                let (log, reply) = (log.clone(), reply.clone());
                async move {
                    log.lock().unwrap().push((
                        method.to_string(),
                        uri.path().to_string(),
                        headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string(),
                        String::from_utf8_lossy(&body).into_owned(),
                    ));
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        axum::Json(reply),
                    )
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    fn reply(id: &str) -> serde_json::Value {
        serde_json::json!({
            "allocation_id": id,
            "bucket_id": "b1",
            "bucket_name": format!("vogt-session-{id}"),
            "access_key_id": "GKsessionkey",
            "secret_access_key": "s3ssion-secret",
            "expires_at": "2026-11-07T00:00:00Z",
        })
    }

    #[tokio::test]
    async fn allocate_posts_no_identifier_and_sends_the_bearer() {
        let (url, seen) = serve(201, reply(ALLOC_ID)).await;
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        std::fs::write(&token, "prov-token\n").unwrap();
        let prov = HttpCacheProvisioner::new(&format!("{url}/"), Some(token)).unwrap();
        let alloc = prov.allocate().await.unwrap().unwrap();
        assert_eq!(alloc, allocation(ALLOC_ID));
        let seen = seen.lock().unwrap();
        let (method, path, auth, body) = &seen[0];
        assert_eq!(
            (method.as_str(), path.as_str()),
            ("POST", "/internal/cache/sessions")
        );
        assert_eq!(auth, "Bearer prov-token");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["expires_at"], "the request names nothing: {body}");
    }

    #[tokio::test]
    async fn at_the_cap_the_session_runs_without_a_bucket_and_other_refusals_fail() {
        let (url, _) = serve(429, serde_json::json!({})).await;
        let prov = HttpCacheProvisioner::new(&url, None).unwrap();
        assert_eq!(prov.allocate().await.unwrap(), None);
        for status in [401, 403, 409, 500, 503] {
            let (url, _) = serve(status, reply(ALLOC_ID)).await;
            let prov = HttpCacheProvisioner::new(&url, None).unwrap();
            let err = prov.allocate().await.expect_err("refusal must fail closed");
            assert!(!err.to_string().contains("s3ssion"), "{err}");
        }
        // A 201 whose bucket is not the allocation's own is refused too.
        let mut wrong = reply(ALLOC_ID);
        wrong["bucket_name"] = "kache".into();
        let (url, _) = serve(201, wrong).await;
        let prov = HttpCacheProvisioner::new(&url, None).unwrap();
        assert!(prov.allocate().await.is_err());
    }

    #[tokio::test]
    async fn release_deletes_by_id_and_unknown_is_done() {
        let (url, seen) = serve(404, serde_json::json!({})).await;
        let prov = HttpCacheProvisioner::new(&url, None).unwrap();
        prov.release(ALLOC_ID).await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].1,
            format!("/internal/cache/sessions/{ALLOC_ID}")
        );
        assert!(prov.release("../../kache").await.is_err());
        assert_eq!(seen.lock().unwrap().len(), 1, "a bad id reached the wire");
    }

    #[test]
    fn the_secret_is_owned_immutable_and_keyed_as_designed() {
        let cfg = PodConfig::default();
        let id = Uuid::new_v4();
        let mut owner = Pod::default();
        owner.metadata.name = Some(pod_name(id));
        owner.metadata.uid = Some("uid-pod".into());
        let s = session_cache_secret(&cfg, id, &allocation(ALLOC_ID), &owner);
        assert_eq!(
            s.metadata.name.as_deref(),
            Some(format!("vogt-session-cache-{ALLOC_ID}").as_str())
        );
        assert_eq!(s.immutable, Some(true));
        assert_eq!(
            s.metadata.owner_references.as_ref().unwrap()[0].uid,
            "uid-pod"
        );
        let data = s.data.unwrap();
        assert_eq!(
            data[SESSION_BUCKET].0,
            format!("vogt-session-{ALLOC_ID}").as_bytes()
        );
        assert_eq!(data[SESSION_SECRET_KEY].0, b"s3ssion-secret");
        assert_eq!(data.len(), 3);
    }
}
