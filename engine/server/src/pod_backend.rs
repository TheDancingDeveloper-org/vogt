//! Kubernetes lifecycle support for opted-in session templates.
use crate::{
    config::SessionTemplate,
    error::{ApiError, Result},
    session_backend::SessionBackend,
    session_cache::{self, CacheProvisioner, HttpCacheProvisioner},
};
use k8s_openapi::api::core::v1::{
    Affinity, Capabilities, ConfigMapEnvSource, ConfigMapVolumeSource, Container,
    EmptyDirVolumeSource, EnvFromSource, EnvVar, HostPathVolumeSource, NodeAffinity, NodeSelector,
    NodeSelectorRequirement, NodeSelectorTerm, PersistentVolumeClaimVolumeSource, Pod,
    PodSecurityContext, PodSpec, PreferredSchedulingTerm, ResourceRequirements, SeccompProfile,
    Secret, SecretEnvSource, SecretVolumeSource, SecurityContext, Toleration, Volume, VolumeMount,
};
use k8s_openapi::{apimachinery::pkg::apis::meta::v1::OwnerReference, ByteString};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{
    api::{Api, DeleteParams, ListParams, LogParams, ObjectMeta, PostParams, Preconditions},
    Client, Config as KubeConfig,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;
#[cfg(test)]
use vogt_engine_contract::source::LandingMode;
use vogt_engine_contract::source::{SourceDescriptor, SourceLocation};

/// What one session template changes about its pod. An unset field keeps the
/// deployment default; a set resource map is merged over the default map key
/// by key, so a policy that leaves out `ephemeral-storage` keeps its cap.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PodTemplatePolicy {
    #[serde(default)]
    pub resource_requests:
        Option<BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>>,
    #[serde(default)]
    pub resource_limits:
        Option<BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>>,
    #[serde(default)]
    pub node_selector: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub affinity: Option<Affinity>,
    /// How long this template's pods may run before the kubelet stops them
    /// (`activeDeadlineSeconds`). Unset keeps the deployment default.
    #[serde(default)]
    pub active_deadline_seconds: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PodConfig {
    pub namespace: String,
    pub kubeconfig: Option<std::path::PathBuf>,
    pub engine_endpoint: String,
    /// MCP endpoint used by agent clients inside a session pod.
    #[serde(default)]
    pub mcp_url: Option<String>,
    pub default_image: String,
    pub connect_timeout: Duration,
    /// How long a running pod may go without an agent heartbeat.
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout: Duration,
    /// Default resource policy; values are copied verbatim into the pod.
    pub resource_requests:
        BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>,
    pub resource_limits: BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>,
    /// Labels every session pod must carry (`ENGINE_POD_NODE_SELECTOR`,
    /// comma-separated `KEY=VALUE`). A template policy may add keys; a key the
    /// deployment set cannot be given a different value.
    pub node_selector: BTreeMap<String, String>,
    /// The cache claim and node are deployment policy rather than pod logic.
    pub cache_pvc: String,
    pub cache_node: String,
    pub cache_env_config_map: String,
    pub cache_secret: String,
    pub kache_config_map: String,
    /// Per-template overrides, keyed by template name (WI-3166). A template
    /// with no entry gets the defaults above.
    #[serde(default)]
    pub template_policies: BTreeMap<String, PodTemplatePolicy>,
    /// Node directories mounted read-only as seeds beside the private cache
    /// (WI-3181). A `hostPath` is refused by the restricted Pod Security
    /// Standard, so these stay empty until the session namespace is relaxed
    /// for them; they are never writable (WI-3242).
    #[serde(default)]
    pub cargo_seed_host_path: Option<String>,
    #[serde(default)]
    pub pnpm_seed_host_path: Option<String>,
    /// A seed path must be at or under one of these directories (and never
    /// under a sensitive root, whatever is listed here).
    #[serde(default = "default_seed_allowed_prefixes")]
    pub seed_allowed_prefixes: Vec<String>,
    /// sccache S3-compatible endpoint, passed as `SCCACHE_ENDPOINT`. The
    /// credentials come from `cache_secret`, not from here.
    #[serde(default)]
    pub sccache_endpoint: Option<String>,
    /// The trusted kache provisioner (`ENGINE_POD_CACHE_PROVISIONER_URL`).
    /// When set, every pod gets a bucket and write key of its own and
    /// `cache_secret` must hold only the shared read-only key; unset, pods get
    /// the shared read-only key alone (WI-3242). See [`crate::session_cache`].
    #[serde(default)]
    pub cache_provisioner_url: Option<String>,
    /// File holding the engine's bearer for the provisioner (a projected
    /// ServiceAccount token); read on every call.
    #[serde(default)]
    pub cache_provisioner_token_file: Option<String>,
    pub cli: String,
    pub extra_ca_file: Option<String>,
    pub priority_class: Option<String>,
    /// Hostnames the scheduler should prefer, with a weight 1..=100 each
    /// (`ENGINE_POD_PREFERRED_NODES=winrarhost4:100,winrarhost1:50`, WI-3343).
    /// Added to every pod as preferred node affinity on top of the template
    /// policy's own; ignored by the scheduler while `cache_node` pins the pod
    /// by `nodeName`.
    #[serde(default)]
    pub preferred_nodes: Vec<(String, i32)>,
    /// Hostnames no session pod may land on (`ENGINE_POD_EXCLUDED_NODES`),
    /// ANDed into every required node-affinity term of the pod.
    #[serde(default)]
    pub excluded_nodes: Vec<String>,
    /// Tolerations for every session pod (`ENGINE_POD_TOLERATIONS`, a JSON
    /// array of Kubernetes tolerations), e.g. Node B's `estate/dedicated`.
    #[serde(default)]
    pub tolerations: Vec<Toleration>,
    pub service_account: String,
    /// Secret holding registry credentials for the session image, if private.
    #[serde(default)]
    pub image_pull_secret: Option<String>,
    /// How often the reconciler compares the cluster with the engine's
    /// sessions (`ENGINE_POD_RECONCILE_TIMEOUT`).
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval: Duration,
    /// How long a pod may sit Pending (unschedulable, pulling) before its
    /// session is ended.
    #[serde(default = "default_pending_timeout")]
    pub pending_timeout: Duration,
    /// How long a session pod may run before the kubelet terminates it
    /// (`activeDeadlineSeconds`, default 12 h). Zero leaves the deadline
    /// unset; a template policy may override it.
    #[serde(default = "default_session_deadline")]
    pub session_deadline: Duration,
    /// How long an adopted pod's agent has to dial in again after an engine
    /// restart before the pod is given up as lost.
    #[serde(default = "default_reconnect_timeout")]
    pub reconnect_timeout: Duration,
    /// Names this engine deployment. Every pod it creates carries it as the
    /// `vogt.engine` label, and the reconciler lists and deletes only pods
    /// that carry its own, so engines sharing a namespace (dev and prod, or an
    /// old process during a rollout) never touch each other's pods. Keep it
    /// The read-only workspace tree every session pod gets (WI-3373/WI-3374);
    /// unset keeps the pre-feature spec.
    #[serde(default)]
    pub workspace: Option<PodWorkspace>,
    /// stable for a deployment and distinct between deployments.
    #[serde(default = "default_engine_id")]
    pub engine_id: String,
}
/// The read-only workspace tree every session pod gets (WI-3373) and the
/// writable per-pod clone beneath it (WI-3374). Off unless
/// `ENGINE_POD_WORKSPACE=1`: a deployment that does not opt in keeps the
/// pre-feature pod spec exactly.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PodWorkspace {
    /// Where the tree is mounted in the session container. The clone goes in
    /// `session/` beneath it, on its own writable emptyDir, so the workspace
    /// files are ancestors of the session's cwd as they are on a host.
    pub dir: String,
    /// The workspace-configuration repository, at a pinned commit. Both
    /// sources are public, so the init container needs no deploy key, token or
    /// Infisical identity and none can reach the session container.
    pub config_repo: String,
    pub config_commit: String,
    /// The shared-skills repository, at a pinned commit.
    pub skills_repo: String,
    pub skills_commit: String,
}

/// The workspace tree's default mount path, beside the image's empty
/// `/home/vogt/Working` it replaces.
pub const DEFAULT_WORKSPACE_DIR: &str = "/home/vogt/Working";
/// Where the session's clone lives inside the workspace tree. The init
/// container creates it in the workspace volume, so the nested writable mount
/// has a mount point; the session container only ever sees it writable.
pub const WORKSPACE_SESSION_DIR: &str = "session";
pub const DEFAULT_WORKSPACE_CONFIG_REPO: &str =
    "https://github.com/TheDancingDeveloper-org/agent-workspace-config.git";
pub const DEFAULT_WORKSPACE_SKILLS_REPO: &str =
    "https://repo.indexarr.net/indexarr/UsefulClaudeSkills.git";

impl PodWorkspace {
    /// The clone root: `<dir>/session`.
    pub fn session_dir(&self) -> String {
        format!("{}/{WORKSPACE_SESSION_DIR}", self.dir.trim_end_matches('/'))
    }
}

/// A workspace mount is an absolute, normalised path strictly under the
/// session home: the home itself is the writable emptyDir the nested session
/// mount hangs from, and a path outside it would make the guidance walk and
/// the nested mount disagree with the image's layout.
fn validate_workspace_dir(raw: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(ApiError::Config(format!(
            "ENGINE_POD_WORKSPACE_DIR {why}: {raw:?}"
        )))
    };
    let Some(rest) = raw.strip_prefix('/') else {
        return bad("must be an absolute path");
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() || rest.split('/').any(|s| matches!(s, "" | "." | "..")) {
        return bad("must be a normalised path");
    }
    let path = format!("/{rest}");
    if path == HOME_MOUNT || !under(&path, HOME_MOUNT) {
        return bad("must be under the session home (/home/vogt)");
    }
    Ok(())
}

/// A full commit id, so a deployment cannot follow a branch whose content
/// changes between pods.
fn validate_pinned_commit(name: &str, value: &str) -> Result<()> {
    if value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ApiError::Config(format!(
            "{name} must be a full 40-character commit id, got {value:?}"
        )))
    }
}

/// Where a node's seed directories may live unless the deployment says so.
fn default_seed_allowed_prefixes() -> Vec<String> {
    [
        "/srv",
        "/mnt",
        "/data",
        "/opt",
        "/var/cache",
        "/var/lib/vogt",
    ]
    .map(String::from)
    .to_vec()
}

/// Node roots a seed can never be mounted from, even if an allowed prefix
/// would cover them.
const SENSITIVE_SEED_ROOTS: &[&str] = &[
    "/etc",
    "/proc",
    "/sys",
    "/dev",
    "/boot",
    "/root",
    "/run",
    "/var/run",
    "/var/lib/kubelet",
    "/var/lib/containerd",
    "/var/lib/docker",
];

fn under(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|r| r.starts_with('/'))
}

/// A seed host path: absolute, normalised (no `.`/`..`/empty segments), not
/// `/`, outside the sensitive roots and under an allowed prefix.
fn validate_seed_path(name: &str, path: &str, allowed: &[String]) -> Result<()> {
    let bad = |why: &str| Err(ApiError::Config(format!("{name} {why}: {path:?}")));
    let Some(rest) = path.strip_prefix('/') else {
        return bad("must be an absolute path");
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        return bad("must not be the node root");
    }
    if rest.split('/').any(|s| matches!(s, "" | "." | "..")) {
        return bad("must not contain empty, `.` or `..` segments");
    }
    let path = format!("/{rest}");
    if SENSITIVE_SEED_ROOTS.iter().any(|r| under(&path, r)) {
        return bad("is under a sensitive node directory");
    }
    if !allowed
        .iter()
        .any(|a| under(&path, a.trim_end_matches('/')))
    {
        return bad(&format!(
            "must be under one of ENGINE_POD_SEED_ALLOWED_PREFIXES ({})",
            allowed.join(", ")
        ));
    }
    Ok(())
}

/// The sccache endpoint: an http(s) URL with a host and no userinfo, so no
/// credential rides in the pod env (those come from the cache secret).
fn validate_sccache_endpoint(raw: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(ApiError::Config(format!(
            "ENGINE_POD_SCCACHE_ENDPOINT {why}"
        )))
    };
    let Ok(url) = url::Url::parse(raw) else {
        return bad("is not a valid URL");
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return bad("must be an http(s) URL with a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return bad("must not carry credentials (userinfo)");
    }
    Ok(())
}

fn is_hostname_key(key: &str) -> bool {
    key == "kubernetes.io/hostname"
}

/// `nodeName` binds the pod to the cache node and skips the scheduler, so a
/// template's selector or affinity cannot move it; the kubelet then fails the
/// pod instead. Refuse a policy whose hostname constraint excludes that node.
fn check_policy_against_node(template: &str, p: &PodTemplatePolicy, node: &str) -> Result<()> {
    if node.is_empty() {
        return Ok(());
    }
    let conflict = |what: &str| {
        Err(ApiError::Config(format!(
            "ENGINE_POD_TEMPLATE_POLICIES[{template}]: {what} conflicts with \
             ENGINE_POD_CACHE_NODE={node}; the pod is pinned by nodeName, which \
             wins over scheduling"
        )))
    };
    if let Some(h) = p
        .node_selector
        .as_ref()
        .and_then(|s| s.get("kubernetes.io/hostname"))
    {
        if h != node {
            return conflict("node_selector kubernetes.io/hostname");
        }
    }
    let required = p
        .affinity
        .as_ref()
        .and_then(|a| a.node_affinity.as_ref())
        .and_then(|n| {
            n.required_during_scheduling_ignored_during_execution
                .as_ref()
        });
    if let Some(sel) = required {
        // Terms are ORed: the pod is only excluded if every term excludes it.
        let excluded = |t: &k8s_openapi::api::core::v1::NodeSelectorTerm| {
            let by_expr = t.match_expressions.iter().flatten().any(|e| {
                is_hostname_key(&e.key)
                    && match e.operator.as_str() {
                        "In" => !e.values.iter().flatten().any(|v| v == node),
                        "NotIn" => e.values.iter().flatten().any(|v| v == node),
                        _ => false,
                    }
            });
            let by_field = t.match_fields.iter().flatten().any(|e| {
                e.key == "metadata.name"
                    && match e.operator.as_str() {
                        "In" => !e.values.iter().flatten().any(|v| v == node),
                        "NotIn" => e.values.iter().flatten().any(|v| v == node),
                        _ => false,
                    }
            });
            by_expr || by_field
        };
        if !sel.node_selector_terms.is_empty() && sel.node_selector_terms.iter().all(excluded) {
            return conflict("required node affinity");
        }
    }
    Ok(())
}

fn hostname_requirement(operator: &str, nodes: &[String]) -> NodeSelectorRequirement {
    NodeSelectorRequirement {
        key: "kubernetes.io/hostname".into(),
        operator: operator.into(),
        values: Some(nodes.to_vec()),
    }
}

/// `key=value,key=value` labels every session pod must carry. A repeated key
/// is refused rather than silently keeping the last value.
fn parse_node_selector(raw: &str) -> Result<BTreeMap<String, String>> {
    let bad = |item: &str| {
        ApiError::Config(format!(
            "ENGINE_POD_NODE_SELECTOR: {item:?} is not KEY=VALUE"
        ))
    };
    let mut selector = BTreeMap::new();
    for item in raw.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let (key, value) = item.split_once('=').ok_or_else(|| bad(item))?;
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || value.is_empty() {
            return Err(bad(item));
        }
        if selector
            .insert(key.to_string(), value.to_string())
            .is_some()
        {
            return Err(ApiError::Config(format!(
                "ENGINE_POD_NODE_SELECTOR: {key:?} is set more than once"
            )));
        }
    }
    Ok(selector)
}

/// `name:weight,name:weight` with weight 1..=100 (default 1 when omitted).
fn parse_preferred_nodes(raw: &str) -> Result<Vec<(String, i32)>> {
    let bad = |item: &str| {
        ApiError::Config(format!(
            "ENGINE_POD_PREFERRED_NODES: {item:?} is not NAME[:WEIGHT] with weight 1..=100"
        ))
    };
    raw.split(',')
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .map(|item| {
            let (name, weight) = match item.split_once(':') {
                Some((n, w)) => (n.trim(), w.trim().parse::<i32>().map_err(|_| bad(item))?),
                None => (item, 1),
            };
            if name.is_empty() || !(1..=100).contains(&weight) {
                return Err(bad(item));
            }
            Ok((name.to_string(), weight))
        })
        .collect()
}

