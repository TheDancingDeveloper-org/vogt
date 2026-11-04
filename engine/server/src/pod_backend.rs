//! Kubernetes lifecycle support for opted-in session templates.
use crate::{
    config::SessionTemplate,
    error::{ApiError, Result},
    session_backend::SessionBackend,
};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMapEnvSource, ConfigMapVolumeSource, Container, EnvFromSource, EnvVar,
    PersistentVolumeClaimVolumeSource, Pod, PodSecurityContext, PodSpec, ResourceRequirements,
    SeccompProfile, Secret, SecretEnvSource, SecretVolumeSource, SecurityContext, Volume,
    VolumeMount,
};
use k8s_openapi::{apimachinery::pkg::apis::meta::v1::OwnerReference, ByteString};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{
    api::{Api, DeleteParams, ListParams, ObjectMeta, PostParams, Preconditions},
    Client, Config as KubeConfig,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PodConfig {
    pub namespace: String,
    pub kubeconfig: Option<std::path::PathBuf>,
    pub engine_endpoint: String,
    pub default_image: String,
    pub connect_timeout: Duration,
    /// Default resource policy; values are copied verbatim into the pod.
    pub resource_requests:
        BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>,
    pub resource_limits: BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>,
    pub node_selector: BTreeMap<String, String>,
    /// The cache claim and node are deployment policy rather than pod logic.
    pub cache_pvc: String,
    pub cache_node: String,
    pub cache_env_config_map: String,
    pub cache_secret: String,
    pub kache_config_map: String,
    pub graft_repo: String,
    pub graft_line: String,
    /// Graft server the entrypoint clones from, passed to the pod as
    /// `GRAFT_ENDPOINT`. It must be reachable from the session namespace.
    #[serde(default)]
    pub graft_endpoint: Option<String>,
    /// File (a mounted secret) holding the graft token scoped to the session's
    /// repo and line. The engine reads it at bootstrap and hands it to the
    /// entrypoint; the value is never in config or the pod spec.
    #[serde(default)]
    pub graft_token_file: Option<String>,
    pub cli: String,
    pub extra_ca_file: Option<String>,
    pub priority_class: Option<String>,
    pub service_account: String,
    /// Secret holding registry credentials for the session image, if private.
    #[serde(default)]
    pub image_pull_secret: Option<String>,
    /// How often the reconciler compares the cluster with the engine's sessions.
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval: Duration,
    /// How long a pod may sit Pending (unschedulable, pulling) before its
    /// session is ended.
    #[serde(default = "default_pending_timeout")]
    pub pending_timeout: Duration,
    /// How long an adopted pod's agent has to dial in again after an engine
    /// restart before the pod is given up as lost.
    #[serde(default = "default_reconnect_timeout")]
    pub reconnect_timeout: Duration,
    /// Names this engine deployment. Every pod it creates carries it as the
    /// `vogt.engine` label, and the reconciler lists and deletes only pods
    /// that carry its own, so engines sharing a namespace (dev and prod, or an
    /// old process during a rollout) never touch each other's pods. Keep it
    /// stable for a deployment and distinct between deployments.
    #[serde(default = "default_engine_id")]
    pub engine_id: String,
}
fn default_reconnect_timeout() -> Duration {
    Duration::from_secs(120)
}
/// The label that marks which engine deployment owns a session pod.
pub const ENGINE_LABEL: &str = "vogt.engine";
pub const DEFAULT_ENGINE_ID: &str = "vogt-engine";
fn default_engine_id() -> String {
    DEFAULT_ENGINE_ID.into()
}
/// Make an operator-supplied engine id a valid label value (at most 63
/// characters of `[A-Za-z0-9._-]`, starting and ending alphanumeric); an id
/// that leaves nothing falls back to the default.
pub fn sanitize_engine_id(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(63)
        .collect();
    let kept = kept.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if kept.is_empty() {
        default_engine_id()
    } else {
        kept.to_string()
    }
}
/// The label selector for the pods this engine owns.
pub fn session_pod_selector(cfg: &PodConfig) -> String {
    format!("vogt.session,{ENGINE_LABEL}={}", cfg.engine_id)
}
/// Whether the pod carries this engine's label.
pub fn pod_is_ours(pod: &Pod, cfg: &PodConfig) -> bool {
    pod.metadata
        .labels
        .as_ref()
        .and_then(|l| l.get(ENGINE_LABEL))
        .is_some_and(|v| v == &cfg.engine_id)
}
fn default_reconcile_interval() -> Duration {
    Duration::from_secs(15)
}
fn default_pending_timeout() -> Duration {
    Duration::from_secs(600)
}
impl Default for PodConfig {
    fn default() -> Self {
        Self {
            namespace: "vogt-sessions".into(),
            kubeconfig: None,
            engine_endpoint: "http://192.168.1.75:18197".into(),
            default_image: "ghcr.io/thedancingdeveloper-org/vogt-session:latest".into(),
            connect_timeout: Duration::from_secs(30),
            resource_requests: BTreeMap::from([
                (
                    "cpu".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("100m".into()),
                ),
                (
                    "memory".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("256Mi".into()),
                ),
                (
                    "ephemeral-storage".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("10Gi".into()),
                ),
            ]),
            resource_limits: BTreeMap::from([
                (
                    "cpu".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("1".into()),
                ),
                (
                    "memory".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("1Gi".into()),
                ),
                (
                    "ephemeral-storage".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("40Gi".into()),
                ),
            ]),
            node_selector: BTreeMap::new(),
            cache_pvc: "session-cache-winrarhost1".into(),
            cache_node: "winrarhost1".into(),
            cache_env_config_map: "session-cache-env".into(),
            cache_secret: "kache-s3".into(),
            kache_config_map: "kache-config".into(),
            graft_repo: "vogt".into(),
            graft_line: "rust-2".into(),
            cli: "codex".into(),
            graft_endpoint: None,
            graft_token_file: None,
            extra_ca_file: None,
            priority_class: Some("vogt-session".into()),
            service_account: "vogt-session".into(),
            image_pull_secret: None,
            reconcile_interval: default_reconcile_interval(),
            pending_timeout: default_pending_timeout(),
            reconnect_timeout: default_reconnect_timeout(),
            engine_id: default_engine_id(),
        }
    }
}
impl PodConfig {
    /// Defaults overridden by `ENGINE_POD_*` settings, read through `get` so
    /// tests need not touch the process environment. An empty value for the
    /// priority class, a cache name, the pull secret or a resource list turns
    /// that item off. `endpoint` is the engine's own `agent_auth_public_url`,
    /// used when `ENGINE_POD_ENGINE_ENDPOINT` is unset.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        endpoint: Option<String>,
    ) -> Result<Self> {
        let mut cfg = Self::default();
        // `VOGT_ENGINE_POD_*` is accepted as an alias (the smoke harness sets
        // it); `ENGINE_POD_*` wins when both are present.
        let text = |name: &str| {
            get(name)
                .or_else(|| get(&format!("VOGT_{name}")))
                .map(|v| v.trim().to_string())
        };
        let set = |name: &str, field: &mut String| {
            if let Some(v) = text(name).filter(|v| !v.is_empty()) {
                *field = v;
            }
        };
        // Cache names may be emptied: a namespace without the cache objects
        // (the smoke cluster) runs sessions with no cache.
        let set_or_empty = |name: &str, field: &mut String| {
            if let Some(v) = text(name) {
                *field = v;
            }
        };
        let optional = |name: &str, field: &mut Option<String>| {
            if let Some(v) = text(name) {
                *field = (!v.is_empty()).then_some(v);
            }
        };
        set("ENGINE_POD_NAMESPACE", &mut cfg.namespace);
        set("ENGINE_POD_IMAGE", &mut cfg.default_image);
        set("ENGINE_POD_SERVICE_ACCOUNT", &mut cfg.service_account);
        set("ENGINE_POD_GRAFT_REPO", &mut cfg.graft_repo);
        set("ENGINE_POD_GRAFT_LINE", &mut cfg.graft_line);
        set("ENGINE_POD_CLI", &mut cfg.cli);
        set_or_empty("ENGINE_POD_CACHE_PVC", &mut cfg.cache_pvc);
        set_or_empty("ENGINE_POD_CACHE_NODE", &mut cfg.cache_node);
        set_or_empty(
            "ENGINE_POD_CACHE_ENV_CONFIG_MAP",
            &mut cfg.cache_env_config_map,
        );
        set_or_empty("ENGINE_POD_CACHE_SECRET", &mut cfg.cache_secret);
        set_or_empty("ENGINE_POD_KACHE_CONFIG_MAP", &mut cfg.kache_config_map);
        optional("ENGINE_POD_PRIORITY_CLASS", &mut cfg.priority_class);
        optional("ENGINE_POD_IMAGE_PULL_SECRET", &mut cfg.image_pull_secret);
        optional("ENGINE_POD_GRAFT_ENDPOINT", &mut cfg.graft_endpoint);
        optional("ENGINE_POD_GRAFT_TOKEN_FILE", &mut cfg.graft_token_file);
        optional("ENGINE_POD_EXTRA_CA_FILE", &mut cfg.extra_ca_file);
        if let Some(v) = text("ENGINE_POD_RESOURCE_REQUESTS") {
            cfg.resource_requests = parse_resources("ENGINE_POD_RESOURCE_REQUESTS", &v)?;
        }
        if let Some(v) = text("ENGINE_POD_RESOURCE_LIMITS") {
            cfg.resource_limits = parse_resources("ENGINE_POD_RESOURCE_LIMITS", &v)?;
        }
        if let Some(v) = text("ENGINE_POD_ENGINE_ID") {
            cfg.engine_id = sanitize_engine_id(&v);
        }
        let insecure = match text("ENGINE_POD_ALLOW_INSECURE_ENDPOINT").as_deref() {
            None | Some("") | Some("false") | Some("0") => false,
            Some("true") | Some("1") => true,
            Some(other) => {
                return Err(ApiError::Config(format!(
                    "ENGINE_POD_ALLOW_INSECURE_ENDPOINT must be true or false, got {other:?}"
                )))
            }
        };
        match text("ENGINE_POD_ENGINE_ENDPOINT").filter(|v| !v.is_empty()) {
            Some(url) => {
                let url = url.trim_end_matches('/').to_string();
                let ok = url.starts_with("https://") || (insecure && url.starts_with("http://"));
                if !ok {
                    return Err(ApiError::Config(
                        "ENGINE_POD_ENGINE_ENDPOINT must be an https URL \
                         (plain http needs ENGINE_POD_ALLOW_INSECURE_ENDPOINT=true)"
                            .into(),
                    ));
                }
                cfg.engine_endpoint = url;
            }
            None => {
                if let Some(url) = endpoint {
                    cfg.engine_endpoint = url;
                }
            }
        }
        Ok(cfg)
    }

    /// `from_lookup` over the process environment.
    pub fn from_env(endpoint: Option<String>) -> Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok(), endpoint)
    }

    pub async fn client(&self) -> Result<Client> {
        let config = match self.kubeconfig.as_ref() {
            Some(path) => {
                let kubeconfig = Kubeconfig::read_from(path)
                    .map_err(|e| ApiError::Config(format!("reading kubeconfig: {e}")))?;
                KubeConfig::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
                    .await
                    .map_err(|e| ApiError::Config(format!("kubernetes config: {e}")))
            }
            None => KubeConfig::infer()
                .await
                .map_err(|e| ApiError::Config(format!("kubernetes config: {e}"))),
        }?;
        Client::try_from(config).map_err(|e| ApiError::Config(format!("kubernetes client: {e}")))
    }
}
/// `cpu=100m,memory=256Mi` into a resource map; empty means none.
fn parse_resources(
    name: &str,
    value: &str,
) -> Result<BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>> {
    let mut out = BTreeMap::new();
    for pair in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (k, v) = pair
            .split_once('=')
            .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
            .ok_or_else(|| {
                ApiError::Config(format!("{name}: expected name=quantity, got {pair:?}"))
            })?;
        out.insert(
            k.trim().to_string(),
            k8s_openapi::apimachinery::pkg::api::resource::Quantity(v.trim().to_string()),
        );
    }
    Ok(out)
}
pub fn pod_name(id: Uuid) -> String {
    format!("vogt-session-{}", id.simple())
}
/// The Secret that carries one pod's single-use bootstrap credential. The
/// name has a random part, fresh for every pod the engine creates, so it can
/// never collide with a Secret an earlier pod of the session left behind and
/// the engine never has to delete a Secret (its Role grants `create` only).
pub fn bootstrap_secret_name(id: Uuid) -> String {
    let nonce = Uuid::new_v4().simple().to_string();
    format!("{}-{}-bootstrap", pod_name(id), &nonce[..8])
}
const BOOTSTRAP_VOLUME: &str = "bootstrap";
/// The Secret name a pod's bootstrap volume refers to.
fn bootstrap_secret_name_in(pod: &Pod) -> Option<String> {
    pod.spec
        .as_ref()?
        .volumes
        .as_ref()?
        .iter()
        .find(|v| v.name == BOOTSTRAP_VOLUME)?
        .secret
        .as_ref()?
        .secret_name
        .clone()
}
/// Directory the bootstrap Secret is mounted at, and the file inside it.
const BOOTSTRAP_MOUNT_DIR: &str = "/run/vogt-bootstrap";
const BOOTSTRAP_TOKEN_KEY: &str = "token";
const BOOTSTRAP_TOKEN_PATH: &str = "/run/vogt-bootstrap/token";