/// The pod's labels: the deployment's hard selector, plus any key a template
/// policy adds. Startup validation refuses a policy that changes a deployment
/// key, so this keeps the deployment's value on a collision.
fn merged_node_selector(
    cfg: &PodConfig,
    policy: Option<&PodTemplatePolicy>,
) -> BTreeMap<String, String> {
    let mut selector = cfg.node_selector.clone();
    if let Some(policy_selector) = policy.and_then(|p| p.node_selector.as_ref()) {
        for (key, value) in policy_selector {
            selector.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    selector
}

/// The pod's affinity: the template policy's, plus the deployment's preferred
/// nodes (one weighted term each) and excluded nodes (a `NotIn` hostname
/// expression added to every required term, since terms are ORed).
fn merged_affinity(cfg: &PodConfig, policy: Option<&PodTemplatePolicy>) -> Option<Affinity> {
    let mut affinity = policy.and_then(|p| p.affinity.clone());
    if cfg.preferred_nodes.is_empty() && cfg.excluded_nodes.is_empty() {
        return affinity;
    }
    let a = affinity.get_or_insert_with(Affinity::default);
    let n = a.node_affinity.get_or_insert_with(NodeAffinity::default);
    for (node, weight) in &cfg.preferred_nodes {
        n.preferred_during_scheduling_ignored_during_execution
            .get_or_insert_with(Vec::new)
            .push(PreferredSchedulingTerm {
                weight: *weight,
                preference: NodeSelectorTerm {
                    match_expressions: Some(vec![hostname_requirement(
                        "In",
                        std::slice::from_ref(node),
                    )]),
                    match_fields: None,
                },
            });
    }
    if !cfg.excluded_nodes.is_empty() {
        let exclude = hostname_requirement("NotIn", &cfg.excluded_nodes);
        let sel = n
            .required_during_scheduling_ignored_during_execution
            .get_or_insert_with(|| NodeSelector {
                node_selector_terms: Vec::new(),
            });
        if sel.node_selector_terms.is_empty() {
            sel.node_selector_terms.push(NodeSelectorTerm::default());
        }
        for term in &mut sel.node_selector_terms {
            term.match_expressions
                .get_or_insert_with(Vec::new)
                .push(exclude.clone());
        }
    }
    affinity
}

fn default_reconnect_timeout() -> Duration {
    Duration::from_secs(120)
}
fn default_heartbeat_timeout() -> Duration {
    Duration::from_secs(60)
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

/// Make a project identity safe to use as a Kubernetes label value (at most
/// 63 characters of `[A-Za-z0-9._-]`, starting and ending alphanumeric).
/// Repository identities may contain URL punctuation, so replace anything
/// outside the label alphabet rather than passing it through to the API.
pub fn sanitize_project_label(raw: &str) -> String {
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
        "project".into()
    } else {
        kept.to_string()
    }
}

fn source_url(source: &SourceDescriptor) -> &str {
    match &source.location {
        SourceLocation::Git { url, .. } => url,
        SourceLocation::Graft { endpoint, .. } => endpoint,
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
fn default_session_deadline() -> Duration {
    Duration::from_secs(12 * 60 * 60)
}
impl Default for PodConfig {
    fn default() -> Self {
        Self {
            namespace: "vogt-sessions".into(),
            kubeconfig: None,
            engine_endpoint: "http://192.168.1.75:18197".into(),
            mcp_url: None,
            default_image: "ghcr.io/thedancingdeveloper-org/vogt-session:latest".into(),
            connect_timeout: Duration::from_secs(30),
            heartbeat_timeout: default_heartbeat_timeout(),
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
            template_policies: BTreeMap::new(),
            cargo_seed_host_path: None,
            pnpm_seed_host_path: None,
            seed_allowed_prefixes: default_seed_allowed_prefixes(),
            sccache_endpoint: None,
            cache_provisioner_url: None,
            cache_provisioner_token_file: None,
            cli: "codex".into(),
            extra_ca_file: None,
            priority_class: Some("vogt-session".into()),
            preferred_nodes: Vec::new(),
            excluded_nodes: Vec::new(),
            tolerations: Vec::new(),
            service_account: "vogt-session".into(),
            image_pull_secret: None,
            reconcile_interval: default_reconcile_interval(),
            pending_timeout: default_pending_timeout(),
            session_deadline: default_session_deadline(),
            reconnect_timeout: default_reconnect_timeout(),
            workspace: None,
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
        if let Some(v) = text("ENGINE_POD_NODE_SELECTOR") {
            cfg.node_selector = parse_node_selector(&v)?;
        }
        if let Some(v) = text("ENGINE_POD_PREFERRED_NODES") {
            cfg.preferred_nodes = parse_preferred_nodes(&v)?;
        }
        if let Some(v) = text("ENGINE_POD_EXCLUDED_NODES") {
            cfg.excluded_nodes = v
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(v) = text("ENGINE_POD_TOLERATIONS").filter(|v| !v.is_empty()) {
            cfg.tolerations = serde_json::from_str(&v).map_err(|e| {
                ApiError::Config(format!("ENGINE_POD_TOLERATIONS: invalid JSON: {e}"))
            })?;
        }
        optional("ENGINE_POD_IMAGE_PULL_SECRET", &mut cfg.image_pull_secret);
        optional("ENGINE_POD_EXTRA_CA_FILE", &mut cfg.extra_ca_file);
        optional(
            "ENGINE_POD_CACHE_PROVISIONER_URL",
            &mut cfg.cache_provisioner_url,
        );
        optional(
            "ENGINE_POD_CACHE_PROVISIONER_TOKEN_FILE",
            &mut cfg.cache_provisioner_token_file,
        );
        optional(
            "ENGINE_POD_CARGO_SEED_HOST_PATH",
            &mut cfg.cargo_seed_host_path,
        );
        optional(
            "ENGINE_POD_PNPM_SEED_HOST_PATH",
            &mut cfg.pnpm_seed_host_path,
        );
        optional("ENGINE_POD_SCCACHE_ENDPOINT", &mut cfg.sccache_endpoint);
        if let Some(v) = text("ENGINE_POD_TEMPLATE_POLICIES").filter(|v| !v.is_empty()) {
            cfg.template_policies = serde_json::from_str(&v).map_err(|e| {
                ApiError::Config(format!("ENGINE_POD_TEMPLATE_POLICIES: invalid JSON: {e}"))
            })?;
        }
        if let Some(v) = text("ENGINE_POD_RESOURCE_REQUESTS") {
            cfg.resource_requests = parse_resources("ENGINE_POD_RESOURCE_REQUESTS", &v)?;
        }
        if let Some(v) = text("ENGINE_POD_RESOURCE_LIMITS") {
            cfg.resource_limits = parse_resources("ENGINE_POD_RESOURCE_LIMITS", &v)?;
        }
        for (name, target) in [
            ("ENGINE_POD_CONNECT_TIMEOUT", &mut cfg.connect_timeout),
            ("ENGINE_POD_PENDING_TIMEOUT", &mut cfg.pending_timeout),
            ("ENGINE_POD_RECONNECT_TIMEOUT", &mut cfg.reconnect_timeout),
            ("ENGINE_POD_RECONCILE_TIMEOUT", &mut cfg.reconcile_interval),
            ("ENGINE_POD_HEARTBEAT_TIMEOUT", &mut cfg.heartbeat_timeout),
        ] {
            if let Some(value) = text(name) {
                let secs: u64 = value.parse().map_err(|_| {
                    ApiError::Config(format!(
                        "{name} must be a whole number of seconds, got {value:?}"
                    ))
                })?;
                *target = Duration::from_secs(secs);
            }
        }
        if let Some(v) = text("ENGINE_POD_SESSION_DEADLINE_SECONDS") {
            let secs: u64 = v.parse().map_err(|_| {
                ApiError::Config(format!(
                    "ENGINE_POD_SESSION_DEADLINE_SECONDS must be a whole number of seconds, got {v:?}"
                ))
            })?;
            cfg.session_deadline = Duration::from_secs(secs);
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
        optional("ENGINE_POD_MCP_URL", &mut cfg.mcp_url);
        let workspace_enabled = match text("ENGINE_POD_WORKSPACE").as_deref() {
            None | Some("") | Some("false") | Some("0") => false,
            Some("true") | Some("1") => true,
            Some(other) => {
                return Err(ApiError::Config(format!(
                    "ENGINE_POD_WORKSPACE must be true or false, got {other:?}"
                )))
            }
        };
        if workspace_enabled {
            // Both commits are required and pinned: a pod's guidance must not
            // change because a branch moved between two launches.
            let pinned = |name: &str| -> Result<String> {
                let value = text(name).filter(|v| !v.is_empty()).ok_or_else(|| {
                    ApiError::Config(format!(
                        "{name} is required when ENGINE_POD_WORKSPACE is enabled"
                    ))
                })?;
                validate_pinned_commit(name, &value)?;
                Ok(value)
            };
            let dir = text("ENGINE_POD_WORKSPACE_DIR")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_WORKSPACE_DIR.to_string());
            validate_workspace_dir(&dir)?;
            let repo = |name: &str, default: &str| {
                text(name)
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| default.to_string())
            };
            cfg.workspace = Some(PodWorkspace {
                dir,
                config_repo: repo(
                    "ENGINE_POD_WORKSPACE_CONFIG_REPO",
                    DEFAULT_WORKSPACE_CONFIG_REPO,
                ),
                config_commit: pinned("ENGINE_POD_WORKSPACE_CONFIG_COMMIT")?,
                skills_repo: repo(
                    "ENGINE_POD_WORKSPACE_SKILLS_REPO",
                    DEFAULT_WORKSPACE_SKILLS_REPO,
                ),
                skills_commit: pinned("ENGINE_POD_WORKSPACE_SKILLS_COMMIT")?,
            });
        }
        if let Some(v) = text("ENGINE_POD_SEED_ALLOWED_PREFIXES") {
            cfg.seed_allowed_prefixes = v
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
        }
        for (name, path) in [
            ("ENGINE_POD_CARGO_SEED_HOST_PATH", &cfg.cargo_seed_host_path),
            ("ENGINE_POD_PNPM_SEED_HOST_PATH", &cfg.pnpm_seed_host_path),
        ] {
            if let Some(path) = path {
                validate_seed_path(name, path, &cfg.seed_allowed_prefixes)?;
            }
        }
        if let Some(endpoint) = &cfg.sccache_endpoint {
            validate_sccache_endpoint(endpoint)?;
        }
        // nodeName wins over scheduling, so an excluded cache node could never
        // run a pod; refuse it rather than let the kubelet fail every pod.
        if cfg.excluded_nodes.contains(&cfg.cache_node) {
            return Err(ApiError::Config(format!(
                "ENGINE_POD_EXCLUDED_NODES lists ENGINE_POD_CACHE_NODE={}; the pod is \
                 pinned by nodeName, which wins over scheduling",
                cfg.cache_node
            )));
        }
        if let Some(hostname) = cfg.node_selector.get("kubernetes.io/hostname") {
            if cfg.excluded_nodes.contains(hostname) {
                return Err(ApiError::Config(format!(
                    "ENGINE_POD_NODE_SELECTOR sets kubernetes.io/hostname={hostname}, which \
                     ENGINE_POD_EXCLUDED_NODES also lists; the pod could never be scheduled"
                )));
            }
            if !cfg.cache_node.is_empty() && cfg.cache_node != *hostname {
                return Err(ApiError::Config(format!(
                    "ENGINE_POD_NODE_SELECTOR sets kubernetes.io/hostname={hostname} but \
                     ENGINE_POD_CACHE_NODE={}; the pod is pinned by nodeName, which wins \
                     over scheduling",
                    cfg.cache_node
                )));
            }
        }
        for (template, policy) in &cfg.template_policies {
            check_policy_against_node(template, policy, &cfg.cache_node)?;
            if let Some(hostname) = policy
                .node_selector
                .as_ref()
                .and_then(|selector| selector.get("kubernetes.io/hostname"))
            {
                if cfg.excluded_nodes.contains(hostname) {
                    return Err(ApiError::Config(format!(
                        "ENGINE_POD_TEMPLATE_POLICIES[{template}]: node_selector \
                         kubernetes.io/hostname={hostname}, which ENGINE_POD_EXCLUDED_NODES \
                         also lists; the pod could never be scheduled"
                    )));
                }
            }
            if let Some(selector) = &policy.node_selector {
                for (key, value) in selector {
                    if let Some(configured) = cfg.node_selector.get(key) {
                        if configured != value {
                            return Err(ApiError::Config(format!(
                                "ENGINE_POD_TEMPLATE_POLICIES[{template}]: node_selector \
                                 {key}={value} conflicts with ENGINE_POD_NODE_SELECTOR \
                                 {key}={configured}"
                            )));
                        }
                    }
                }
            }
            if policy.active_deadline_seconds.is_some_and(|s| s <= 0) {
                return Err(ApiError::Config(format!(
                    "ENGINE_POD_TEMPLATE_POLICIES: {template}.active_deadline_seconds \
                     must be a positive number of seconds"
                )));
            }
        }
        if let Some(url) = &cfg.cache_provisioner_url {
            if !(url.starts_with("https://") || (insecure && url.starts_with("http://"))) {
                return Err(ApiError::Config(
                    "ENGINE_POD_CACHE_PROVISIONER_URL must be an https URL \
                     (plain http needs ENGINE_POD_ALLOW_INSECURE_ENDPOINT=true)"
                        .into(),
                ));
            }
        }
        Ok(cfg)
    }

    /// The clone root a session pod uses: `<workspace>/session` when the
    /// workspace is on (WI-3374), else the image's `/work`.
    pub fn work_dir(&self) -> String {
        match &self.workspace {
            Some(workspace) => workspace.session_dir(),
            None => vogt_session_agent::DEFAULT_WORK_DIR.to_string(),
        }
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
/// Grace the engine gives a session pod it deletes. The agent has exited by
/// then, so this only bounds how long the pod lingers in `Terminating`, during
/// which its name cannot be reused by a wake.
const POD_DELETE_GRACE_SECONDS: u32 = 5;

/// The container the session agent (entrypoint and CLI) runs in.
pub const SESSION_CONTAINER: &str = "session-agent";
/// How many lines of that container's log a failed-session note keeps.
pub const LOG_TAIL_LINES: i64 = 50;
/// How long [`teardown_pod`] waits for a deleted pod to leave the cluster. Under
/// the 30 s a hibernate waits for the teardown, so a slow API is reported by
/// the teardown, not by the hibernate giving up first.
pub const POD_GONE_TIMEOUT: Duration = Duration::from_secs(20);
/// How often [`teardown_pod`] asks whether the pod is gone yet.
pub const POD_GONE_POLL: Duration = Duration::from_millis(250);

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

/// Where a session's own tool caches live: a per-pod `emptyDir`, so nothing a
/// session writes there can reach another session (WI-3242). `CARGO_HOME`
/// (config.toml, bin, credentials), npm, pnpm, pip, uv and XDG caches all sit
/// under it; a package cache that any session can write is code execution in
/// the next session that installs from it.
const PRIVATE_CACHE_VOLUME: &str = "session-cache";
const PRIVATE_CACHE_DIR: &str = "/cache";
/// The container root filesystem is read-only, so every path the entrypoint,
/// the agent CLIs and a coding session write to is a per-pod `emptyDir`: the
/// session user's home (and with it `CODEX_HOME`, `CLAUDE_CONFIG_DIR` and
/// `XDG_CONFIG_HOME`), `/tmp`, the clone and work tree, and the runtime
/// directory the bootstrap writes its token and MCP files to.
const HOME_MOUNT: &str = "/home/vogt";
const TMP_MOUNT: &str = "/tmp";
const RUNTIME_MOUNT: &str = "/run/vogt";
/// The pod-local emptyDir that carries a pod's read-only workspace tree
/// (WI-3373) and the init container that builds it.
const WORKSPACE_VOLUME: &str = "session-workspace";
const WORKSPACE_INIT_CONTAINER: &str = "workspace-config";
/// The shared per-node claim, read-only. Sessions can read a seed an operator
/// put there but can never write it; nothing in the image reads it by default.
const SHARED_CACHE_VOLUME: &str = "session-cache-seed";
const SHARED_CACHE_DIR: &str = "/cache-ro";
/// Cache locations pinned in the container env. A container `env` entry beats
/// `envFrom`, so the cache ConfigMap cannot point a tool back at shared
/// storage. Every path is under [`PRIVATE_CACHE_DIR`].
const PRIVATE_CACHE_ENV: &[(&str, &str)] = &[
    ("CARGO_HOME", "/cache/cargo"),
    ("XDG_CACHE_HOME", "/cache/xdg"),
    ("NPM_CONFIG_CACHE", "/cache/npm"),
    ("npm_config_cache", "/cache/npm"),
    ("npm_config_store_dir", "/cache/pnpm-store"),
    ("PNPM_HOME", "/cache/pnpm"),
    ("PIP_CACHE_DIR", "/cache/pip"),
    ("UV_CACHE_DIR", "/cache/uv"),
];

/// Volumes in `pod` that a session can write and another session's pod can
/// also mount: any claim, host path, NFS or CSI source without both the
/// volume and the mount marked read-only. `emptyDir`, Secret, ConfigMap,
/// projected and downward-API volumes are per pod (or read-only by
/// construction) and never count. The engine refuses to create a pod that has
/// one, so a later change to the template cannot reopen WI-3242.
pub fn shared_writable_volumes(pod: &Pod) -> Vec<String> {
    let Some(spec) = pod.spec.as_ref() else {
        return Vec::new();
    };
    let volumes = spec.volumes.as_deref().unwrap_or_default();
    let mut flagged = Vec::new();
    for volume in volumes {
        let (shared, volume_read_only) = if let Some(pvc) = &volume.persistent_volume_claim {
            (true, pvc.read_only == Some(true))
        } else if volume.host_path.is_some()
            || volume.nfs.is_some()
            || volume.csi.is_some()
            || volume.cephfs.is_some()
            || volume.glusterfs.is_some()
            || volume.iscsi.is_some()
            || volume.fc.is_some()
            || volume.rbd.is_some()
            || volume.flex_volume.is_some()
        {
            // These have no per-volume read-only switch we can rely on across
            // sources, so only the mount can make them read-only.
            (true, true)
        } else if volume.empty_dir.is_some()
            || volume.secret.is_some()
            || volume.config_map.is_some()
            || volume.projected.is_some()
            || volume.downward_api.is_some()
            || volume.ephemeral.is_some()
        {
            (false, true)
        } else {
            // A source this check does not know is treated as shared.
            (true, false)
        };
        if !shared {
            continue;
        }
        let writable = spec
            .containers
            .iter()
            .chain(spec.init_containers.iter().flatten())
            .flat_map(|c| c.volume_mounts.iter().flatten())
            .any(|m| m.name == volume.name && m.read_only != Some(true))
            || !volume_read_only;
        if writable {
            flagged.push(volume.name.clone());
        }
    }
    flagged
}

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

fn pod_env(
    cfg: &PodConfig,
    source: &SourceDescriptor,
    source_env: &[(String, String)],
    id: Uuid,
    permission_mode: Option<&str>,
    codex_approval_policy: &str,
) -> Vec<EnvVar> {
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
            name: "VOGT_SOURCE_JSON".into(),
            value: Some(serde_json::to_string(source).expect("source serializes")),
            ..Default::default()
        },
        EnvVar {
            name: "VOGT_CLI".into(),
            value: Some(cfg.cli.clone()),
            ..Default::default()
        },
        // The clone root: the entrypoint checks the project's source out here,
        // and a caller's cwd, carried through the bootstrap response, is a
        // path inside it (WI-3883). With a workspace tree (WI-3374) the clone
        // sits under it, so the workspace files are ancestors of the session.
        EnvVar {
            name: "VOGT_CLONE_DIR".into(),
            value: Some(cfg.work_dir()),
            ..Default::default()
        },
        // The session's working directory. The bootstrap response replaces it
        // with the caller's cwd inside the clone; with no cwd it stays the
        // clone root.
        EnvVar {
            name: "VOGT_WORK_DIR".into(),
            value: Some(cfg.work_dir()),
            ..Default::default()
        },
    ];
    // The session agent gathers Codex guidance from the pod's own workspace
    // tree (WI-3374); the engine cannot read a per-pod emptyDir.
    if let Some(workspace) = &cfg.workspace {
        env.push(EnvVar {
            name: "VOGT_WORKSPACE_ROOT".into(),
            value: Some(workspace.dir.clone()),
            ..Default::default()
        });
    }
    // The session agent builds the pod's Codex line itself, so the posture the
    // engine validated and would have put on a host argv has to travel with
    // the pod (WI-3864). The default posture is the CLI default: leave the
    // variable out and the pod's own `Config` applies it.
    if let Some(mode) = permission_mode.map(str::trim) {
        if !mode.is_empty() && mode != "default" {
            env.push(EnvVar {
                name: "VOGT_PERMISSION_MODE".into(),
                value: Some(mode.to_string()),
                ..Default::default()
            });
        }
    }
    env.push(EnvVar {
        name: "VOGT_CODEX_APPROVAL_POLICY".into(),
        value: Some(codex_approval_policy.to_string()),
        ..Default::default()
    });
    if let Some(url) = &cfg.mcp_url {
        env.push(EnvVar {
            name: "VOGT_MCP_URL".into(),
            value: Some(url.clone()),
            ..Default::default()
        });
    }
    env.extend(source_env.iter().map(|(name, value)| EnvVar {
        name: name.clone(),
        value: Some(value.clone()),
        ..Default::default()
    }));
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

/// Whether the kubelet stopped this pod because it outlived its
/// `activeDeadlineSeconds`. The pod and the terminated container both carry
/// `DeadlineExceeded`; either is enough, and the reconciler gives such a
/// session its own end reason rather than the generic failure note.
pub fn pod_hit_active_deadline(pod: &Pod) -> bool {
    const DEADLINE_EXCEEDED: &str = "DeadlineExceeded";
    let Some(status) = pod.status.as_ref() else {
        return false;
    };
    if status.reason.as_deref() == Some(DEADLINE_EXCEEDED) {
        return true;
    }
    status.container_statuses.as_ref().is_some_and(|statuses| {
        statuses.iter().any(|container| {
            container
                .state
                .as_ref()
                .and_then(|state| state.terminated.as_ref())
                .and_then(|terminated| terminated.reason.as_deref())
                == Some(DEADLINE_EXCEEDED)
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

type ResourceMap = BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>;

/// The deployment defaults with a template policy's entries laid over them.
fn merge_resources(defaults: &ResourceMap, over: Option<&ResourceMap>) -> ResourceMap {
    let mut merged = defaults.clone();
    if let Some(over) = over {
        merged.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    merged
}

/// The source a pod-backed session checks out (WI-3369): the project's own
/// descriptor when the session carries one, else the deployment's default.
/// The provider is picked from `registry` by the descriptor's kind, and a
/// source it cannot serve is refused before anything is created.
pub fn session_source(
    requested: Option<&SourceDescriptor>,
    registry: &crate::source_provider::SourceRegistry,
) -> std::result::Result<SourceDescriptor, String> {
    let source = match requested {
        Some(s) => s.clone(),
        None => {
            tracing::warn!(
                "session has no project source; using the deprecated deployment default"
            );
            registry
                .default_source()
                .ok_or("the session names no source and the deployment configures no default")?
        }
    };
    let provider = registry
        .for_source(&source)
        .map_err(|e| format!("project source cannot be served: {e}"))?;
    provider.validate(&source).map_err(|e| e.to_string())?;
    Ok(source)
}

#[allow(clippy::too_many_arguments)]
pub fn pod_template(
    cfg: &PodConfig,
    template: &SessionTemplate,
    id: Uuid,
    source: &SourceDescriptor,
    source_env: &[(String, String)],
    kind: &str,
    account: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    permission_mode: Option<&str>,
    codex_approval_policy: &str,
    command: &[String],
) -> Pod {
    let mut labels = BTreeMap::from([
        ("vogt.session".into(), id.to_string()),
        ("vogt.kind".into(), kind.into()),
        (
            "vogt.project".into(),
            sanitize_project_label(source.location.repo()),
        ),
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
    let mut env = pod_env(
        cfg,
        source,
        source_env,
        id,
        permission_mode,
        codex_approval_policy,
    );
    if let Some(model) = model {
        env.push(EnvVar {
            name: "VOGT_MODEL".into(),
            value: Some(model.into()),
            ..Default::default()
        });
    }
    // The session agent builds the pod's Claude line itself (WI-3881), so the
    // effort the engine validated has to travel with the pod the same way the
    // model does.
    if let Some(effort) = effort {
        env.push(EnvVar {
            name: "VOGT_EFFORT".into(),
            value: Some(effort.into()),
            ..Default::default()
        });
    }
    env.extend(PRIVATE_CACHE_ENV.iter().map(|(name, value)| EnvVar {
        name: (*name).into(),
        value: Some((*value).into()),
        ..Default::default()
    }));
    // An agent template (`vogt-agent-auth run -- claude`) names the pod's CLI
    // instead of its argv (WI-3169): the session agent builds that CLI's
    // command line itself, with the driven-session policy, the MCP set and
    // the conversation, which a raw argv would leave out. Without this every
    // pod ran the deployment's `cli` whatever the template asked for.
    let agent = crate::agent_cli::agent_name(command)
        .filter(|name| matches!(name.as_str(), "claude" | "codex" | "opencode"));
    if let Some(agent) = &agent {
        if let Some(cli) = env.iter_mut().find(|e| e.name == "VOGT_CLI") {
            cli.value = Some(agent.clone());
        }
    }
    // Any other requested argv runs in the PTY (the session agent reads it);
    // the container command stays the image entrypoint.
    if !command.is_empty() && agent.is_none() {
        env.push(EnvVar {
            name: "VOGT_SESSION_COMMAND".into(),
            value: Some(serde_json::to_string(command).expect("argv serialises")),
            ..Default::default()
        });
    }
    if let Some(endpoint) = &cfg.sccache_endpoint {
        env.push(EnvVar {
            name: "SCCACHE_ENDPOINT".into(),
            value: Some(endpoint.clone()),
            ..Default::default()
        });
    }
    let policy = cfg.template_policies.get(&template.name);
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
    // The workspace tree (WI-3373) is a pod-local emptyDir built by an init
    // container and mounted read-only here. The session's clone keeps its own
    // writable emptyDir at `<dir>/session`, a nested mount beneath the
    // read-only tree, so the workspace files are ancestors of the session's
    // cwd exactly as they are on a host.
    if let Some(workspace) = &cfg.workspace {
        mounts.push(VolumeMount {
            name: WORKSPACE_VOLUME.into(),
            mount_path: workspace.dir.clone(),
            read_only: Some(true),
            ..Default::default()
        });
        volumes.push(Volume {
            name: WORKSPACE_VOLUME.into(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Default::default()
        });
    }
    // The tool caches are private to the pod: an emptyDir that goes with it.
    // The shared per-node claim is mounted read-only beside it, never at the
    // path a tool writes to (WI-3242).
    mounts.push(VolumeMount {
        name: PRIVATE_CACHE_VOLUME.into(),
        mount_path: PRIVATE_CACHE_DIR.into(),
        ..Default::default()
    });
    volumes.push(Volume {
        name: PRIVATE_CACHE_VOLUME.into(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    });
    if !cfg.cache_pvc.is_empty() {
        mounts.push(VolumeMount {
            name: SHARED_CACHE_VOLUME.into(),
            mount_path: SHARED_CACHE_DIR.into(),
            read_only: Some(true),
            ..Default::default()
        });
        volumes.push(Volume {
            name: SHARED_CACHE_VOLUME.into(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: cfg.cache_pvc.clone(),
                read_only: Some(true),
            }),
            ..Default::default()
        });
    }
    // Host seeds are read-only at both the volume and the mount, so they can
    // never be a path between sessions.
    for (name, host, dir, env_name) in [
        (
            "cargo-seed",
            &cfg.cargo_seed_host_path,
            "/cache-ro/cargo-seed",
            "VOGT_CARGO_SEED_DIR",
        ),
        (
            "pnpm-seed",
            &cfg.pnpm_seed_host_path,
            "/cache-ro/pnpm-seed",
            "VOGT_PNPM_SEED_DIR",
        ),
    ] {
        let Some(host) = host else { continue };
        mounts.push(VolumeMount {
            name: name.into(),
            mount_path: dir.into(),
            read_only: Some(true),
            // Submounts under the seed are read-only too where the runtime
            // supports it; older ones ignore the request.
            recursive_read_only: Some("IfPossible".into()),
            ..Default::default()
        });
        volumes.push(Volume {
            name: name.into(),
            host_path: Some(HostPathVolumeSource {
                path: host.clone(),
                type_: Some("Directory".into()),
            }),
            ..Default::default()
        });
        env.push(EnvVar {
            name: env_name.into(),
            value: Some(dir.into()),
            ..Default::default()
        });
    }
    if !cfg.kache_config_map.is_empty() {
        mounts.push(VolumeMount {
            name: "kache-config".into(),
            mount_path: "/etc/kache".into(),
            read_only: Some(true),
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
    // The root filesystem is read-only (F5b): the writable paths the
    // entrypoint and the agents need are per-pod emptyDirs, never shared
    // storage. CODEX_HOME, CLAUDE_CONFIG_DIR and XDG_CONFIG_HOME default
    // under HOME, so the home mount covers them too.
    for (name, dir) in [
        ("session-home", HOME_MOUNT.to_string()),
        ("session-tmp", TMP_MOUNT.to_string()),
        ("session-work", cfg.work_dir()),
        ("session-run", RUNTIME_MOUNT.to_string()),
    ] {
        mounts.push(VolumeMount {
            name: name.into(),
            mount_path: dir,
            ..Default::default()
        });
        volumes.push(Volume {
            name: name.into(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
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
    // A session pod is bounded: the kubelet terminates it at its template's
    // policy or the deployment default, so a wedged agent cannot hold a node
    // until an operator notices. Zero in the deployment config leaves it unset.
    let active_deadline_seconds = policy.and_then(|p| p.active_deadline_seconds).or_else(|| {
        (!cfg.session_deadline.is_zero())
            .then(|| i64::try_from(cfg.session_deadline.as_secs()).unwrap_or(i64::MAX))
    });
    Pod {
        metadata: ObjectMeta {
            name: Some(pod_name(id)),
            namespace: Some(cfg.namespace.clone()),
            labels: Some(labels),
            annotations: Some(BTreeMap::from([(
                "vogt.project-url".into(),
                source_url(source).into(),
            )])),
            ..Default::default()
        },
        spec: Some(PodSpec {
            init_containers: cfg
                .workspace
                .as_ref()
                .map(|workspace| vec![workspace_init_container(workspace, image)]),
            containers: vec![Container {
                name: SESSION_CONTAINER.into(),
                image: Some(image.into()),
                env: Some(env),
                env_from: Some(env_from),
                volume_mounts: Some(mounts),
                resources: Some({
                    let requests = merge_resources(
                        &cfg.resource_requests,
                        policy.and_then(|p| p.resource_requests.as_ref()),
                    );
                    let limits = merge_resources(
                        &cfg.resource_limits,
                        policy.and_then(|p| p.resource_limits.as_ref()),
                    );
                    ResourceRequirements {
                        requests: (!requests.is_empty()).then_some(requests),
                        limits: (!limits.is_empty()).then_some(limits),
                        claims: None,
                    }
                }),
                security_context: Some(SecurityContext {
                    allow_privilege_escalation: Some(false),
                    read_only_root_filesystem: Some(true),
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
            node_selector: {
                let selector = merged_node_selector(cfg, policy);
                (!selector.is_empty()).then_some(selector)
            },
            affinity: merged_affinity(cfg, policy),
            tolerations: (!cfg.tolerations.is_empty()).then(|| cfg.tolerations.clone()),
            node_name: (!cfg.cache_node.is_empty()).then(|| cfg.cache_node.clone()),
            image_pull_secrets: cfg.image_pull_secret.as_ref().map(|name| {
                vec![k8s_openapi::api::core::v1::LocalObjectReference { name: name.clone() }]
            }),
            volumes: Some(volumes),
            priority_class_name: cfg.priority_class.clone(),
            active_deadline_seconds,
            restart_policy: Some("Never".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}
/// Builds the read-only workspace tree once per pod (WI-3373). It runs before
/// the session container and exits; only this container can reach the clone
/// sources, and both are public, so it needs no secret at all.
const WORKSPACE_INIT_SCRIPT: &str = r#"set -eu
umask 077
root=${WORKSPACE_ROOT:?WORKSPACE_ROOT is required}
mkdir -p "$root/session"
clone_at() {
  repo=$1; commit=$2; dir=$3
  rm -rf "$dir"
  git clone --quiet --no-checkout "$repo" "$dir"
  git -C "$dir" -c advice.detachedHead=false checkout --quiet "$commit"
  rm -rf "$dir/.git"
}
# Both repositories are public, so this clone needs no deploy key, token or
# Infisical identity: no credential exists that could reach the session.
if ! clone_at "$WORKSPACE_CONFIG_REPO" "$WORKSPACE_CONFIG_COMMIT" "$root/agent-workspace-config"; then
  echo 'workspace-init: could not clone the workspace configuration; continuing without it' >&2
  exit 0
fi
if ! clone_at "$WORKSPACE_SKILLS_REPO" "$WORKSPACE_SKILLS_COMMIT" "$root/UsefulClaudeSkills"; then
  echo 'workspace-init: could not clone the shared skills; continuing without them' >&2
  exit 0
fi
if ! WORKSPACE_ROOT="$root" "$root/agent-workspace-config/bootstrap/install.sh" --manifest=/dev/null; then
  echo 'workspace-init: install.sh failed; continuing without the workspace assets' >&2
  exit 0
fi
rm -rf "$root/agent-workspace-config"
printf 'workspace-init: workspace tree ready at %s
' "$root" >&2
"#;

fn workspace_init_container(workspace: &PodWorkspace, image: &str) -> Container {
    let env = |name: &str, value: &str| EnvVar {
        name: name.into(),
        value: Some(value.into()),
        ..Default::default()
    };
    Container {
        name: WORKSPACE_INIT_CONTAINER.into(),
        image: Some(image.into()),
        // The image is the session image, so git, python3 and the installer's
        // shell are the pinned ones, and the admission policy that pins the
        // session image covers this container too.
        command: Some(vec!["sh".into(), "-c".into()]),
        args: Some(vec![WORKSPACE_INIT_SCRIPT.into()]),
        env: Some(vec![
            env("WORKSPACE_ROOT", &workspace.dir),
            env("WORKSPACE_CONFIG_REPO", &workspace.config_repo),
            env("WORKSPACE_CONFIG_COMMIT", &workspace.config_commit),
            env("WORKSPACE_SKILLS_REPO", &workspace.skills_repo),
            env("WORKSPACE_SKILLS_COMMIT", &workspace.skills_commit),
        ]),
        volume_mounts: Some(vec![
            VolumeMount {
                name: WORKSPACE_VOLUME.into(),
                mount_path: workspace.dir.clone(),
                ..Default::default()
            },
            // Writable paths for git's global config; the container root is
            // read-only like the session container's.
            VolumeMount {
                name: "session-home".into(),
                mount_path: HOME_MOUNT.into(),
                ..Default::default()
            },
            VolumeMount {
                name: "session-tmp".into(),
                mount_path: TMP_MOUNT.into(),
                ..Default::default()
            },
        ]),
        resources: Some(ResourceRequirements {
            requests: Some(BTreeMap::from([
                (
                    "cpu".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("50m".into()),
                ),
                (
                    "memory".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("64Mi".into()),
                ),
                (
                    "ephemeral-storage".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("64Mi".into()),
                ),
            ])),
            limits: Some(BTreeMap::from([
                (
                    "cpu".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("500m".into()),
                ),
                (
                    "memory".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("256Mi".into()),
                ),
                (
                    "ephemeral-storage".into(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity("512Mi".into()),
                ),
            ])),
            claims: None,
        }),
        security_context: Some(SecurityContext {
            allow_privilege_escalation: Some(false),
            read_only_root_filesystem: Some(true),
            capabilities: Some(Capabilities {
                drop: Some(vec!["ALL".into()]),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct PodBackendFactory {
    pub cfg: Arc<PodConfig>,
    pub client: Client,
    /// The kache provisioner, when configured (WI-3242).
    pub cache: Option<Arc<dyn CacheProvisioner>>,
    /// Pod uid to the kache allocation it holds, so teardown can release it.
    /// Filled at create and from listed pods' labels (an adopted pod).
    allocations: Arc<dashmap::DashMap<String, String>>,
}
impl PodBackendFactory {
    pub async fn new(cfg: PodConfig) -> Result<Self> {
        let client = cfg.client().await?;
        let cache = HttpCacheProvisioner::from_config(&cfg)?
            .map(|p| Arc::new(p) as Arc<dyn CacheProvisioner>);
        Ok(Self::from_parts(Arc::new(cfg), client, cache))
    }
    pub fn from_parts(
        cfg: Arc<PodConfig>,
        client: Client,
        cache: Option<Arc<dyn CacheProvisioner>>,
    ) -> Self {
        Self {
            cfg,
            client,
            cache,
            allocations: Arc::default(),
        }
    }
    /// Release a kache allocation; a failure is logged, the provisioner's
    /// sweep is the backstop.
    async fn release_cache(&self, id: Uuid, allocation: &str) {
        let Some(cache) = &self.cache else { return };
        match cache.release(allocation).await {
            Ok(()) => tracing::info!(
                target: "vogt::audit", session_id = %id, allocation,
                "released the session's kache allocation"
            ),
            Err(e) => tracing::warn!(
                target: "vogt::audit", session_id = %id, allocation, error = %e,
                "could not release the session's kache allocation; the provisioner sweep will"
            ),
        }
    }
    pub async fn create(&self, pod: Pod) -> Result<Pod> {
        let shared = shared_writable_volumes(&pod);
        if !shared.is_empty() {
            return Err(ApiError::Internal(format!(
                "refusing to create a session pod with a shared writable volume: {}",
                shared.join(", ")
            )));
        }
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
        // The session's own kache bucket and key, allocated before the pod so
        // a refusal creates nothing. At the provisioner's cap the pod runs
        // with the shared read-only key only.
        let mut pod = pod;
        let allocation = match &self.cache {
            Some(cache) => {
                let shared = shared_writable_volumes(&pod);
                if !shared.is_empty() {
                    return Err(ApiError::Internal(format!(
                        "refusing to create a session pod with a shared writable volume: {}",
                        shared.join(", ")
                    )));
                }
                match cache.allocate().await? {
                    Some(alloc) => {
                        session_cache::attach(&mut pod, &alloc.allocation_id);
                        tracing::info!(
                            target: "vogt::audit", session_id = %id,
                            allocation = %alloc.allocation_id,
                            "allocated the session's kache bucket"
                        );
                        Some(alloc)
                    }
                    None => {
                        tracing::warn!(
                            session = %id,
                            "the kache provisioner is at its cap; this pod gets the shared \
                             read-only cache key only"
                        );
                        None
                    }
                }
            }
            None => None,
        };
        let auth = crate::agent_stream::global();
        auth.issue(id, token).await;
        let created = match self.create(pod).await {
            Ok(created) => created,
            Err(e) => {
                auth.withdraw(id).await;
                if let Some(alloc) = &allocation {
                    self.release_cache(id, &alloc.allocation_id).await;
                }
                return Err(e);
            }
        };
        let secret = bootstrap_secret(&self.cfg, id, &name, token, &created);
        let api = Api::<Secret>::namespaced(self.client.clone(), &self.cfg.namespace);
        let mut made = api
            .create(&PostParams::default(), &secret)
            .await
            .map(|_| ())
            .map_err(|e| format!("creating bootstrap secret: {e}"));
        if let (Ok(()), Some(alloc)) = (&made, &allocation) {
            let secret = session_cache::session_cache_secret(&self.cfg, id, alloc, &created);
            // The error names the Secret, never its data.
            made = api
                .create(&PostParams::default(), &secret)
                .await
                .map(|_| ())
                .map_err(|e| format!("creating session cache secret: {e}"));
        }
        if let Err(e) = made {
            // No pod will ever present the bootstrap credential.
            auth.withdraw(id).await;
            if let Err(delete_error) = self.delete(id, created.metadata.uid.clone()).await {
                tracing::warn!(
                    session = %id,
                    error = %delete_error,
                    "removing the pod after its bootstrap secret failed also failed; \
                     it may be left behind"
                );
            }
            if let Some(alloc) = &allocation {
                self.release_cache(id, &alloc.allocation_id).await;
            }
            return Err(ApiError::Internal(e));
        }
        if let (Some(alloc), Some(uid)) = (&allocation, &created.metadata.uid) {
            self.allocations
                .insert(uid.clone(), alloc.allocation_id.clone());
        }
        Ok(created)
    }

    /// Delete the session's pod. With `uid` the API server refuses (409) when
    /// the pod now under that name is a different one, so a decision made
    /// about an earlier pod cannot remove its replacement. The grace period is
    /// short (`POD_DELETE_GRACE_SECONDS`): the agent has already exited by the
    /// time the engine deletes, and the pod must leave `Terminating` quickly
    /// enough for a wake to create its replacement under the same name. A pod
    /// that is already gone is not an error.
    /// The pod's kache allocation, if it held one, is released once the pod
    /// is deleted (or already gone).
    pub async fn delete(&self, id: Uuid, uid: Option<String>) -> Result<()> {
        self.delete_pod(id, uid.clone()).await?;
        if let Some((_, allocation)) = uid.and_then(|uid| self.allocations.remove(&uid)) {
            self.release_cache(id, &allocation).await;
        }
        Ok(())
    }
    async fn delete_pod(&self, id: Uuid, uid: Option<String>) -> Result<()> {
        let params = DeleteParams {
            grace_period_seconds: Some(POD_DELETE_GRACE_SECONDS),
            preconditions: uid.map(|uid| Preconditions {
                uid: Some(uid),
                resource_version: None,
            }),
            ..Default::default()
        };
        match Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .delete(&pod_name(id), &params)
            .await
        {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(status)) if status.code == 404 => Ok(()),
            Err(e) => Err(ApiError::Internal(format!("deleting session pod: {e}"))),
        }
    }

    /// Whether the pod that carried `uid` has left the cluster: nothing is
    /// under the session's pod name, or what is there has a different uid (a
    /// replacement). Without a uid, only an empty name counts.
    pub async fn pod_gone(&self, id: Uuid, uid: Option<&str>) -> Result<bool> {
        let found = Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .get_opt(&pod_name(id))
            .await
            .map_err(|e| ApiError::Internal(format!("reading session pod: {e}")))?;
        Ok(match found {
            None => true,
            Some(pod) => uid.is_some_and(|uid| pod.metadata.uid.as_deref() != Some(uid)),
        })
    }
    pub async fn reconcile(&self) -> Result<Vec<Pod>> {
        let pods = Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .list(&ListParams::default().labels(&session_pod_selector(&self.cfg)))
            .await
            .map_err(|e| ApiError::Internal(format!("listing session pods: {e}")))?;
        // A pod adopted after an engine restart still holds its allocation.
        for pod in &pods.items {
            if let (Some(uid), Some(allocation)) =
                (&pod.metadata.uid, session_cache::allocation_of(pod))
            {
                self.allocations.entry(uid.clone()).or_insert(allocation);
            }
        }
        Ok(pods.items)
    }

    /// The last [`LOG_TAIL_LINES`] lines the session container wrote. A pod
    /// that exits without a message reports only "Error" in its status, so
    /// this is often the only place the cause of a failed session appears.
    pub async fn pod_log_tail(&self, id: Uuid) -> Result<String> {
        Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .logs(
                &pod_name(id),
                &LogParams {
                    container: Some(SESSION_CONTAINER.into()),
                    tail_lines: Some(LOG_TAIL_LINES),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| ApiError::Internal(format!("reading session pod logs: {e}")))
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
    /// Create the pod and return its uid. `token` is the one-shot bootstrap
    /// credential already registered with the stream; implementations only
    /// deliver it to the pod. The uid is what a later [`Self::delete`] and
    /// [`Self::gone`] name, so they apply to this pod and never its successor.
    fn create<'a>(&'a self, id: Uuid, token: &'a [u8], pod: Pod) -> CreateFuture<'a>;
    /// Delete the session's pod; `uid` (from the created or listed pod) makes
    /// the delete apply only to that exact pod.
    fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_>;
    /// Whether the pod carrying `uid` is gone from the cluster (absent, or
    /// replaced by a pod with another uid). A deleted pod lingers in
    /// `Terminating` until then and its name cannot be reused.
    fn gone(&self, id: Uuid, uid: Option<String>) -> GoneFuture<'_>;
    /// Every pod in the sessions namespace that carries a session label and
    /// this engine's `vogt.engine` label.
    fn list(&self) -> ListFuture<'_>;
    /// The tail of the session container's log, for a diagnosis the pod's
    /// status does not carry. A launcher with no logs returns an empty string.
    fn log_tail(&self, id: Uuid) -> LogFuture<'_>;
}
pub type LogFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>>;
pub type ListFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Pod>>> + Send + 'a>>;
pub type LaunchFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
pub type CreateFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<String>>> + Send + 'a>>;
pub type GoneFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>>;

impl PodLauncher for PodBackendFactory {
    fn create<'a>(&'a self, id: Uuid, token: &'a [u8], pod: Pod) -> CreateFuture<'a> {
        Box::pin(async move {
            self.create_resources(id, token, pod)
                .await
                .map(|created| created.metadata.uid)
        })
    }
    fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_> {
        Box::pin(PodBackendFactory::delete(self, id, uid))
    }
    fn gone(&self, id: Uuid, uid: Option<String>) -> GoneFuture<'_> {
        Box::pin(async move { self.pod_gone(id, uid.as_deref()).await })
    }
    fn list(&self) -> ListFuture<'_> {
        Box::pin(self.reconcile())
    }
    fn log_tail(&self, id: Uuid) -> LogFuture<'_> {
        Box::pin(PodBackendFactory::pod_log_tail(self, id))
    }
}

/// Delete the session pod that carries `uid` and wait, up to `limit`, until it
/// has left the cluster, so a wake that follows can create its replacement
/// under the same name. Returns whether the pod is confirmed gone.
///
/// The delete always carries the uid: a late teardown (a slow API, a retry
/// after a reconnect) must never remove the pod of a later incarnation. With
/// no uid there is nothing safe to name and the pod is left to the reconciler.
pub async fn teardown_pod(
    launcher: &dyn PodLauncher,
    id: Uuid,
    uid: Option<String>,
    limit: Duration,
    poll: Duration,
) -> bool {
    let Some(uid) = uid else {
        tracing::debug!(session = %id, "no pod uid is known for the session; not deleting by name");
        return true;
    };
    if let Err(e) = launcher.delete(id, Some(uid.clone())).await {
        tracing::debug!(session = %id, error = %e, "deleting the session pod failed");
        return false;
    }
    let wait = async {
        loop {
            match launcher.gone(id, Some(uid.clone())).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => {
                    tracing::debug!(session = %id, error = %e, "reading the deleted session pod failed")
                }
            }
            tokio::time::sleep(poll).await;
        }
    };
    let gone = tokio::time::timeout(limit, wait).await.is_ok();
    if !gone {
        tracing::warn!(session = %id, "the deleted session pod is still terminating");
    }
    gone
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
    fn create<'a>(&'a self, id: Uuid, token: &'a [u8], pod: Pod) -> CreateFuture<'a> {
        Box::pin(async move { PodLauncher::create(self.factory().await?, id, token, pod).await })
    }
    fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_> {
        Box::pin(async move { PodLauncher::delete(self.factory().await?, id, uid).await })
    }
    fn gone(&self, id: Uuid, uid: Option<String>) -> GoneFuture<'_> {
        Box::pin(async move { PodLauncher::gone(self.factory().await?, id, uid).await })
    }
    fn list(&self) -> ListFuture<'_> {
        Box::pin(async move { PodLauncher::list(self.factory().await?).await })
    }
    fn log_tail(&self, id: Uuid) -> LogFuture<'_> {
        Box::pin(async move { PodLauncher::log_tail(self.factory().await?, id).await })
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
        let (cols, rows) = *self.size.lock();
        Ok((rows, cols))
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

    fn project_source(repo: &str, reference: &str) -> SourceDescriptor {
        SourceDescriptor {
            location: SourceLocation::Git {
                url: format!("https://example.test/o/{repo}.git"),
                repo: repo.into(),
            },
            reference: reference.into(),
            landing: LandingMode::PushBranch,
        }
    }

    /// A provider with a deployment default and checkout env, standing in
    /// for whichever providers a deployment registers.
    struct StubProvider;

    impl crate::source_provider::SourceProvider for StubProvider {
        fn kind(&self) -> vogt_engine_contract::source::SourceKind {
            vogt_engine_contract::source::SourceKind::Git
        }
        fn validate(
            &self,
            source: &SourceDescriptor,
        ) -> std::result::Result<(), crate::source_provider::SourceError> {
            if source.location.repo().is_empty() || source.location.repo().starts_with('-') {
                return Err(crate::source_provider::SourceError::InvalidSource(
                    "bad repo".into(),
                ));
            }
            Ok(())
        }
        fn default_source(&self) -> Option<SourceDescriptor> {
            Some(project_source("vogt", "rust-2"))
        }
        fn pod_env(&self, source: &SourceDescriptor) -> Vec<(String, String)> {
            vec![("STUB_SOURCE_REPO".into(), source.location.repo().into())]
        }
        fn checkout(
            &self,
            _: &SourceDescriptor,
            _: &std::path::Path,
            _: Option<&crate::source_provider::CredentialHandle>,
        ) -> std::result::Result<(), crate::source_provider::SourceError> {
            unimplemented!()
        }
        fn mint_credential<'a>(
            &'a self,
            _: &'a SourceDescriptor,
            _: &'a crate::source_provider::CredentialRequest,
        ) -> crate::source_provider::SourceFuture<'a, crate::source_provider::CredentialHandle>
        {
            unimplemented!()
        }
        fn revoke_credential<'a>(
            &'a self,
            _: &'a crate::source_provider::CredentialHandle,
        ) -> crate::source_provider::SourceFuture<'a, ()> {
            unimplemented!()
        }
        fn status(
            &self,
            _: &std::path::Path,
        ) -> std::result::Result<
            vogt_engine_contract::source::SourceStatus,
            crate::source_provider::SourceError,
        > {
            unimplemented!()
        }
        fn diff(
            &self,
            _: &std::path::Path,
        ) -> std::result::Result<String, crate::source_provider::SourceError> {
            unimplemented!()
        }
        fn log(
            &self,
            _: &std::path::Path,
            _: usize,
        ) -> std::result::Result<
            Vec<vogt_engine_contract::source::SourceLogEntry>,
            crate::source_provider::SourceError,
        > {
            unimplemented!()
        }
        fn save_unlanded(
            &self,
            _: &std::path::Path,
            _: &str,
        ) -> std::result::Result<
            vogt_engine_contract::source::SavedWork,
            crate::source_provider::SourceError,
        > {
            unimplemented!()
        }
        fn land(
            &self,
            _: &SourceDescriptor,
            _: &std::path::Path,
            _: &crate::source_provider::LandRequest,
        ) -> std::result::Result<
            vogt_engine_contract::source::LandOutcome,
            crate::source_provider::SourceError,
        > {
            unimplemented!()
        }
        fn report_landed(
            &self,
            _: &SourceDescriptor,
            _: &str,
        ) -> std::result::Result<
            Vec<vogt_engine_contract::source::SourceLogEntry>,
            crate::source_provider::SourceError,
        > {
            unimplemented!()
        }
    }

    fn default_source(_cfg: &PodConfig) -> SourceDescriptor {
        session_source(None, &registry()).unwrap()
    }

    fn source_env(source: &SourceDescriptor) -> Vec<(String, String)> {
        use crate::source_provider::SourceProvider;
        StubProvider.pod_env(source)
    }

    fn registry() -> crate::source_provider::SourceRegistry {
        let mut r = crate::source_provider::SourceRegistry::default();
        r.register(Arc::new(StubProvider));
        r
    }

    fn env_value(pod: &Pod, name: &str) -> Option<String> {
        pod.spec.as_ref()?.containers[0]
            .env
            .as_ref()?
            .iter()
            .find(|v| v.name == name)?
            .value
            .clone()
    }

    #[test]
    fn session_source_defaults_to_the_deployment_default() {
        let s = session_source(None, &registry()).unwrap();
        assert_eq!(s.location.repo(), "vogt");
        assert_eq!(s.reference, "rust-2");
    }

    #[test]
    fn session_source_uses_the_project_source() {
        let s =
            session_source(Some(&project_source("farmeggs", "backlog-1")), &registry()).unwrap();
        assert_eq!(s.location.repo(), "farmeggs");
        assert_eq!(s.reference, "backlog-1");
    }

    #[test]
    fn session_source_serves_a_git_project_through_the_default_registry() {
        let r = crate::source_provider::SourceRegistry::with_defaults();
        let s = session_source(Some(&project_source("r", "main")), &r).unwrap();
        assert_eq!(s.kind(), vogt_engine_contract::source::SourceKind::Git);
    }

    #[test]
    fn session_source_refuses_what_the_provider_refuses() {
        for bad in [project_source("", "l"), project_source("-r", "l")] {
            assert!(session_source(Some(&bad), &registry()).is_err());
        }
    }

    #[test]
    fn session_source_refuses_a_provider_the_engine_lacks() {
        let r = crate::source_provider::SourceRegistry::default();
        assert!(session_source(Some(&project_source("r", "main")), &r).is_err());
        assert!(session_source(None, &r).is_err());
    }

    #[test]
    fn two_projects_render_their_own_repo_line_and_label() {
        let cfg = PodConfig::default();
        let template = SessionTemplate {
            name: "t".into(),
            description: "t".into(),
            command: None,
            cwd: None,
            env: vec![],
            default_name: None,
            match_repo_names: vec![],
            match_path_prefixes: vec![],
            tags: vec![],
            backend: "pod".into(),
            host_capable: false,
            pod_capable: false,
            account: None,
        };
        for (repo, line) in [("vogt", "rust-2"), ("farmeggs", "backlog-1")] {
            let src = session_source(Some(&project_source(repo, line)), &registry()).unwrap();
            let pod = pod_template(
                &cfg,
                &template,
                Uuid::nil(),
                &src,
                &source_env(&src),
                "session",
                None,
                None,
                None,
                None,
                vogt_session_agent::CODEX_APPROVAL_DEFAULT,
                &[],
            );
            let carried: SourceDescriptor =
                serde_json::from_str(&env_value(&pod, "VOGT_SOURCE_JSON").unwrap()).unwrap();
            assert_eq!(carried, src);
            assert_eq!(env_value(&pod, "STUB_SOURCE_REPO").as_deref(), Some(repo));
            assert_eq!(
                pod.metadata.labels.as_ref().unwrap().get("vogt.project"),
                Some(&repo.to_string())
            );
        }
    }

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
            host_capable: false,
            pod_capable: false,
            account: None,
        };
        let id = Uuid::nil();
        let pod = pod_template(
            &cfg,
            &template,
            id,
            &project_source("demo", "rust-2"),
            &[],
            "agent",
            Some("acct"),
            None,
            None,
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
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
            "VOGT_SOURCE_JSON",
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
        // /cache is a private emptyDir; the shared claim is read-only beside it.
        assert_eq!(mounts[0].name, "session-cache");
        assert_eq!(mounts[0].mount_path, "/cache");
        assert!(mounts[0].sub_path.is_none());
        assert_ne!(mounts[0].read_only, Some(true));
        assert_eq!(mounts[1].name, "session-cache-seed");
        assert_eq!(mounts[1].mount_path, "/cache-ro");
        assert_eq!(mounts[1].read_only, Some(true));
        assert_eq!(mounts[2].mount_path, "/etc/kache");
        let volumes = spec.volumes.as_ref().unwrap();
        assert!(volumes[0].empty_dir.is_some());
        assert!(volumes[0].persistent_volume_claim.is_none());
        let seed = volumes[1].persistent_volume_claim.as_ref().unwrap();
        assert_eq!(seed.claim_name, "session-cache-winrarhost1");
        assert_eq!(seed.read_only, Some(true));
        assert_eq!(volumes[2].config_map.as_ref().unwrap().name, "kache-config");
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
        // The root filesystem is read-only (F5b); every writable path the
        // entrypoint and the agents use is a per-pod emptyDir.
        assert_eq!(security.read_only_root_filesystem, Some(true));
        assert_eq!(spec.active_deadline_seconds, Some(12 * 60 * 60));
        for (name, path) in [
            ("session-home", "/home/vogt"),
            ("session-tmp", "/tmp"),
            ("session-work", "/work"),
            ("session-run", "/run/vogt"),
        ] {
            let mount = mounts.iter().find(|mount| mount.name == name).unwrap();
            assert_eq!(mount.mount_path, path);
            assert_ne!(mount.read_only, Some(true), "{name} must be writable");
            let volume = volumes.iter().find(|volume| volume.name == name).unwrap();
            assert!(volume.empty_dir.is_some(), "{name} must be an emptyDir");
        }
    }

    #[test]
    fn project_label_is_kubernetes_safe_and_url_is_an_annotation() {
        let raw = "https://github.com/TheDancingDeveloper-org/vogt";
        let safe = sanitize_project_label(raw);
        assert!(safe.len() <= 63);
        assert!(safe
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
        assert!(safe.chars().next().unwrap().is_ascii_alphanumeric());
        assert!(safe.chars().last().unwrap().is_ascii_alphanumeric());

        let pod = pod_template(
            &PodConfig::default(),
            &SessionTemplate {
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
                host_capable: false,
                pod_capable: false,
                account: None,
            },
            Uuid::nil(),
            &SourceDescriptor {
                location: SourceLocation::Git {
                    url: raw.into(),
                    repo: raw.into(),
                },
                reference: "main".into(),
                landing: LandingMode::PushBranch,
            },
            &[],
            "session",
            None,
            None,
            None,
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
            &[],
        );
        let metadata = pod.metadata;
        let labels = metadata.labels.unwrap();
        assert_eq!(labels["vogt.project"], safe);
        assert!(labels.values().all(|value| {
            value.len() <= 63
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                && value
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                && value
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
        }));
        assert_eq!(metadata.annotations.unwrap()["vogt.project-url"], raw);
    }

    #[test]
    fn project_label_sanitizing_handles_long_and_odd_inputs() {
        assert_eq!(sanitize_project_label("---...___"), "project");
        let safe = sanitize_project_label(&"a".repeat(100));
        assert_eq!(safe.len(), 63);
        assert_eq!(safe, "a".repeat(63));
        assert_eq!(sanitize_project_label("org/repo.git"), "org-repo.git");
    }

    #[test]
    fn pod_env_carries_the_providers_source_env() {
        let cfg = PodConfig::default();
        let src = default_source(&cfg);
        let env = pod_env(
            &cfg,
            &src,
            &source_env(&src),
            Uuid::nil(),
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
        );
        let stub = env.iter().find(|v| v.name == "STUB_SOURCE_REPO");
        assert_eq!(stub.and_then(|v| v.value.as_deref()), Some("vogt"));
        assert!(pod_env(
            &cfg,
            &src,
            &[],
            Uuid::nil(),
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
        )
        .iter()
        .all(|v| v.name != "STUB_SOURCE_REPO"));
    }

    #[test]
    fn pod_env_includes_optional_ca_contract() {
        let cfg = PodConfig {
            extra_ca_file: Some("/etc/ssl/custom.pem".into()),
            ..Default::default()
        };
        let env = pod_env(
            &cfg,
            &default_source(&cfg),
            &[],
            Uuid::nil(),
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
        );
        assert_eq!(
            env.iter()
                .find(|v| v.name == "VOGT_EXTRA_CA_FILE")
                .unwrap()
                .value
                .as_deref(),
            Some("/etc/ssl/custom.pem")
        );
    }

    #[test]
    fn pod_env_includes_configured_mcp_url() {
        let cfg = PodConfig {
            mcp_url: Some("http://vogt-engine.vogt.svc:8910/mcp".into()),
            ..Default::default()
        };
        let env = pod_env(
            &cfg,
            &default_source(&cfg),
            &[],
            Uuid::nil(),
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
        );
        assert_eq!(
            env.iter()
                .find(|variable| variable.name == "VOGT_MCP_URL")
                .and_then(|variable| variable.value.as_deref()),
            Some("http://vogt-engine.vogt.svc:8910/mcp")
        );
    }

    fn cache_env(pod: &Pod) -> BTreeMap<String, String> {
        pod.spec.as_ref().unwrap().containers[0]
            .env
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|v| Some((v.name.clone(), v.value.clone()?)))
            .collect()
    }

    #[test]
    fn tool_caches_are_pinned_inside_the_private_cache() {
        let pod = pod_template_for(&PodConfig::default());
        let env = cache_env(&pod);
        for name in [
            "CARGO_HOME",
            "XDG_CACHE_HOME",
            "NPM_CONFIG_CACHE",
            "npm_config_cache",
            "npm_config_store_dir",
            "PNPM_HOME",
            "PIP_CACHE_DIR",
            "UV_CACHE_DIR",
        ] {
            let value = env.get(name).unwrap_or_else(|| panic!("{name} not pinned"));
            assert!(
                value.starts_with("/cache/"),
                "{name}={value} is outside the private cache"
            );
            assert!(
                !value.starts_with(SHARED_CACHE_DIR),
                "{name}={value} is on the shared claim"
            );
        }
        // The container env is what wins over the cache ConfigMap's envFrom.
        assert!(pod.spec.unwrap().containers[0]
            .env_from
            .as_ref()
            .unwrap()
            .iter()
            .any(
                |e| e.config_map_ref.as_ref().map(|c| c.name.as_str()) == Some("session-cache-env")
            ));
    }

    #[test]
    fn kache_cache_env_is_secret_backed_and_configured_read_only() {
        let pod = pod_template_for(&PodConfig::default());
        let spec = pod.spec.as_ref().unwrap();
        let container = &spec.containers[0];
        let env_from = container.env_from.as_ref().unwrap();
        assert!(env_from.iter().any(|source| {
            source
                .config_map_ref
                .as_ref()
                .is_some_and(|reference| reference.name == "session-cache-env")
        }));
        assert!(env_from.iter().any(|source| {
            source
                .secret_ref
                .as_ref()
                .is_some_and(|reference| reference.name == "kache-s3")
        }));
        assert!(spec.volumes.as_ref().unwrap().iter().any(|volume| {
            volume
                .config_map
                .as_ref()
                .is_some_and(|config| config.name == "kache-config")
        }));
        let kache_mount = container
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|mount| mount.name == "kache-config")
            .unwrap();
        assert_eq!(kache_mount.mount_path, "/etc/kache");
        assert_eq!(kache_mount.read_only, Some(true));
    }

    #[test]
    fn two_sessions_share_no_writable_volume() {
        let cfg = PodConfig::default();
        let a = pod_for(&cfg, Uuid::from_u128(1));
        let b = pod_for(&cfg, Uuid::from_u128(2));
        assert!(shared_writable_volumes(&a).is_empty());
        assert!(shared_writable_volumes(&b).is_empty());
        // A writable mount is a per-pod emptyDir, an RW claim never.
        let writable = |pod: &Pod| -> Vec<(String, String)> {
            let spec = pod.spec.as_ref().unwrap();
            let volumes = spec.volumes.as_ref().unwrap();
            spec.containers[0]
                .volume_mounts
                .as_ref()
                .unwrap()
                .iter()
                .filter(|m| m.read_only != Some(true))
                .map(|m| {
                    let v = volumes.iter().find(|v| v.name == m.name).unwrap();
                    assert!(
                        v.empty_dir.is_some() || v.secret.is_some(),
                        "{} is writable and not private",
                        m.name
                    );
                    (m.mount_path.clone(), m.name.clone())
                })
                .collect()
        };
        let writable_a = writable(&a);
        assert!(writable_a.iter().any(|(path, _)| path == "/cache"));
        assert_eq!(writable_a, writable(&b));
        // Nothing the pods mount read-write is the same claim.
        let claims = |pod: &Pod| -> Vec<String> {
            pod.spec
                .as_ref()
                .unwrap()
                .volumes
                .as_ref()
                .unwrap()
                .iter()
                .filter_map(|v| v.persistent_volume_claim.as_ref())
                .filter(|c| c.read_only != Some(true))
                .map(|c| c.claim_name.clone())
                .collect()
        };
        assert!(claims(&a).is_empty() && claims(&b).is_empty());
    }

    #[test]
    fn a_shared_writable_volume_is_refused() {
        let mut pod = pod_template_for(&PodConfig::default());
        assert!(shared_writable_volumes(&pod).is_empty());
        // The old shape: the claim mounted read-write at /cache.
        {
            let spec = pod.spec.as_mut().unwrap();
            for v in spec.volumes.as_mut().unwrap() {
                if v.name == SHARED_CACHE_VOLUME {
                    v.persistent_volume_claim.as_mut().unwrap().read_only = Some(false);
                }
            }
        }
        assert_eq!(shared_writable_volumes(&pod), vec![SHARED_CACHE_VOLUME]);
        // Read-only on the volume but writable on the mount is also refused.
        let mut pod = pod_template_for(&PodConfig::default());
        for m in pod.spec.as_mut().unwrap().containers[0]
            .volume_mounts
            .as_mut()
            .unwrap()
        {
            if m.name == SHARED_CACHE_VOLUME {
                m.read_only = None;
            }
        }
        assert_eq!(shared_writable_volumes(&pod), vec![SHARED_CACHE_VOLUME]);
        // A host path is shared however it is mounted, unless the mount is read-only.
        let mut pod = pod_template_for(&PodConfig::default());
        let spec = pod.spec.as_mut().unwrap();
        spec.volumes.as_mut().unwrap().push(Volume {
            name: "host".into(),
            host_path: Some(k8s_openapi::api::core::v1::HostPathVolumeSource {
                path: "/var/cache".into(),
                type_: None,
            }),
            ..Default::default()
        });
        spec.containers[0]
            .volume_mounts
            .as_mut()
            .unwrap()
            .push(VolumeMount {
                name: "host".into(),
                mount_path: "/host".into(),
                ..Default::default()
            });
        assert_eq!(shared_writable_volumes(&pod), vec!["host"]);
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
                    if parts.method == hyper::Method::GET && parts.uri.path().contains("/pods/") {
                        let name = parts.uri.path().rsplit('/').next().unwrap();
                        let found = state
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|p| p.metadata.name.as_deref() == Some(name))
                            .cloned();
                        return match found {
                            Some(pod) => json(200, serde_json::to_value(pod).unwrap()),
                            None => json(
                                404,
                                serde_json::json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"NotFound","code":404,"message":"not found"}),
                            ),
                        };
                    }
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

    /// WI-3169: an agent template's argv picks the pod's CLI, and the
    /// session agent builds the command line, so the posture is not lost.
    #[test]
    fn an_agent_template_command_names_the_pod_cli() {
        let cfg = PodConfig::default();
        let template = SessionTemplate {
            name: "Claude Code (protected)".into(),
            description: "claude".into(),
            command: None,
            cwd: None,
            env: vec![],
            default_name: None,
            match_repo_names: vec![],
            match_path_prefixes: vec![],
            tags: vec![],
            backend: "pod".into(),
            host_capable: false,
            pod_capable: false,
            account: None,
        };
        let env_of = |command: &[String]| {
            let pod = pod_template(
                &cfg,
                &template,
                Uuid::nil(),
                &default_source(&cfg),
                &[],
                "session",
                None,
                None,
                None,
                None,
                vogt_session_agent::CODEX_APPROVAL_DEFAULT,
                command,
            );
            pod.spec.unwrap().containers[0]
                .env
                .clone()
                .unwrap()
                .into_iter()
                .map(|e| (e.name, e.value.unwrap_or_default()))
                .collect::<BTreeMap<_, _>>()
        };
        let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();

        let env = env_of(&argv(&["vogt-agent-auth", "run", "--", "claude"]));
        assert_eq!(env["VOGT_CLI"], "claude");
        assert!(!env.contains_key("VOGT_SESSION_COMMAND"));

        let env = env_of(&argv(&["opencode"]));
        assert_eq!(env["VOGT_CLI"], "opencode");

        let env = env_of(&[]);
        assert_eq!(
            env["VOGT_CLI"], cfg.cli,
            "no command keeps the deployment CLI"
        );

        let env = env_of(&argv(&["bash", "-l"]));
        assert_eq!(env["VOGT_CLI"], cfg.cli);
        assert_eq!(env["VOGT_SESSION_COMMAND"], r#"["bash","-l"]"#);
    }

    /// WI-3864, WI-3881: the pod's session agent builds the agent line itself,
    /// so the launch inputs the engine validated have to reach it in the env.
    /// The default posture is left out so the pod's own default applies.
    #[test]
    fn a_pod_carries_the_codex_posture_the_engine_validated() {
        let cfg = PodConfig::default();
        let template = SessionTemplate {
            name: "Codex (protected)".into(),
            description: "codex".into(),
            command: None,
            cwd: None,
            env: vec![],
            default_name: None,
            match_repo_names: vec![],
            match_path_prefixes: vec![],
            tags: vec![],
            backend: "pod".into(),
            host_capable: false,
            pod_capable: false,
            account: None,
        };
        let env_of =
            |model: Option<&str>, effort: Option<&str>, mode: Option<&str>, approval: &str| {
                let pod = pod_template(
                    &cfg,
                    &template,
                    Uuid::nil(),
                    &default_source(&cfg),
                    &[],
                    "session",
                    None,
                    model,
                    effort,
                    mode,
                    approval,
                    &[
                        "vogt-agent-auth".into(),
                        "run".into(),
                        "--".into(),
                        "codex".into(),
                    ],
                );
                pod.spec.unwrap().containers[0]
                    .env
                    .clone()
                    .unwrap()
                    .into_iter()
                    .map(|e| (e.name, e.value.unwrap_or_default()))
                    .collect::<BTreeMap<_, _>>()
            };

        let env = env_of(None, None, None, "never");
        assert_eq!(env["VOGT_CODEX_APPROVAL_POLICY"], "never");
        assert!(!env.contains_key("VOGT_MODEL"));
        assert!(!env.contains_key("VOGT_EFFORT"));
        assert!(
            !env.contains_key("VOGT_PERMISSION_MODE"),
            "the default posture is the pod's own default"
        );

        let env = env_of(
            Some("claude-opus-4-6"),
            Some("high"),
            Some("accept-edits"),
            "on-request",
        );
        assert_eq!(env["VOGT_MODEL"], "claude-opus-4-6");
        assert_eq!(env["VOGT_EFFORT"], "high");
        assert_eq!(env["VOGT_PERMISSION_MODE"], "accept-edits");
        assert_eq!(env["VOGT_CODEX_APPROVAL_POLICY"], "on-request");

        let env = env_of(None, None, Some("default"), "never");
        assert!(!env.contains_key("VOGT_PERMISSION_MODE"));
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
            host_capable: false,
            pod_capable: false,
            account: None,
        };
        let pod = pod_template(
            &cfg,
            &template,
            Uuid::nil(),
            &project_source("demo", "rust-2"),
            &[],
            "session",
            None,
            None,
            None,
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
            &[],
        );
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
        let factory = PodBackendFactory::from_parts(Arc::new(cfg), client, None);
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
        let factory = PodBackendFactory::from_parts(Arc::new(PodConfig::default()), client, None);

        assert!(factory.delete(id, Some("uid-old".into())).await.is_err());
        assert_eq!(store.lock().unwrap().len(), 1, "the new pod survives");
        let sent = seen.lock().unwrap()[0].2.clone();
        assert!(sent.contains("\"uid\":\"uid-old\""), "body: {sent}");

        // With the uid of the pod that is there, the delete goes through.
        factory.delete(id, Some("uid-new".into())).await.unwrap();
        assert!(store.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_pod_delete_has_a_short_grace_and_a_missing_pod_is_not_an_error() {
        let id = Uuid::new_v4();
        let (client, seen, store) = mock_kube::client(vec![named(id, DEFAULT_ENGINE_ID, "uid-1")]);
        let factory = PodBackendFactory::from_parts(Arc::new(PodConfig::default()), client, None);
        factory.delete(id, Some("uid-1".into())).await.unwrap();
        assert!(store.lock().unwrap().is_empty());
        let sent = seen.lock().unwrap()[0].2.clone();
        assert!(
            sent.contains(&format!(
                "\"gracePeriodSeconds\":{POD_DELETE_GRACE_SECONDS}"
            )),
            "a delete without a grace period leaves the pod Terminating for 30 s: {sent}"
        );
        // Deleting what is already gone succeeds.
        factory.delete(id, Some("uid-1".into())).await.unwrap();
    }

    #[tokio::test]
    async fn a_pod_is_gone_when_absent_or_replaced_by_another_uid() {
        let id = Uuid::new_v4();
        let (client, _, store) = mock_kube::client(vec![named(id, DEFAULT_ENGINE_ID, "uid-new")]);
        let factory = PodBackendFactory::from_parts(Arc::new(PodConfig::default()), client, None);
        assert!(!factory.pod_gone(id, Some("uid-new")).await.unwrap());
        assert!(!factory.pod_gone(id, None).await.unwrap());
        assert!(factory.pod_gone(id, Some("uid-old")).await.unwrap());
        store.lock().unwrap().clear();
        assert!(factory.pod_gone(id, Some("uid-new")).await.unwrap());
        assert!(factory.pod_gone(id, None).await.unwrap());
    }

    /// A launcher that records deletes and reports the pod gone after
    /// `polls_until_gone` questions (or never, with `None`).
    struct Lingering {
        deletes: std::sync::Mutex<Vec<(Uuid, Option<String>)>>,
        asked: std::sync::atomic::AtomicUsize,
        polls_until_gone: Option<usize>,
    }
    impl PodLauncher for Lingering {
        fn create<'a>(&'a self, _: Uuid, _: &'a [u8], _: Pod) -> CreateFuture<'a> {
            Box::pin(async { Ok(None) })
        }
        fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_> {
            Box::pin(async move {
                self.deletes.lock().unwrap().push((id, uid));
                Ok(())
            })
        }
        fn gone(&self, _: Uuid, _: Option<String>) -> GoneFuture<'_> {
            Box::pin(async move {
                let asked = self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                Ok(self.polls_until_gone.is_some_and(|n| asked >= n))
            })
        }
        fn list(&self) -> ListFuture<'_> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn log_tail(&self, _: Uuid) -> LogFuture<'_> {
            Box::pin(async { Ok(String::new()) })
        }
    }
    fn lingering(polls_until_gone: Option<usize>) -> Lingering {
        Lingering {
            deletes: Default::default(),
            asked: Default::default(),
            polls_until_gone,
        }
    }

    #[tokio::test]
    async fn teardown_deletes_by_uid_and_waits_until_the_pod_is_gone() {
        let id = Uuid::new_v4();
        let launcher = lingering(Some(3));
        let gone = teardown_pod(
            &launcher,
            id,
            Some("uid-1".into()),
            Duration::from_secs(5),
            Duration::from_millis(5),
        )
        .await;
        assert!(gone);
        assert_eq!(
            *launcher.deletes.lock().unwrap(),
            vec![(id, Some("uid-1".to_string()))]
        );
        assert_eq!(launcher.asked.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn teardown_is_bounded_when_the_pod_never_goes() {
        let launcher = lingering(None);
        let started = std::time::Instant::now();
        let gone = teardown_pod(
            &launcher,
            Uuid::new_v4(),
            Some("uid-1".into()),
            Duration::from_millis(100),
            Duration::from_millis(10),
        )
        .await;
        assert!(!gone);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn teardown_never_deletes_a_pod_by_name_alone() {
        let launcher = lingering(Some(1));
        let gone = teardown_pod(
            &launcher,
            Uuid::new_v4(),
            None,
            Duration::from_secs(1),
            Duration::from_millis(5),
        )
        .await;
        assert!(gone);
        assert!(launcher.deletes.lock().unwrap().is_empty());
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
    async fn a_pod_with_a_shared_writable_volume_is_never_sent_to_the_cluster() {
        let id = Uuid::new_v4();
        let cfg = PodConfig::default();
        let (client, seen) = mock_create::client(vec![]);
        let factory = PodBackendFactory::from_parts(Arc::new(cfg.clone()), client, None);
        let mut pod = pod_for(&cfg, id);
        for v in pod.spec.as_mut().unwrap().volumes.as_mut().unwrap() {
            if let Some(pvc) = v.persistent_volume_claim.as_mut() {
                pvc.read_only = Some(false);
            }
        }
        let err = factory
            .create_resources(id, b"t", pod)
            .await
            .expect_err("a shared writable claim must be refused");
        assert!(err.to_string().contains("shared writable volume"), "{err}");
        assert!(seen.lock().unwrap().is_empty(), "the cluster was called");
    }

    #[tokio::test]
    async fn the_bootstrap_secret_is_created_owned_by_the_pod_and_the_spec_has_no_token() {
        let id = Uuid::new_v4();
        let cfg = PodConfig {
            engine_id: "vogt-dev".into(),
            ..Default::default()
        };
        let (client, seen) = mock_create::client(vec![]);
        let factory = PodBackendFactory::from_parts(Arc::new(cfg.clone()), client, None);
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
        let factory = PodBackendFactory::from_parts(Arc::new(cfg.clone()), client, None);
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
    async fn a_bootstrap_token_whose_pod_was_not_made_is_withdrawn() {
        let id = Uuid::new_v4();
        let cfg = PodConfig::default();
        let (client, _seen) = mock_create::client(vec![403]);
        let factory = PodBackendFactory::from_parts(Arc::new(cfg.clone()), client, None);
        assert!(factory
            .create_resources(id, b"unused-token", pod_for(&cfg, id))
            .await
            .is_err());
        assert!(
            !crate::agent_stream::global()
                .consume(id, b"unused-token")
                .await,
            "the token outlived the failed pod create"
        );
    }

    #[tokio::test]
    async fn each_pod_gets_its_own_secret_name_and_the_engine_never_deletes_a_secret() {
        let id = Uuid::new_v4();
        let cfg = PodConfig::default();
        let (client, seen) = mock_create::client(vec![]);
        let factory = PodBackendFactory::from_parts(Arc::new(cfg.clone()), client, None);
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
        let factory = PodBackendFactory::from_parts(Arc::new(cfg.clone()), client, None);
        assert!(factory
            .create_resources(id, b"t", pod_for(&cfg, id))
            .await
            .is_err());
        let seen = seen.lock().unwrap();
        let deletes: Vec<_> = seen.iter().filter(|(m, _, _)| m == "DELETE").collect();
        assert_eq!(deletes.len(), 1);
        assert!(deletes[0].1.contains("/pods/"), "{}", deletes[0].1);
    }

    fn cached_factory(
        secret_posts: Vec<u16>,
        prov: Arc<crate::session_cache::tests::FakeProvisioner>,
    ) -> (PodBackendFactory, mock_create::Seen, PodConfig) {
        let cfg = PodConfig::default();
        let (client, seen) = mock_create::client(secret_posts);
        let factory = PodBackendFactory::from_parts(
            Arc::new(cfg.clone()),
            client,
            Some(prov as Arc<dyn CacheProvisioner>),
        );
        (factory, seen, cfg)
    }

    #[tokio::test]
    async fn a_pod_gets_its_own_kache_key_in_an_owned_secret_and_releases_it_at_delete() {
        use crate::session_cache::tests::{FakeProvisioner, ALLOC_ID};
        let id = Uuid::new_v4();
        let prov = Arc::new(FakeProvisioner::default());
        let (factory, seen, cfg) = cached_factory(vec![], prov.clone());
        let created = factory
            .create_resources(id, b"t", pod_for(&cfg, id))
            .await
            .unwrap();
        {
            let seen = seen.lock().unwrap();
            let paths: Vec<_> = seen.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
            assert_eq!(
                paths,
                [
                    "POST /api/v1/namespaces/vogt-sessions/pods",
                    "POST /api/v1/namespaces/vogt-sessions/secrets",
                    "POST /api/v1/namespaces/vogt-sessions/secrets",
                ]
            );
            let pod: Pod = serde_json::from_str(&seen[0].2).unwrap();
            assert!(!seen[0].2.contains("s3ssion-secret"), "key in the pod spec");
            assert_eq!(
                pod.metadata.labels.as_ref().unwrap()[crate::session_cache::ALLOCATION_LABEL],
                ALLOC_ID
            );
            let env_from = pod.spec.unwrap().containers[0].env_from.clone().unwrap();
            let last = env_from.last().unwrap().secret_ref.as_ref().unwrap();
            assert_eq!(last.name, format!("vogt-session-cache-{ALLOC_ID}"));
            let secret: Secret = serde_json::from_str(&seen[2].2).unwrap();
            assert_eq!(secret.metadata.name.as_deref(), Some(last.name.as_str()));
            assert_eq!(secret.metadata.owner_references.unwrap()[0].uid, "uid-pod");
        }
        assert!(prov.released.lock().unwrap().is_empty());
        factory.delete(id, created.metadata.uid).await.unwrap();
        assert_eq!(*prov.released.lock().unwrap(), [ALLOC_ID]);
    }

    #[tokio::test]
    async fn a_failed_cache_secret_deletes_the_pod_then_releases_the_allocation() {
        use crate::session_cache::tests::{FakeProvisioner, ALLOC_ID};
        let id = Uuid::new_v4();
        let prov = Arc::new(FakeProvisioner::default());
        // The bootstrap Secret is made; the cache Secret is refused.
        let (factory, seen, cfg) = cached_factory(vec![201, 403], prov.clone());
        let err = factory
            .create_resources(id, b"t", pod_for(&cfg, id))
            .await
            .expect_err("no pod without its cache secret");
        assert!(!err.to_string().contains("s3ssion"), "{err}");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.last().unwrap().0, "DELETE");
        assert!(seen.last().unwrap().1.contains("/pods/"));
        assert_eq!(*prov.released.lock().unwrap(), [ALLOC_ID]);
    }

    #[tokio::test]
    async fn a_refused_allocation_creates_nothing_and_the_cap_runs_without_a_bucket() {
        use crate::session_cache::tests::FakeProvisioner;
        let id = Uuid::new_v4();
        let prov = FakeProvisioner::answering(Err(ApiError::Internal("HTTP 403".into())));
        let (factory, seen, cfg) = cached_factory(vec![], prov.clone());
        assert!(factory
            .create_resources(id, b"t", pod_for(&cfg, id))
            .await
            .is_err());
        assert!(seen.lock().unwrap().is_empty(), "the cluster was called");

        let prov = FakeProvisioner::answering(Ok(None));
        let (factory, seen, cfg) = cached_factory(vec![], prov.clone());
        factory
            .create_resources(id, b"t", pod_for(&cfg, id))
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "pod and bootstrap secret only: {seen:?}");
        let pod: Pod = serde_json::from_str(&seen[0].2).unwrap();
        assert!(crate::session_cache::allocation_of(&pod).is_none());
    }

    #[tokio::test]
    async fn an_adopted_pods_allocation_is_released_at_its_teardown() {
        use crate::session_cache::tests::{FakeProvisioner, ALLOC_ID};
        let id = Uuid::new_v4();
        let mut pod = named(id, DEFAULT_ENGINE_ID, "uid-1");
        crate::session_cache::attach(&mut pod, ALLOC_ID);
        let (client, _seen, _store) = mock_kube::client(vec![pod]);
        let prov = Arc::new(FakeProvisioner::default());
        let factory = PodBackendFactory::from_parts(
            Arc::new(PodConfig::default()),
            client,
            Some(prov.clone() as Arc<dyn CacheProvisioner>),
        );
        factory.reconcile().await.unwrap();
        factory.delete(id, Some("uid-1".into())).await.unwrap();
        assert_eq!(*prov.released.lock().unwrap(), [ALLOC_ID]);
    }

    #[test]
    fn the_provisioner_url_is_read_and_must_be_https() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_PROVISIONER_URL", "https://prov.svc:8443"),
                ("ENGINE_POD_CACHE_PROVISIONER_TOKEN_FILE", "/run/prov/token"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.cache_provisioner_url.as_deref(),
            Some("https://prov.svc:8443")
        );
        assert_eq!(
            cfg.cache_provisioner_token_file.as_deref(),
            Some("/run/prov/token")
        );
        assert!(PodConfig::default().cache_provisioner_url.is_none());
        assert!(PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_CACHE_PROVISIONER_URL", "http://prov.svc")]),
            None
        )
        .is_err());
    }

    const CONFIG_COMMIT: &str = "f0115632e9c789e585606f156b8c5c621a04c258";
    const SKILLS_COMMIT: &str = "d93c350828148975a273132242dae935362f4145";

    fn workspace_cfg() -> PodConfig {
        PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_WORKSPACE", "1"),
                ("ENGINE_POD_WORKSPACE_CONFIG_COMMIT", CONFIG_COMMIT),
                ("ENGINE_POD_WORKSPACE_SKILLS_COMMIT", SKILLS_COMMIT),
            ]),
            None,
        )
        .unwrap()
    }

    #[test]
    fn the_workspace_is_off_by_default_and_pinned_when_on() {
        let cfg = PodConfig::default();
        assert!(cfg.workspace.is_none());
        assert_eq!(cfg.work_dir(), vogt_session_agent::DEFAULT_WORK_DIR);

        let cfg = workspace_cfg();
        let workspace = cfg.workspace.as_ref().unwrap();
        assert_eq!(workspace.dir, DEFAULT_WORKSPACE_DIR);
        assert_eq!(workspace.config_repo, DEFAULT_WORKSPACE_CONFIG_REPO);
        assert_eq!(workspace.config_commit, CONFIG_COMMIT);
        assert_eq!(workspace.skills_repo, DEFAULT_WORKSPACE_SKILLS_REPO);
        assert_eq!(workspace.skills_commit, SKILLS_COMMIT);
        assert_eq!(cfg.work_dir(), "/home/vogt/Working/session");

        let enabled = |pairs: &[(&str, &str)]| {
            let mut all = vec![
                ("ENGINE_POD_WORKSPACE", "true"),
                ("ENGINE_POD_WORKSPACE_CONFIG_COMMIT", CONFIG_COMMIT),
                ("ENGINE_POD_WORKSPACE_SKILLS_COMMIT", SKILLS_COMMIT),
            ];
            all.extend_from_slice(pairs);
            PodConfig::from_lookup(lookup(&all), None)
        };
        // A branch is not a pin.
        let error = enabled(&[("ENGINE_POD_WORKSPACE_CONFIG_COMMIT", "main")]).unwrap_err();
        assert!(error.to_string().contains("40-character"), "{error}");
        assert!(enabled(&[("ENGINE_POD_WORKSPACE_CONFIG_COMMIT", "")]).is_err());
        assert!(enabled(&[("ENGINE_POD_WORKSPACE_SKILLS_COMMIT", "rust-2")]).is_err());
        // The mount must stay under the session home.
        assert!(enabled(&[("ENGINE_POD_WORKSPACE_DIR", "/etc/workspace")]).is_err());
        assert!(enabled(&[("ENGINE_POD_WORKSPACE_DIR", "/home/vogt")]).is_err());
        assert!(enabled(&[("ENGINE_POD_WORKSPACE_DIR", "/home/vogt/../etc")]).is_err());
        let moved = enabled(&[("ENGINE_POD_WORKSPACE_DIR", "/home/vogt/tree/")]).unwrap();
        assert_eq!(moved.work_dir(), "/home/vogt/tree/session");
        // An unusable flag value is refused, not read as off.
        assert!(PodConfig::from_lookup(lookup(&[("ENGINE_POD_WORKSPACE", "yes")]), None).is_err());
    }

    #[test]
    fn a_workspace_pod_builds_a_read_only_tree_without_credentials() {
        let cfg = workspace_cfg();
        let pod = pod_template_for(&cfg);
        let spec = pod.spec.as_ref().unwrap();
        let init = spec.init_containers.as_ref().unwrap();
        assert_eq!(init.len(), 1);
        let workspace_init = &init[0];
        assert_eq!(workspace_init.name, WORKSPACE_INIT_CONTAINER);
        // The session image and its digest pin, which the namespace's
        // admission policy checks for every container.
        assert_eq!(
            workspace_init.image.as_deref(),
            Some(cfg.default_image.as_str())
        );
        assert_eq!(
            workspace_init
                .security_context
                .as_ref()
                .unwrap()
                .read_only_root_filesystem,
            Some(true)
        );
        // It builds the tree, so it writes the workspace volume...
        let init_mount = workspace_init
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.name == WORKSPACE_VOLUME)
            .unwrap();
        assert_eq!(init_mount.mount_path, DEFAULT_WORKSPACE_DIR);
        assert_eq!(init_mount.read_only, None);
        // ...and names the pins as data, never a credential.
        let init_env: BTreeMap<_, _> = workspace_init
            .env
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| (e.name.as_str(), e.value.clone().unwrap_or_default()))
            .collect();
        assert_eq!(init_env["WORKSPACE_CONFIG_COMMIT"], CONFIG_COMMIT);
        assert_eq!(init_env["WORKSPACE_SKILLS_COMMIT"], SKILLS_COMMIT);
        assert!(init_env
            .values()
            .all(|v| !v.contains("TOKEN") && !v.contains("KEY")));

        // The session container reads it, never writes it.
        let container = &spec.containers[0];
        let mounts = container.volume_mounts.as_ref().unwrap();
        let mount = mounts.iter().find(|m| m.name == WORKSPACE_VOLUME).unwrap();
        assert_eq!(mount.mount_path, DEFAULT_WORKSPACE_DIR);
        assert_eq!(mount.read_only, Some(true));
        let work = mounts.iter().find(|m| m.name == "session-work").unwrap();
        assert_eq!(work.mount_path, "/home/vogt/Working/session");
        assert_ne!(work.read_only, Some(true));
        // The workspace volume is pod-local: never a claim, host path or NFS
        // share another session could also mount.
        let volume = spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == WORKSPACE_VOLUME)
            .unwrap();
        assert!(volume.empty_dir.is_some());
        assert!(shared_writable_volumes(&pod).is_empty());

        // The clone root and the guidance root travel to the session.
        let env: BTreeMap<_, _> = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| (e.name.as_str(), e.value.clone().unwrap_or_default()))
            .collect();
        assert_eq!(env["VOGT_CLONE_DIR"], "/home/vogt/Working/session");
        assert_eq!(env["VOGT_WORK_DIR"], "/home/vogt/Working/session");
        assert_eq!(env["VOGT_WORKSPACE_ROOT"], DEFAULT_WORKSPACE_DIR);
        assert!(!env.contains_key("VOGT_BOOTSTRAP_TOKEN"));

        // No new Secret: the only secret volume is the bootstrap credential
        // file, mounted into the session container as before.
        let secret_volumes: Vec<_> = spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|v| v.secret.as_ref().and_then(|s| s.secret_name.as_deref()))
            .collect();
        assert_eq!(secret_volumes.len(), 1);
        assert!(secret_volumes[0].ends_with("-bootstrap"));
    }

    #[test]
    fn a_pod_without_the_workspace_flag_keeps_the_old_spec() {
        let cfg = PodConfig::default();
        let pod = pod_template_for(&cfg);
        let spec = pod.spec.as_ref().unwrap();
        assert!(spec.init_containers.is_none());
        let container = &spec.containers[0];
        assert!(!container
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .any(|m| m.name == WORKSPACE_VOLUME));
        let work = container
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.name == "session-work")
            .unwrap();
        assert_eq!(work.mount_path, vogt_session_agent::DEFAULT_WORK_DIR);
        let env: BTreeMap<_, _> = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| (e.name.as_str(), e.value.clone().unwrap_or_default()))
            .collect();
        assert_eq!(env["VOGT_CLONE_DIR"], vogt_session_agent::DEFAULT_WORK_DIR);
        assert!(!env.contains_key("VOGT_WORKSPACE_ROOT"));
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
            host_capable: false,
            pod_capable: false,
            account: None,
        };
        pod_template(
            cfg,
            &template,
            Uuid::nil(),
            &default_source(cfg),
            &[],
            "session",
            None,
            None,
            None,
            None,
            vogt_session_agent::CODEX_APPROVAL_DEFAULT,
            &[],
        )
    }

    #[test]
    fn active_deadline_follows_the_template_policy_and_zero_disables_it() {
        let mut cfg = PodConfig {
            session_deadline: Duration::from_secs(3600),
            ..Default::default()
        };
        assert_eq!(
            pod_template_for(&cfg).spec.unwrap().active_deadline_seconds,
            Some(3600)
        );
        cfg.template_policies.insert(
            "Pod".into(),
            PodTemplatePolicy {
                active_deadline_seconds: Some(900),
                ..Default::default()
            },
        );
        assert_eq!(
            pod_template_for(&cfg).spec.unwrap().active_deadline_seconds,
            Some(900)
        );
        cfg.template_policies.clear();
        cfg.session_deadline = Duration::ZERO;
        assert_eq!(
            pod_template_for(&cfg).spec.unwrap().active_deadline_seconds,
            None
        );
    }

    #[test]
    fn the_session_deadline_is_read_from_the_environment_and_must_be_positive() {
        let cfg = PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_SESSION_DEADLINE_SECONDS", "7200")]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.session_deadline, Duration::from_secs(7200));
        let cfg = PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_SESSION_DEADLINE_SECONDS", "0")]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.session_deadline, Duration::ZERO);
        assert!(PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_SESSION_DEADLINE_SECONDS", "soon")]),
            None
        )
        .is_err());
        assert!(PodConfig::from_lookup(
            lookup(&[(
                "ENGINE_POD_TEMPLATE_POLICIES",
                r#"{"Pod":{"active_deadline_seconds":0}}"#,
            )]),
            None
        )
        .is_err());
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
        assert_eq!(cfg.session_deadline, Duration::from_secs(12 * 60 * 60));
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
    fn pod_timeouts_read_from_the_environment() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CONNECT_TIMEOUT", "11"),
                ("ENGINE_POD_PENDING_TIMEOUT", "22"),
                ("ENGINE_POD_RECONNECT_TIMEOUT", "33"),
                ("ENGINE_POD_RECONCILE_TIMEOUT", "44"),
                ("ENGINE_POD_HEARTBEAT_TIMEOUT", "55"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.connect_timeout, Duration::from_secs(11));
        assert_eq!(cfg.pending_timeout, Duration::from_secs(22));
        assert_eq!(cfg.reconnect_timeout, Duration::from_secs(33));
        assert_eq!(cfg.reconcile_interval, Duration::from_secs(44));
        assert_eq!(cfg.heartbeat_timeout, Duration::from_secs(55));

        for name in [
            "ENGINE_POD_CONNECT_TIMEOUT",
            "ENGINE_POD_PENDING_TIMEOUT",
            "ENGINE_POD_RECONNECT_TIMEOUT",
            "ENGINE_POD_RECONCILE_TIMEOUT",
            "ENGINE_POD_HEARTBEAT_TIMEOUT",
        ] {
            assert!(
                PodConfig::from_lookup(lookup(&[(name, "1.5")]), None).is_err(),
                "{name} must refuse a non-integer"
            );
        }
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

    fn with_policy(json: &str) -> Result<PodConfig> {
        PodConfig::from_lookup(lookup(&[("ENGINE_POD_TEMPLATE_POLICIES", json)]), None)
    }

    #[test]
    fn a_policy_limit_map_keeps_the_default_ephemeral_cap() {
        let cfg = with_policy(r#"{"Pod":{"resource_limits":{"cpu":"4","memory":"8Gi"}}}"#).unwrap();
        let spec = pod_template_for(&cfg).spec.unwrap();
        let limits = spec.containers[0]
            .resources
            .as_ref()
            .unwrap()
            .limits
            .clone()
            .unwrap();
        assert_eq!(limits["cpu"].0, "4");
        assert_eq!(limits["memory"].0, "8Gi");
        assert_eq!(limits["ephemeral-storage"].0, "40Gi");
        // A policy can still raise the cap itself.
        let cfg =
            with_policy(r#"{"Pod":{"resource_limits":{"ephemeral-storage":"80Gi"}}}"#).unwrap();
        let limits = pod_template_for(&cfg).spec.unwrap().containers[0]
            .resources
            .clone()
            .unwrap()
            .limits
            .unwrap();
        assert_eq!(limits["ephemeral-storage"].0, "80Gi");
        assert_eq!(limits["cpu"].0, "1");
    }

    #[test]
    fn seed_paths_are_normalised_absolute_and_allow_listed() {
        let seed = |p: &str| {
            PodConfig::from_lookup(lookup(&[("ENGINE_POD_CARGO_SEED_HOST_PATH", p)]), None)
        };
        assert!(seed("/srv/seeds/cargo").is_ok());
        assert!(seed("/srv/seeds/cargo/").is_ok());
        for bad in [
            "relative/dir",
            "/",
            "/srv/../etc",
            "/srv/./x",
            "/srv//x",
            "/etc/kubernetes",
            "/var/run/docker.sock",
            "/run/x",
            "/srv/../..",
            "/home/other",
            "/srvx/seed",
        ] {
            assert!(seed(bad).is_err(), "{bad} must be refused");
        }
        // A sensitive root stays refused even when an operator allows it.
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_PNPM_SEED_HOST_PATH", "/etc/x"),
                ("ENGINE_POD_SEED_ALLOWED_PREFIXES", "/etc"),
            ]),
            None,
        );
        assert!(cfg.is_err());
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_PNPM_SEED_HOST_PATH", "/home/seeds/pnpm"),
                ("ENGINE_POD_SEED_ALLOWED_PREFIXES", "/home/seeds"),
            ]),
            None,
        )
        .unwrap();
        let pod = pod_template_for(&cfg);
        let mounts = pod.spec.unwrap().containers[0]
            .volume_mounts
            .clone()
            .unwrap();
        let m = mounts.iter().find(|m| m.name == "pnpm-seed").unwrap();
        assert_eq!(m.read_only, Some(true));
        assert_eq!(m.recursive_read_only.as_deref(), Some("IfPossible"));
    }

    #[test]
    fn the_sccache_endpoint_refuses_userinfo_and_non_http() {
        let ep =
            |v: &str| PodConfig::from_lookup(lookup(&[("ENGINE_POD_SCCACHE_ENDPOINT", v)]), None);
        assert_eq!(
            ep("https://s3.example:9000")
                .unwrap()
                .sccache_endpoint
                .as_deref(),
            Some("https://s3.example:9000")
        );
        for bad in [
            "https://key:secret@s3.example",
            "https://key@s3.example",
            "ftp://s3.example",
            "not a url",
            "s3.example:9000",
        ] {
            assert!(ep(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_policy_that_excludes_the_cache_node_is_refused_at_config_time() {
        // nodeName (winrarhost1) wins over scheduling, so these could never run.
        for bad in [
            r#"{"Pod":{"node_selector":{"kubernetes.io/hostname":"other"}}}"#,
            r#"{"Pod":{"affinity":{"nodeAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":{"nodeSelectorTerms":[{"matchExpressions":[{"key":"kubernetes.io/hostname","operator":"In","values":["other"]}]}]}}}}}"#,
            r#"{"Pod":{"affinity":{"nodeAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":{"nodeSelectorTerms":[{"matchExpressions":[{"key":"kubernetes.io/hostname","operator":"NotIn","values":["winrarhost1"]}]}]}}}}}"#,
        ] {
            let err = with_policy(bad).unwrap_err().to_string();
            assert!(err.contains("ENGINE_POD_CACHE_NODE"), "{err}");
        }
        // A matching or unrelated constraint is fine, as is any policy when
        // no node is pinned.
        assert!(with_policy(
            r#"{"Pod":{"node_selector":{"kubernetes.io/hostname":"winrarhost1"}}}"#
        )
        .is_ok());
        assert!(with_policy(r#"{"Pod":{"node_selector":{"disk":"ssd"}}}"#).is_ok());
        assert!(PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                (
                    "ENGINE_POD_TEMPLATE_POLICIES",
                    r#"{"Pod":{"node_selector":{"kubernetes.io/hostname":"other"}}}"#
                ),
            ]),
            None
        )
        .is_ok());
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
        // The private cache, the writable paths under the read-only root and
        // the bootstrap Secret are left: no shared claim.
        let volumes = spec.volumes.unwrap();
        assert_eq!(
            volumes
                .iter()
                .map(|volume| volume.name.as_str())
                .collect::<Vec<_>>(),
            [
                "session-cache",
                "session-home",
                "session-tmp",
                "session-work",
                "session-run",
                "bootstrap"
            ]
        );
        assert!(volumes
            .iter()
            .all(|volume| volume.persistent_volume_claim.is_none()));
        assert!(volumes[0].empty_dir.is_some());
        assert!(volumes.last().unwrap().secret.is_some());
        assert!(spec.node_name.is_none());
        let c = &spec.containers[0];
        assert!(c.env_from.as_ref().unwrap().is_empty());
        assert_eq!(c.volume_mounts.as_ref().unwrap().len(), 6);
        assert_eq!(spec.image_pull_secrets.unwrap()[0].name, "pull");
    }

    #[test]
    fn a_template_policy_overrides_resources_selector_and_adds_affinity() {
        let cfg = PodConfig::from_lookup(
            lookup(&[(
                "ENGINE_POD_TEMPLATE_POLICIES",
                r#"{"Pod":{"resource_requests":{"cpu":"2"},"resource_limits":{"cpu":"4","memory":"8Gi"},
                   "node_selector":{"disk":"nvme"},
                   "affinity":{"nodeAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":
                     {"nodeSelectorTerms":[{"matchExpressions":[{"key":"k","operator":"Exists"}]}]}}}},
                   "Other":{"resource_limits":{"cpu":"9"}}}"#,
            )]),
            None,
        )
        .unwrap();
        let spec = pod_template_for(&cfg).spec.unwrap();
        let r = spec.containers[0].resources.clone().unwrap();
        let requests = r.requests.unwrap();
        assert_eq!(requests["cpu"].0, "2");
        assert_eq!(requests["memory"].0, "256Mi");
        assert_eq!(r.limits.unwrap()["memory"].0, "8Gi");
        assert_eq!(spec.node_selector.unwrap()["disk"], "nvme");
        assert!(spec.affinity.unwrap().node_affinity.is_some());
        // A template with no entry keeps the defaults.
        let d = PodConfig::default();
        let plain = PodConfig {
            template_policies: BTreeMap::new(),
            ..d.clone()
        };
        let spec = pod_template_for(&plain).spec.unwrap();
        assert!(spec.affinity.is_none());
        assert_eq!(
            spec.containers[0]
                .resources
                .clone()
                .unwrap()
                .limits
                .unwrap()["ephemeral-storage"]
                .0,
            "40Gi"
        );
    }

    #[test]
    fn node_preferences_exclusions_and_tolerations_reach_the_pod() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                (
                    "ENGINE_POD_PREFERRED_NODES",
                    "winrarhost4:100, winrarhost1:50,winrarhost",
                ),
                ("ENGINE_POD_EXCLUDED_NODES", "winrarhost3"),
                (
                    "ENGINE_POD_TOLERATIONS",
                    r#"[{"key":"estate/dedicated","operator":"Exists","effect":"NoSchedule"}]"#,
                ),
                (
                    "ENGINE_POD_TEMPLATE_POLICIES",
                    r#"{"Pod":{"affinity":{"nodeAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":
                      {"nodeSelectorTerms":[{"matchExpressions":[{"key":"k","operator":"Exists"}]}]}}}}}"#,
                ),
            ]),
            None,
        )
        .unwrap();
        let spec = pod_template_for(&cfg).spec.unwrap();
        assert!(spec.node_name.is_none());
        let n = spec.affinity.unwrap().node_affinity.unwrap();
        let pref = n
            .preferred_during_scheduling_ignored_during_execution
            .unwrap();
        let got: Vec<(i32, String)> = pref
            .iter()
            .map(|t| {
                let e = &t.preference.match_expressions.as_ref().unwrap()[0];
                assert_eq!(e.key, "kubernetes.io/hostname");
                assert_eq!(e.operator, "In");
                (t.weight, e.values.as_ref().unwrap()[0].clone())
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (100, "winrarhost4".to_string()),
                (50, "winrarhost1".to_string()),
                (1, "winrarhost".to_string())
            ]
        );
        // The template's own required term is kept and the exclusion ANDed in.
        let terms = n
            .required_during_scheduling_ignored_during_execution
            .unwrap()
            .node_selector_terms;
        assert_eq!(terms.len(), 1);
        let exprs = terms[0].match_expressions.as_ref().unwrap();
        assert_eq!(exprs[0].key, "k");
        assert_eq!(exprs[1].operator, "NotIn");
        assert_eq!(exprs[1].values.as_ref().unwrap(), &["winrarhost3"]);
        let tol = spec.tolerations.unwrap();
        assert_eq!(tol[0].key.as_deref(), Some("estate/dedicated"));
    }

    #[test]
    fn exclusions_alone_make_a_required_term_and_defaults_add_nothing() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                ("ENGINE_POD_EXCLUDED_NODES", "a,b"),
            ]),
            None,
        )
        .unwrap();
        let spec = pod_template_for(&cfg).spec.unwrap();
        let req = spec
            .affinity
            .unwrap()
            .node_affinity
            .unwrap()
            .required_during_scheduling_ignored_during_execution
            .unwrap();
        assert_eq!(
            req.node_selector_terms[0]
                .match_expressions
                .as_ref()
                .unwrap()[0]
                .values
                .as_ref()
                .unwrap(),
            &["a", "b"]
        );
        assert!(spec.tolerations.is_none());
        let plain = pod_template_for(&PodConfig::default()).spec.unwrap();
        assert!(plain.affinity.is_none() && plain.tolerations.is_none());
        assert!(plain.node_selector.is_none());
    }

    #[test]
    fn a_deployment_node_selector_pins_every_pod() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                (
                    "ENGINE_POD_NODE_SELECTOR",
                    "kubernetes.io/hostname=winrarhost4, estate/host=bench",
                ),
            ]),
            None,
        )
        .unwrap();
        let spec = pod_template_for(&cfg).spec.unwrap();
        assert!(spec.node_name.is_none());
        let selector = spec.node_selector.unwrap();
        assert_eq!(selector["kubernetes.io/hostname"], "winrarhost4");
        assert_eq!(selector["estate/host"], "bench");
        assert_eq!(selector.len(), 2);
    }

    #[test]
    fn a_template_policy_adds_to_the_deployment_node_selector() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                ("ENGINE_POD_NODE_SELECTOR", "estate/host=bench"),
                (
                    "ENGINE_POD_TEMPLATE_POLICIES",
                    r#"{"Pod":{"node_selector":{"disk":"nvme"}}}"#,
                ),
            ]),
            None,
        )
        .unwrap();
        let selector = pod_template_for(&cfg).spec.unwrap().node_selector.unwrap();
        assert_eq!(selector["estate/host"], "bench");
        assert_eq!(selector["disk"], "nvme");
        assert_eq!(selector.len(), 2);
    }

    #[test]
    fn conflicting_deployment_placement_is_refused_at_config_time() {
        let err = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                (
                    "ENGINE_POD_NODE_SELECTOR",
                    "kubernetes.io/hostname=winrarhost1",
                ),
                ("ENGINE_POD_EXCLUDED_NODES", "winrarhost1"),
            ]),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ENGINE_POD_EXCLUDED_NODES"), "{err}");

        let err = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                ("ENGINE_POD_NODE_SELECTOR", "estate/host=bench"),
                (
                    "ENGINE_POD_TEMPLATE_POLICIES",
                    r#"{"Pod":{"node_selector":{"estate/host":"node-b"}}}"#,
                ),
            ]),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ENGINE_POD_NODE_SELECTOR"), "{err}");

        let err = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                ("ENGINE_POD_EXCLUDED_NODES", "winrarhost2"),
                (
                    "ENGINE_POD_TEMPLATE_POLICIES",
                    r#"{"Pod":{"node_selector":{"kubernetes.io/hostname":"winrarhost2"}}}"#,
                ),
            ]),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ENGINE_POD_EXCLUDED_NODES"), "{err}");

        // The same key and value on both is agreement, not a conflict.
        assert!(PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CACHE_NODE", ""),
                ("ENGINE_POD_NODE_SELECTOR", "estate/host=bench"),
                (
                    "ENGINE_POD_TEMPLATE_POLICIES",
                    r#"{"Pod":{"node_selector":{"estate/host":"bench"}}}"#,
                ),
            ]),
            None,
        )
        .is_ok());
    }

    #[test]
    fn bad_node_placement_settings_are_refused() {
        for (name, bad) in [
            ("ENGINE_POD_PREFERRED_NODES", "n:0"),
            ("ENGINE_POD_PREFERRED_NODES", "n:101"),
            ("ENGINE_POD_PREFERRED_NODES", ":5"),
            ("ENGINE_POD_PREFERRED_NODES", "n:x"),
            ("ENGINE_POD_TOLERATIONS", "not json"),
            ("ENGINE_POD_NODE_SELECTOR", "no-equals"),
            ("ENGINE_POD_NODE_SELECTOR", "=winrarhost4"),
            ("ENGINE_POD_NODE_SELECTOR", "estate/host="),
            ("ENGINE_POD_NODE_SELECTOR", "a=1,a=2"),
            // Default cache node winrarhost1 is pinned by nodeName; a hostname
            // selector that disagrees can never schedule.
            (
                "ENGINE_POD_NODE_SELECTOR",
                "kubernetes.io/hostname=winrarhost4",
            ),
            // Default cache node winrarhost1 is pinned by nodeName.
            ("ENGINE_POD_EXCLUDED_NODES", "winrarhost1"),
        ] {
            assert!(
                PodConfig::from_lookup(lookup(&[(name, bad)]), None).is_err(),
                "{name}={bad}"
            );
        }
    }

    #[test]
    fn bad_template_policies_are_refused() {
        for bad in ["not json", r#"{"Pod":{"bogus":1}}"#] {
            assert!(
                PodConfig::from_lookup(lookup(&[("ENGINE_POD_TEMPLATE_POLICIES", bad)]), None)
                    .is_err()
            );
        }
    }

    #[test]
    fn host_seeds_and_sccache_are_off_by_default() {
        let spec = pod_template_for(&PodConfig::default()).spec.unwrap();
        assert!(spec.volumes.unwrap().iter().all(|v| v.host_path.is_none()));
        assert!(
            !cache_env(&pod_template_for(&PodConfig::default())).contains_key("SCCACHE_ENDPOINT")
        );
    }

    #[test]
    fn host_seeds_are_read_only_and_sccache_endpoint_is_passed() {
        let cfg = PodConfig::from_lookup(
            lookup(&[
                ("ENGINE_POD_CARGO_SEED_HOST_PATH", "/srv/cargo"),
                ("ENGINE_POD_PNPM_SEED_HOST_PATH", "/srv/pnpm"),
                ("ENGINE_POD_SCCACHE_ENDPOINT", "https://s3.example/sccache"),
            ]),
            None,
        )
        .unwrap();
        let pod = pod_template_for(&cfg);
        // The WI-3242 guard accepts them: read-only mounts only.
        assert!(shared_writable_volumes(&pod).is_empty());
        let spec = pod.spec.as_ref().unwrap();
        let mounts = spec.containers[0].volume_mounts.as_ref().unwrap();
        for (name, path) in [("cargo-seed", "/srv/cargo"), ("pnpm-seed", "/srv/pnpm")] {
            let m = mounts.iter().find(|m| m.name == name).unwrap();
            assert_eq!(m.read_only, Some(true));
            assert!(m.mount_path.starts_with(SHARED_CACHE_DIR));
            let v = spec
                .volumes
                .as_ref()
                .unwrap()
                .iter()
                .find(|v| v.name == name)
                .unwrap();
            assert_eq!(v.host_path.as_ref().unwrap().path, path);
        }
        let env = cache_env(&pod);
        assert_eq!(env["SCCACHE_ENDPOINT"], "https://s3.example/sccache");
        assert_eq!(env["VOGT_CARGO_SEED_DIR"], "/cache-ro/cargo-seed");
        assert!(PodConfig::from_lookup(
            lookup(&[("ENGINE_POD_CARGO_SEED_HOST_PATH", "relative")]),
            None
        )
        .is_err());
    }
}