/// The Secret holding a pod's bootstrap credential. `owner` is the pod as the
/// API server returned it (it has a uid); the owner reference makes the
/// garbage collector delete the Secret when the pod goes. It is immutable and
/// carries the session and engine labels so an orphan can be found.
pub fn bootstrap_secret(
    cfg: &PodConfig,
    id: Uuid,
    name: &str,
    token: &[u8],
    owner: &Pod,
) -> Secret {
    Secret {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(cfg.namespace.clone()),
            labels: Some(BTreeMap::from([
                ("vogt.session".into(), id.to_string()),
                ("vogt.kind".into(), "bootstrap".into()),
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
            BOOTSTRAP_TOKEN_KEY.to_string(),
            ByteString(token.to_vec()),
        )])),
        ..Default::default()
    }
}

fn pod_env(cfg: &PodConfig, id: Uuid) -> Vec<EnvVar> {
    let mut env = vec![
        EnvVar {
            name: "VOGT_ENGINE_URL".into(),
            value: Some(cfg.engine_endpoint.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "VOGT_SESSION_ID".into(),
            value: Some(id.to_string()),
            ..Default::default()
        },
        // The path only: the credential itself is in the per-session Secret.
        EnvVar {
            name: "VOGT_BOOTSTRAP_TOKEN_FILE".into(),
            value: Some(BOOTSTRAP_TOKEN_PATH.into()),
            ..Default::default()
        },
        EnvVar {
            name: "VOGT_GRAFT_REPO".into(),
            value: Some(cfg.graft_repo.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "VOGT_GRAFT_LINE".into(),
            value: Some(cfg.graft_line.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "VOGT_CLI".into(),
            value: Some(cfg.cli.clone()),
            ..Default::default()
        },
    ];
    if let Some(endpoint) = &cfg.graft_endpoint {
        env.push(EnvVar {
            name: "GRAFT_ENDPOINT".into(),
            value: Some(endpoint.clone()),
            ..Default::default()
        });
    }
    if let Some(path) = &cfg.extra_ca_file {
        env.push(EnvVar {
            name: "VOGT_EXTRA_CA_FILE".into(),
            value: Some(path.clone()),
            ..Default::default()
        });
    }
    env
}
/// Convert Kubernetes state into the engine's terminal vocabulary. A missing
/// phase is deliberately pending: the API may return a pod before the
/// scheduler has assigned it.
/// Human-readable reason retained when a pod disappears or fails. Prefer the
/// controller reason/message, then the first terminated container message.
pub fn pod_reason(pod: &Pod) -> Option<String> {
    let status = pod.status.as_ref()?;
    if let Some(message) = status.message.as_deref().filter(|m| !m.trim().is_empty()) {
        return Some(message.to_string());
    }
    if let Some(reason) = status.reason.as_deref().filter(|r| !r.trim().is_empty()) {
        return Some(reason.to_string());
    }
    status
        .container_statuses
        .as_ref()?
        .iter()
        .filter_map(|container| container.state.as_ref())
        .filter_map(|state| state.terminated.as_ref())
        .find_map(|terminated| {
            terminated
                .message
                .clone()
                .or_else(|| terminated.reason.clone())
        })
}

pub fn pod_phase(pod: &Pod) -> &'static str {
    match pod.status.as_ref().and_then(|s| s.phase.as_deref()) {
        Some("Succeeded") => "exited",
        Some("Failed") => "errored",
        Some("Unknown") => "errored",
        Some("Running") => "running",
        Some("Pending") | None => "pending",
        Some(_) => "pending",
    }
}

/// The exit code the first terminated container reported, if any.
pub fn pod_exit_code(pod: &Pod) -> Option<i32> {
    pod.status
        .as_ref()?
        .container_statuses
        .as_ref()?
        .iter()
        .filter_map(|c| c.state.as_ref()?.terminated.as_ref())
        .map(|t| t.exit_code)
        .next()
}

/// Why a container is stuck waiting, when the kubelet gave up on it (a bad
/// image or config does not heal by waiting).
pub fn pod_fatal_waiting_reason(pod: &Pod) -> Option<String> {
    const FATAL: [&str; 5] = [
        "ErrImagePull",
        "ImagePullBackOff",
        "InvalidImageName",
        "CreateContainerConfigError",
        "CreateContainerError",
    ];
    pod.status
        .as_ref()?
        .container_statuses
        .as_ref()?
        .iter()
        .filter_map(|c| c.state.as_ref()?.waiting.as_ref())
        .find_map(|w| {
            let reason = w.reason.as_deref()?;
            FATAL.contains(&reason).then(|| match w.message.as_deref() {
                Some(m) if !m.trim().is_empty() => format!("{reason}: {m}"),
                _ => reason.to_string(),
            })
        })
}

/// Extract a session id only from the engine-owned label, never from a pod
/// name supplied by an external caller.
pub fn session_id_from_pod(pod: &Pod) -> Option<Uuid> {
    pod.metadata
        .labels
        .as_ref()?
        .get("vogt.session")?
        .parse()
        .ok()
}

#[allow(clippy::too_many_arguments)]
pub fn pod_template(
    cfg: &PodConfig,
    template: &SessionTemplate,
    id: Uuid,
    project: &str,
    kind: &str,
    account: Option<&str>,
    command: &[String],
) -> Pod {
    let mut labels = BTreeMap::from([
        ("vogt.session".into(), id.to_string()),
        ("vogt.kind".into(), kind.into()),
        ("vogt.project".into(), project.into()),
        (ENGINE_LABEL.into(), cfg.engine_id.clone()),
    ]);
    if let Some(account) = account {
        labels.insert("vogt.account".into(), account.into());
    }
    let image = template
        .env
        .iter()
        .find_map(|(k, v)| (k == "VOGT_SESSION_IMAGE").then_some(v.as_str()))
        .unwrap_or(&cfg.default_image);
    let mut env = pod_env(cfg, id);
    // The requested argv runs in the PTY (the session agent reads it); the
    // container command stays the image entrypoint.
    if !command.is_empty() {
        env.push(EnvVar {
            name: "VOGT_SESSION_COMMAND".into(),
            value: Some(serde_json::to_string(command).expect("argv serialises")),
            ..Default::default()
        });
    }
    // Each cache object is optional: an empty name leaves it out.
    let mut env_from = Vec::new();
    if !cfg.cache_env_config_map.is_empty() {
        env_from.push(EnvFromSource {
            config_map_ref: Some(ConfigMapEnvSource {
                name: cfg.cache_env_config_map.clone(),
                optional: Some(false),
            }),
            ..Default::default()
        });
    }
    if !cfg.cache_secret.is_empty() {
        env_from.push(EnvFromSource {
            secret_ref: Some(SecretEnvSource {
                name: cfg.cache_secret.clone(),
                optional: Some(false),
            }),
            ..Default::default()
        });
    }
    let mut mounts = Vec::new();
    let mut volumes = Vec::new();
    if !cfg.cache_pvc.is_empty() {
        mounts.push(VolumeMount {
            name: "session-cache".into(),
            mount_path: "/cache".into(),
            ..Default::default()
        });
        volumes.push(Volume {
            name: "session-cache".into(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: cfg.cache_pvc.clone(),
                read_only: Some(false),
            }),
            ..Default::default()
        });
    }
    if !cfg.kache_config_map.is_empty() {
        mounts.push(VolumeMount {
            name: "kache-config".into(),
            mount_path: "/etc/kache".into(),
            ..Default::default()
        });
        volumes.push(Volume {
            name: "kache-config".into(),
            config_map: Some(ConfigMapVolumeSource {
                name: cfg.kache_config_map.clone(),
                ..Default::default()
            }),
            ..Default::default()
        });
    }
    // The bootstrap credential is a file, never a pod-spec value. The Secret
    // it names is created right after the pod (it needs the pod's uid to be
    // owned by it); the kubelet holds the container until the volume resolves.
    mounts.push(VolumeMount {
        name: BOOTSTRAP_VOLUME.into(),
        mount_path: BOOTSTRAP_MOUNT_DIR.into(),
        read_only: Some(true),
        ..Default::default()
    });
    volumes.push(Volume {
        name: BOOTSTRAP_VOLUME.into(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(bootstrap_secret_name(id)),
            default_mode: Some(0o400),
            optional: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    });
    Pod {
        metadata: ObjectMeta {
            name: Some(pod_name(id)),
            namespace: Some(cfg.namespace.clone()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(PodSpec {
            containers: vec![Container {
                name: "session-agent".into(),
                image: Some(image.into()),
                env: Some(env),
                env_from: Some(env_from),
                volume_mounts: Some(mounts),
                resources: Some(ResourceRequirements {
                    requests: (!cfg.resource_requests.is_empty())
                        .then(|| cfg.resource_requests.clone()),
                    limits: (!cfg.resource_limits.is_empty()).then(|| cfg.resource_limits.clone()),
                    claims: None,
                }),
                security_context: Some(SecurityContext {
                    allow_privilege_escalation: Some(false),
                    capabilities: Some(Capabilities {
                        drop: Some(vec!["ALL".into()]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            service_account_name: Some(cfg.service_account.clone()),
            automount_service_account_token: Some(false),
            security_context: Some(PodSecurityContext {
                run_as_non_root: Some(true),
                run_as_user: Some(1000),
                run_as_group: Some(1000),
                fs_group: Some(1000),
                seccomp_profile: Some(SeccompProfile {
                    type_: "RuntimeDefault".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            node_selector: (!cfg.node_selector.is_empty()).then(|| cfg.node_selector.clone()),
            node_name: (!cfg.cache_node.is_empty()).then(|| cfg.cache_node.clone()),
            image_pull_secrets: cfg.image_pull_secret.as_ref().map(|name| {
                vec![k8s_openapi::api::core::v1::LocalObjectReference { name: name.clone() }]
            }),
            volumes: Some(volumes),
            priority_class_name: cfg.priority_class.clone(),
            restart_policy: Some("Never".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}
#[derive(Clone)]
pub struct PodBackendFactory {
    pub cfg: Arc<PodConfig>,
    pub client: Client,
}
impl PodBackendFactory {
    pub async fn new(cfg: PodConfig) -> Result<Self> {
        let client = cfg.client().await?;
        Ok(Self {
            cfg: Arc::new(cfg),
            client,
        })
    }
    pub async fn create(&self, pod: Pod) -> Result<Pod> {
        Api::namespaced(self.client.clone(), &self.cfg.namespace)
            .create(&PostParams::default(), &pod)
            .await
            .map_err(|e| ApiError::Internal(format!("creating session pod: {e}")))
    }

    /// Create the session's pod, then the Secret that carries its bootstrap
    /// credential, owned by that pod so the cluster deletes it with the pod.
    /// The pod spec holds only the Secret's (unique) name; the pod waits in
    /// `ContainerCreating` until the Secret exists. If the Secret cannot be
    /// made the pod is removed, so none is left that can never boot. The
    /// engine never deletes a Secret: the garbage collector does.
    pub async fn create_resources(&self, id: Uuid, token: &[u8], pod: Pod) -> Result<Pod> {
        let name = bootstrap_secret_name_in(&pod).ok_or_else(|| {
            ApiError::BadRequest("session pod has no bootstrap secret volume".into())
        })?;
        crate::agent_stream::global().issue(id, token).await;
        let created = self.create(pod).await?;
        let secret = bootstrap_secret(&self.cfg, id, &name, token, &created);
        let api = Api::<Secret>::namespaced(self.client.clone(), &self.cfg.namespace);
        if let Err(e) = api.create(&PostParams::default(), &secret).await {
            let _ = self.delete(id, created.metadata.uid.clone()).await;
            return Err(ApiError::Internal(format!(
                "creating bootstrap secret: {e}"
            )));
        }
        Ok(created)
    }

    /// Delete the session's pod. With `uid` the API server refuses (409) when
    /// the pod now under that name is a different one, so a decision made
    /// about an earlier pod cannot remove its replacement.
    pub async fn delete(&self, id: Uuid, uid: Option<String>) -> Result<()> {
        let params = DeleteParams {
            preconditions: uid.map(|uid| Preconditions {
                uid: Some(uid),
                resource_version: None,
            }),
            ..Default::default()
        };
        Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .delete(&pod_name(id), &params)
            .await
            .map_err(|e| ApiError::Internal(format!("deleting session pod: {e}")))?;
        Ok(())
    }
    pub async fn reconcile(&self) -> Result<Vec<Pod>> {
        let pods = Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .list(&ListParams::default().labels(&session_pod_selector(&self.cfg)))
            .await
            .map_err(|e| ApiError::Internal(format!("listing session pods: {e}")))?;
        Ok(pods.items)
    }
}

/// Validate the first agent frame before associating a socket with a session.
/// Authentication remains separate from this parser: the HTTP bearer token is
/// checked by the route, while this prevents a valid token being replayed for
/// another session id.
pub fn validate_agent_hello(frame: &vogt_session_agent::Frame, expected: Uuid) -> Result<()> {
    match frame {
        vogt_session_agent::Frame::Hello { session_id } if session_id == &expected.to_string() => {
            Ok(())
        }
        vogt_session_agent::Frame::Hello { .. } => Err(ApiError::Unauthorized),
        _ => Err(ApiError::BadRequest(
            "agent stream must begin with hello".into(),
        )),
    }
}

/// Resume cursors are absolute byte offsets. Rejecting a cursor beyond the
/// observed stream avoids manufacturing output gaps after reconnect.
pub fn validate_resume_cursor(cursor: u64, next_offset: u64) -> Result<u64> {
    if cursor > next_offset {
        return Err(ApiError::BadRequest(
            "agent resume cursor is beyond the output stream".into(),
        ));
    }
    Ok(cursor)
}

/// Creates and removes the pod behind a session. The Kubernetes
/// implementation is [`PodBackendFactory`]; tests substitute a fake that runs
/// the agent in-process.
pub trait PodLauncher: Send + Sync {
    /// Create the pod. `token` is the one-shot bootstrap credential already
    /// registered with the stream; implementations only deliver it to the pod.
    fn create<'a>(&'a self, id: Uuid, token: &'a [u8], pod: Pod) -> LaunchFuture<'a>;
    /// Delete the session's pod; `uid` (from the listed pod) makes the delete
    /// apply only to that exact pod.
    fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_>;
    /// Every pod in the sessions namespace that carries a session label and
    /// this engine's `vogt.engine` label.
    fn list(&self) -> ListFuture<'_>;
}
pub type ListFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Pod>>> + Send + 'a>>;
pub type LaunchFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

impl PodLauncher for PodBackendFactory {
    fn create<'a>(&'a self, id: Uuid, token: &'a [u8], pod: Pod) -> LaunchFuture<'a> {
        Box::pin(async move { self.create_resources(id, token, pod).await.map(|_| ()) })
    }
    fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_> {
        Box::pin(PodBackendFactory::delete(self, id, uid))
    }
    fn list(&self) -> ListFuture<'_> {
        Box::pin(self.reconcile())
    }
}

/// The production launcher: connects to the cluster on first use, so an
/// engine with the flag on but no cluster still starts and local sessions
/// are unaffected.
pub struct KubeLauncher {
    cfg: PodConfig,
    factory: tokio::sync::OnceCell<PodBackendFactory>,
}
impl KubeLauncher {
    pub fn new(cfg: PodConfig) -> Self {
        Self {
            cfg,
            factory: tokio::sync::OnceCell::new(),
        }
    }
    pub fn config(&self) -> &PodConfig {
        &self.cfg
    }
    async fn factory(&self) -> Result<&PodBackendFactory> {
        self.factory
            .get_or_try_init(|| PodBackendFactory::new(self.cfg.clone()))
            .await
    }
}
impl PodLauncher for KubeLauncher {
    fn create<'a>(&'a self, id: Uuid, token: &'a [u8], pod: Pod) -> LaunchFuture<'a> {
        Box::pin(async move { PodLauncher::create(self.factory().await?, id, token, pod).await })
    }
    fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_> {
        Box::pin(async move { PodLauncher::delete(self.factory().await?, id, uid).await })
    }
    fn list(&self) -> ListFuture<'_> {
        Box::pin(async move { PodLauncher::list(self.factory().await?).await })
    }
}

pub struct PodBackend {
    id: Uuid,
    link: std::sync::Arc<crate::agent_stream::PodLink>,
    size: parking_lot::Mutex<(u16, u16)>,
}
impl PodBackend {
    pub fn new(
        id: Uuid,
        link: std::sync::Arc<crate::agent_stream::PodLink>,
        size: (u16, u16),
    ) -> Self {
        Self {
            id,
            link,
            size: parking_lot::Mutex::new(size),
        }
    }
}
impl SessionBackend for PodBackend {
    fn write_input(&self, data: &[u8]) -> std::io::Result<()> {
        crate::agent_stream::global().send(
            self.id,
            vogt_session_agent::Frame::Input {
                data: data.to_vec(),
            },
        )
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        *self.size.lock() = (cols, rows);
        crate::agent_stream::global()
            .send(self.id, vogt_session_agent::Frame::Resize { cols, rows })
            .map_err(|e| ApiError::Pty(format!("resize: {e}")))?;
        Ok(())
    }
    fn signal_stop(&self) -> Result<()> {
        self.link.request_stop();
        match crate::agent_stream::global().send(
            self.id,
            vogt_session_agent::Frame::Signal {
                signal: "kill".into(),
            },
        ) {
            Ok(()) => Ok(()),
            // No agent is attached, so there is no child to wait for: the
            // session ends now, and an agent that dials in later is told to
            // kill itself.
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {
                self.link.finish(137);
                Ok(())
            }
            Err(e) => Err(ApiError::Pty(format!("stop: {e}"))),
        }
    }
    fn current_size(&self) -> Result<(u16, u16)> {
        Ok(*self.size.lock())
    }
    fn wait(&self) -> std::io::Result<portable_pty::ExitStatus> {
        Ok(portable_pty::ExitStatus::with_exit_code(
            self.link.wait_exit() as u32,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_hello_and_resume_are_bound_to_session() {
        let id = Uuid::nil();
        let hello = vogt_session_agent::Frame::Hello {
            session_id: id.to_string(),
        };
        assert!(validate_agent_hello(&hello, id).is_ok());
        assert!(validate_agent_hello(&hello, Uuid::new_v4()).is_err());
        assert_eq!(validate_resume_cursor(3, 5).unwrap(), 3);
        assert!(validate_resume_cursor(6, 5).is_err());
    }

    #[test]
    fn pod_phases_and_labels_are_safe() {
        let mut pod = Pod::default();
        assert_eq!(pod_phase(&pod), "pending");
        pod.metadata.labels = Some(BTreeMap::from([(
            "vogt.session".into(),
            Uuid::nil().to_string(),
        )]));
        assert_eq!(session_id_from_pod(&pod), Some(Uuid::nil()));
        pod.status = Some(k8s_openapi::api::core::v1::PodStatus {
            phase: Some("Failed".into()),
            ..Default::default()
        });
        assert_eq!(pod_phase(&pod), "errored");
        pod.status.as_mut().unwrap().message = Some("OOMKilled".into());
        assert_eq!(pod_reason(&pod).as_deref(), Some("OOMKilled"));
    }

    #[test]
    fn pod_spec_has_session_identity_and_bootstrap_file_and_security_defaults() {
        let cfg = PodConfig {
            engine_endpoint: "http://192.168.1.75:18197".into(),
            ..Default::default()
        };
        let template = SessionTemplate {
            name: "pod".into(),
            description: "pod".into(),
            command: None,
            cwd: None,
            env: vec![],
            default_name: None,
            match_repo_names: vec![],
            match_path_prefixes: vec![],
            tags: vec![],
            backend: "pod".into(),
            account: None,
        };
        let id = Uuid::nil();
        let pod = pod_template(
            &cfg,
            &template,
            id,
            "demo",
            "agent",
            Some("acct"),
            &["bash".into()],
        );
        let labels = pod.metadata.labels.unwrap();
        assert_eq!(labels.get("vogt.session"), Some(&id.to_string()));
        assert_eq!(labels.get("vogt.account"), Some(&"acct".into()));
        let spec = pod.spec.as_ref().unwrap();
        assert_eq!(spec.service_account_name.as_deref(), Some("vogt-session"));
        assert_eq!(spec.automount_service_account_token, Some(false));
        assert_eq!(
            spec.security_context.as_ref().unwrap().run_as_non_root,
            Some(true)
        );
        assert_eq!(
            spec.security_context.as_ref().unwrap().run_as_user,
            Some(1000)
        );
        assert_eq!(
            spec.security_context.as_ref().unwrap().run_as_group,
            Some(1000)
        );
        assert_eq!(spec.security_context.as_ref().unwrap().fs_group, Some(1000));
        assert_eq!(
            spec.security_context
                .as_ref()
                .unwrap()
                .seccomp_profile
                .as_ref()
                .unwrap()
                .type_,
            "RuntimeDefault"
        );
        let container = &pod.spec.as_ref().unwrap().containers[0];
        assert!(container.command.is_none());
        let argv = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "VOGT_SESSION_COMMAND")
            .unwrap();
        assert_eq!(argv.value.as_deref(), Some(r#"["bash"]"#));
        // No credential in the spec: only the path of the mounted file.
        let token_file = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "VOGT_BOOTSTRAP_TOKEN_FILE")
            .unwrap();
        assert_eq!(
            token_file.value.as_deref(),
            Some("/run/vogt-bootstrap/token")
        );
        let security = container.security_context.as_ref().unwrap();
        assert_eq!(security.allow_privilege_escalation, Some(false));
        assert_eq!(
            security
                .capabilities
                .as_ref()
                .unwrap()
                .drop
                .as_ref()
                .unwrap(),
            &["ALL"]
        );
        let env = container.env.as_ref().unwrap();
        for name in [
            "VOGT_ENGINE_URL",
            "VOGT_SESSION_ID",
            "VOGT_BOOTSTRAP_TOKEN_FILE",
            "VOGT_GRAFT_REPO",
            "VOGT_GRAFT_LINE",
            "VOGT_CLI",
        ] {
            assert!(
                env.iter().any(|variable| variable.name == name),
                "missing {name}"
            );
        }
        assert!(
            !env.iter().any(
                |v| v.name.contains("BOOTSTRAP_TOKEN") && v.name != "VOGT_BOOTSTRAP_TOKEN_FILE"
            ),
            "a bootstrap credential is in the pod env"
        );
        assert!(env.iter().all(|v| v.value_from.is_none()));
        let resources = container.resources.as_ref().unwrap();
        assert_eq!(
            resources.requests.as_ref().unwrap()["ephemeral-storage"].0,
            "10Gi"
        );
        assert_eq!(
            resources.limits.as_ref().unwrap()["ephemeral-storage"].0,
            "40Gi"
        );
        assert_eq!(spec.node_name.as_deref(), Some("winrarhost1"));
        let mounts = container.volume_mounts.as_ref().unwrap();
        assert_eq!(mounts[0].mount_path, "/cache");
        assert!(mounts[0].sub_path.is_none());
        assert_eq!(mounts[1].mount_path, "/etc/kache");
        let volumes = spec.volumes.as_ref().unwrap();
        assert_eq!(
            volumes[0]
                .persistent_volume_claim
                .as_ref()
                .unwrap()
                .claim_name,
            "session-cache-winrarhost1"
        );
        assert_eq!(volumes[1].config_map.as_ref().unwrap().name, "kache-config");
        // The bootstrap Secret is mounted read-only, mode 0400, and required.
        let mount = mounts.last().unwrap();
        assert_eq!(mount.name, "bootstrap");
        assert_eq!(mount.mount_path, "/run/vogt-bootstrap");
        assert_eq!(mount.read_only, Some(true));
        let secret = volumes.last().unwrap().secret.as_ref().unwrap();
        let name = secret.secret_name.as_deref().unwrap();
        assert!(
            name.starts_with(&format!("{}-", pod_name(id))) && name.ends_with("-bootstrap"),
            "{name}"
        );
        assert_eq!(secret.default_mode, Some(0o400));
        assert_eq!(secret.optional, Some(false));
    }

    #[test]
    fn pod_env_passes_the_graft_endpoint_only_when_configured() {
        let find = |cfg: &PodConfig| {
            pod_env(cfg, Uuid::nil())
                .into_iter()
                .find(|v| v.name == "GRAFT_ENDPOINT")
                .and_then(|v| v.value)
        };
        assert_eq!(find(&PodConfig::default()), None);
        let cfg = PodConfig {
            graft_endpoint: Some("http://graft.example:17888".into()),
            ..Default::default()
        };
        assert_eq!(find(&cfg).as_deref(), Some("http://graft.example:17888"));
    }

    #[test]
    fn pod_env_includes_optional_ca_contract() {
        let cfg = PodConfig {
            extra_ca_file: Some("/etc/ssl/custom.pem".into()),
            ..Default::default()
        };
        let env = pod_env(&cfg, Uuid::nil());
        assert_eq!(
            env.iter()
                .find(|v| v.name == "VOGT_EXTRA_CA_FILE")
                .unwrap()
                .value
                .as_deref(),
            Some("/etc/ssl/custom.pem")
        );
    }

    /// A mocked kube API: serves one pod (list and delete) and records every
    /// request's method, URI and body. A delete whose `preconditions.uid` does
    /// not match the pod's uid is refused with 409, as the API server does.
    mod mock_kube {
        use super::*;
        use std::sync::Mutex;

        pub type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

        pub fn client(pods: Vec<Pod>) -> (Client, Seen, Arc<Mutex<Vec<Pod>>>) {
            let seen: Seen = Arc::default();
            let store = Arc::new(Mutex::new(pods));
            let (log, state) = (seen.clone(), store.clone());
            let service = tower::service_fn(move |req: hyper::Request<kube::client::Body>| {
                let (log, state) = (log.clone(), state.clone());
                async move {
                    let (parts, body) = req.into_parts();
                    let body =
                        String::from_utf8_lossy(&body.collect_bytes().await.unwrap()).into_owned();
                    let uri = parts.uri.to_string();
                    log.lock()
                        .unwrap()
                        .push((parts.method.to_string(), uri.clone(), body.clone()));
                    let json = |status: u16, value: serde_json::Value| {
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(status)
                                .header("content-type", "application/json")
                                .body(kube::client::Body::from(value.to_string().into_bytes()))
                                .unwrap(),
                        )
                    };
                    if parts.method == hyper::Method::GET {
                        let items = state.lock().unwrap().clone();
                        return json(
                            200,
                            serde_json::json!({
                                "apiVersion": "v1", "kind": "PodList",
                                "metadata": {}, "items": items,
                            }),
                        );
                    }
                    let want: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let want = want["preconditions"]["uid"].as_str();
                    let mut pods = state.lock().unwrap();
                    let name = uri.rsplit('/').next().unwrap().split('?').next().unwrap();
                    let Some(at) = pods
                        .iter()
                        .position(|p| p.metadata.name.as_deref() == Some(name))
                    else {
                        return json(
                            404,
                            serde_json::json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"NotFound","code":404,"message":"not found"}),
                        );
                    };
                    if want.is_some() && want != pods[at].metadata.uid.as_deref() {
                        return json(
                            409,
                            serde_json::json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Conflict","code":409,"message":"uid precondition failed"}),
                        );
                    }
                    let gone = pods.remove(at);
                    json(200, serde_json::to_value(gone).unwrap())
                }
            });
            (Client::new(service, "vogt-sessions"), seen, store)
        }
    }

    fn named(id: Uuid, engine: &str, uid: &str) -> Pod {
        let mut pod = Pod::default();
        pod.metadata.name = Some(pod_name(id));
        pod.metadata.uid = Some(uid.into());
        pod.metadata.labels = Some(BTreeMap::from([
            ("vogt.session".into(), id.to_string()),
            (ENGINE_LABEL.into(), engine.into()),
        ]));
        pod
    }

    #[test]
    fn pods_carry_the_engine_label_and_the_selector_names_it() {
        let cfg = PodConfig {
            engine_id: "vogt-dev".into(),
            ..Default::default()
        };
        let template = SessionTemplate {
            name: "pod".into(),
            description: "pod".into(),
            command: None,
            cwd: None,
            env: vec![],
            default_name: None,
            match_repo_names: vec![],
            match_path_prefixes: vec![],
            tags: vec![],
            backend: "pod".into(),
            account: None,
        };
        let pod = pod_template(&cfg, &template, Uuid::nil(), "demo", "session", None, &[]);
        assert_eq!(
            pod.metadata.labels.as_ref().unwrap().get(ENGINE_LABEL),
            Some(&"vogt-dev".to_string())
        );
        assert!(pod_is_ours(&pod, &cfg));
        assert!(!pod_is_ours(&pod, &PodConfig::default()));
        assert_eq!(
            session_pod_selector(&cfg),
            "vogt.session,vogt.engine=vogt-dev"
        );
    }

    #[test]
    fn engine_ids_become_valid_label_values() {
        assert_eq!(sanitize_engine_id("vogt-prod"), "vogt-prod");
        assert_eq!(sanitize_engine_id("  prod/eu 1 "), "prod-eu-1");
        assert_eq!(sanitize_engine_id("---"), DEFAULT_ENGINE_ID);
        assert_eq!(sanitize_engine_id(""), DEFAULT_ENGINE_ID);
        assert_eq!(sanitize_engine_id(&"a".repeat(100)).len(), 63);
    }

    #[tokio::test]
    async fn the_list_request_carries_the_engine_selector() {
        let (client, seen, _) = mock_kube::client(vec![]);
        let cfg = PodConfig {
            engine_id: "vogt-dev".into(),
            ..Default::default()
        };
        let factory = PodBackendFactory {
            cfg: Arc::new(cfg),
            client,
        };
        factory.reconcile().await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "GET");
        assert!(
            seen[0]
                .1
                .contains("labelSelector=vogt.session%2Cvogt.engine%3Dvogt-dev"),
            "selector missing from {}",
            seen[0].1
        );
    }

    #[tokio::test]
    async fn a_stale_decision_cannot_delete_a_recreated_pod() {
        let id = Uuid::new_v4();
        // The sweep listed uid-old; the pod was then recreated under the same
        // name with a new uid.
        let (client, seen, store) =
            mock_kube::client(vec![named(id, DEFAULT_ENGINE_ID, "uid-new")]);
        let factory = PodBackendFactory {
            cfg: Arc::new(PodConfig::default()),
            client,
        };

        assert!(factory.delete(id, Some("uid-old".into())).await.is_err());
        assert_eq!(store.lock().unwrap().len(), 1, "the new pod survives");
        let sent = seen.lock().unwrap()[0].2.clone();
        assert!(sent.contains("\"uid\":\"uid-old\""), "body: {sent}");

        // With the uid of the pod that is there, the delete goes through.
        factory.delete(id, Some("uid-new".into())).await.unwrap();
        assert!(store.lock().unwrap().is_empty());
    }

    /// A mocked kube API for creation: pods get a uid; the Secret endpoint
    /// answers as `secret_status` says (201, 409 first-time, 403, ...).
    /// Records (method, path, body).
    mod mock_create {
        use super::*;
        use std::sync::Mutex;

        pub type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

        /// `secret_posts` lists the status for each successive Secret POST.
        pub fn client(secret_posts: Vec<u16>) -> (Client, Seen) {
            let seen: Seen = Arc::default();
            let log = seen.clone();
            let queue = Arc::new(Mutex::new(secret_posts.into_iter()));
            let service = tower::service_fn(move |req: hyper::Request<kube::client::Body>| {
                let (log, queue) = (log.clone(), queue.clone());
                async move {
                    let (parts, body) = req.into_parts();
                    let body =
                        String::from_utf8_lossy(&body.collect_bytes().await.unwrap()).into_owned();
                    let path = parts.uri.path().to_string();
                    log.lock().unwrap().push((
                        parts.method.to_string(),
                        path.clone(),
                        body.clone(),
                    ));
                    let json = |status: u16, value: serde_json::Value| {
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(status)
                                .header("content-type", "application/json")
                                .body(kube::client::Body::from(value.to_string().into_bytes()))
                                .unwrap(),
                        )
                    };
                    let failure = |code: u16| serde_json::json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"x","code":code,"message":"no"});
                    if parts.method == hyper::Method::POST && path.ends_with("/pods") {
                        let mut pod: serde_json::Value = serde_json::from_str(&body).unwrap();
                        pod["metadata"]["uid"] = "uid-pod".into();
                        return json(201, pod);
                    }
                    if parts.method == hyper::Method::POST && path.ends_with("/secrets") {
                        let status = queue.lock().unwrap().next().unwrap_or(201);
                        return if status == 201 {
                            json(201, serde_json::from_str(&body).unwrap())
                        } else {
                            json(status, failure(status))
                        };
                    }
                    // Deletes (pod or secret).
                    json(
                        200,
                        serde_json::json!({"kind":"Status","apiVersion":"v1","status":"Success"}),
                    )
                }
            });
            (Client::new(service, "vogt-sessions"), seen)
        }
    }

    fn pod_for(cfg: &PodConfig, id: Uuid) -> Pod {
        let mut pod = pod_template_for(cfg);
        pod.metadata.name = Some(pod_name(id));
        pod
    }

    #[tokio::test]
    async fn the_bootstrap_secret_is_created_owned_by_the_pod_and_the_spec_has_no_token() {
        let id = Uuid::new_v4();
        let cfg = PodConfig {
            engine_id: "vogt-dev".into(),
            ..Default::default()
        };
        let (client, seen) = mock_create::client(vec![]);
        let factory = PodBackendFactory {
            cfg: Arc::new(cfg.clone()),
            client,
        };
        factory
            .create_resources(id, b"s3cret-bootstrap", pod_for(&cfg, id))
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        let kinds: Vec<_> = seen
            .iter()
            .map(|(m, p, _)| (m.as_str(), p.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("POST", "/api/v1/namespaces/vogt-sessions/pods"),
                ("POST", "/api/v1/namespaces/vogt-sessions/secrets"),
            ],
            "the pod first (the Secret needs its uid), then the Secret"
        );
        // The pod body carries the Secret's name and no token anywhere.
        assert!(
            !seen[0].2.contains("s3cret-bootstrap"),
            "token in the pod spec: {}",
            seen[0].2
        );
        let secret: Secret = serde_json::from_str(&seen[1].2).unwrap();
        // The Secret is the one the pod's volume names.
        let pod: Pod = serde_json::from_str(&seen[0].2).unwrap();
        assert_eq!(
            secret.metadata.name,
            bootstrap_secret_name_in(&pod),
            "the Secret is not the one the pod mounts"
        );
        let owner = &secret.metadata.owner_references.as_ref().unwrap()[0];
        assert_eq!(
            (owner.kind.as_str(), owner.name.as_str(), owner.uid.as_str()),
            ("Pod", pod_name(id).as_str(), "uid-pod")
        );
        let labels = secret.metadata.labels.as_ref().unwrap();
        assert_eq!(labels["vogt.session"], id.to_string());
        assert_eq!(labels[ENGINE_LABEL], "vogt-dev");
        assert_eq!(
            secret.data.as_ref().unwrap()["token"].0,
            b"s3cret-bootstrap"
        );
        assert_eq!(secret.immutable, Some(true));
    }

    #[tokio::test]
    async fn a_pod_whose_secret_cannot_be_created_is_removed() {
        let id = Uuid::new_v4();
        let cfg = PodConfig::default();
        let (client, seen) = mock_create::client(vec![403]);
        let factory = PodBackendFactory {
            cfg: Arc::new(cfg.clone()),
            client,
        };
        assert!(factory
            .create_resources(id, b"t", pod_for(&cfg, id))
            .await
            .is_err());
        let seen = seen.lock().unwrap();
        let last = seen.last().unwrap();
        assert_eq!(last.0, "DELETE");
        assert!(
            last.1.ends_with(&format!("/pods/{}", pod_name(id))),
            "{}",
            last.1
        );
        assert!(
            last.2.contains("\"uid\":\"uid-pod\""),
            "uid precondition: {}",
            last.2
        );
    }

    #[tokio::test]
    async fn each_pod_gets_its_own_secret_name_and_the_engine_never_deletes_a_secret() {
        let id = Uuid::new_v4();
        let cfg = PodConfig::default();
        let (client, seen) = mock_create::client(vec![]);
        let factory = PodBackendFactory {
            cfg: Arc::new(cfg.clone()),
            client,
        };
        // Two pods of the same session, as after a wake: the earlier pod's
        // Secret may still be waiting for the garbage collector.
        let (first, second) = (pod_for(&cfg, id), pod_for(&cfg, id));
        assert_ne!(
            bootstrap_secret_name_in(&first),
            bootstrap_secret_name_in(&second)
        );
        factory.create_resources(id, b"t", first).await.unwrap();
        factory.create_resources(id, b"t", second).await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().all(|(m, _, _)| m == "POST"),
            "a request other than create: {seen:?}"
        );
        let names: Vec<String> = seen
            .iter()
            .filter(|(_, p, _)| p.ends_with("/secrets"))
            .map(|(_, _, b)| {
                serde_json::from_str::<Secret>(b)
                    .unwrap()
                    .metadata
                    .name
                    .unwrap()
            })
            .collect();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
    }

    #[tokio::test]
    async fn a_failed_secret_create_deletes_only_the_pod() {
        let id = Uuid::new_v4();
        let cfg = PodConfig::default();
        let (client, seen) = mock_create::client(vec![409]);
        let factory = PodBackendFactory {
            cfg: Arc::new(cfg.clone()),
            client,
        };
        assert!(factory
            .create_resources(id, b"t", pod_for(&cfg, id))
            .await
            .is_err());
        let seen = seen.lock().unwrap();
        let deletes: Vec<_> = seen.iter().filter(|(m, _, _)| m == "DELETE").collect();
        assert_eq!(deletes.len(), 1);
        assert!(deletes[0].1.contains("/pods/"), "{}", deletes[0].1);
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn pod_template_for(cfg: &PodConfig) -> Pod {
        let template = SessionTemplate {
            name: "Pod".into(),
            description: "pod".into(),
            command: None,
            cwd: None,
            env: vec![],
            default_name: None,
            match_repo_names: vec![],
            match_path_prefixes: vec![],
            tags: vec![],
            backend: "pod".into(),
            account: None,
        };
        pod_template(cfg, &template, Uuid::nil(), "vogt", "session", None, &[])
    }

    #[test]
    fn config_defaults_are_the_current_values() {
        let cfg = PodConfig::from_lookup(lookup(&[]), None).unwrap();
        let d = PodConfig::default();
        assert_eq!(cfg.namespace, "vogt-sessions");
        assert_eq!(cfg.default_image, d.default_image);
        assert_eq!(cfg.engine_endpoint, d.engine_endpoint);
        assert_eq!(cfg.service_account, "vogt-session");
        assert_eq!(cfg.priority_class.as_deref(), Some("vogt-session"));
        assert_eq!(cfg.cache_pvc, "session-cache-winrarhost1");
        assert_eq!(cfg.image_pull_secret, None);
        assert_eq!(cfg.resource_requests, d.resource_requests);
        assert_eq!(cfg.resource_limits, d.resource_limits);
    }

    #[test]
    fn config_reads_every_override() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_NAMESPACE", "smoke-sessions"),
                ("ENGINE_POD_IMAGE", "reg/pod:1"),
                ("ENGINE_POD_SERVICE_ACCOUNT", "sa"),
                ("ENGINE_POD_IMAGE_PULL_SECRET", "pull"),
                ("ENGINE_POD_PRIORITY_CLASS", ""),
                ("ENGINE_POD_CACHE_PVC", "pvc"),
                ("ENGINE_POD_RESOURCE_REQUESTS", "cpu=50m, memory=64Mi"),
                ("ENGINE_POD_RESOURCE_LIMITS", ""),
                ("ENGINE_POD_ENGINE_ID", "Smoke Engine"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.namespace, "smoke-sessions");
        assert_eq!(cfg.default_image, "reg/pod:1");
        assert_eq!(cfg.service_account, "sa");
        assert_eq!(cfg.image_pull_secret.as_deref(), Some("pull"));
        assert_eq!(cfg.priority_class, None);
        assert_eq!(cfg.cache_pvc, "pvc");
        assert_eq!(cfg.resource_requests.len(), 2);
        assert_eq!(cfg.resource_requests["cpu"].0, "50m");
        assert!(cfg.resource_limits.is_empty());
        assert_eq!(cfg.engine_id, sanitize_engine_id("Smoke Engine"));
    }

    #[test]
    fn endpoint_must_be_https_unless_explicitly_allowed() {
        let http = "http://vogt-smoke-engine.vogt-smoke.svc:8910/";
        assert!(
            PodConfig::from_lookup(lookup(&[("ENGINE_POD_ENGINE_ENDPOINT", http)]), None).is_err()
        );
        assert!(PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_ENGINE_ENDPOINT", http),
                ("ENGINE_POD_ALLOW_INSECURE_ENDPOINT", "false"),
            ]),
            None
        )
        .is_err());
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_ENGINE_ENDPOINT", http),
                ("ENGINE_POD_ALLOW_INSECURE_ENDPOINT", "true"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.engine_endpoint,
            "http://vogt-smoke-engine.vogt-smoke.svc:8910"
        );
        let https = PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_ENGINE_ENDPOINT", "https://e.example:18197")]),
            None,
        )
        .unwrap();
        assert_eq!(https.engine_endpoint, "https://e.example:18197");
        assert!(PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_ALLOW_INSECURE_ENDPOINT", "maybe")]),
            None
        )
        .is_err());
    }

    #[test]
    fn the_public_url_is_the_endpoint_unless_overridden() {
        let cfg = PodConfig::from_lookup(lookup(&[]), Some("https://pub.example".into())).unwrap();
        assert_eq!(cfg.engine_endpoint, "https://pub.example");
        let cfg = PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_ENGINE_ENDPOINT", "https://own.example")]),
            Some("https://pub.example".into()),
        )
        .unwrap();
        assert_eq!(cfg.engine_endpoint, "https://own.example");
    }

    #[test]
    fn the_vogt_prefixed_alias_is_read_and_the_plain_name_wins() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("VOGT_ENGINE_POD_NAMESPACE", "alias-ns"),
                ("VOGT_ENGINE_POD_IMAGE", "alias/img"),
                ("ENGINE_POD_IMAGE", "plain/img"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.namespace, "alias-ns");
        assert_eq!(cfg.default_image, "plain/img");
    }

    #[test]
    fn bad_resources_are_refused() {
        for bad in ["cpu", "cpu=", "=1"] {
            assert!(
                PodConfig::from_lookup(lookup(&[("ENGINE_POD_RESOURCE_LIMITS", bad)]), None)
                    .is_err()
            );
        }
    }

    #[test]
    fn empty_cache_names_leave_the_cache_out_of_the_pod() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_PVC", ""),
                ("ENGINE_POD_CACHE_NODE", ""),
                ("ENGINE_POD_CACHE_ENV_CONFIG_MAP", ""),
                ("ENGINE_POD_CACHE_SECRET", ""),
                ("ENGINE_POD_KACHE_CONFIG_MAP", ""),
                ("ENGINE_POD_IMAGE_PULL_SECRET", "pull"),
            ]),
            None,
        )
        .unwrap();
        let spec = pod_template_for(&cfg).spec.unwrap();
        // Only the bootstrap Secret volume is left.
        let volumes = spec.volumes.unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].name, "bootstrap");
        assert!(spec.node_name.is_none());
        let c = &spec.containers[0];
        assert!(c.env_from.as_ref().unwrap().is_empty());
        assert_eq!(c.volume_mounts.as_ref().unwrap().len(), 1);
        assert_eq!(spec.image_pull_secrets.unwrap()[0].name, "pull");
    }
}
