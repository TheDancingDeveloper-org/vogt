//! Kubernetes lifecycle support for opted-in session templates.
use crate::{
    config::SessionTemplate,
    error::{ApiError, Result},
    session_backend::SessionBackend,
};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMapEnvSource, ConfigMapVolumeSource, Container, EnvFromSource, EnvVar,
    PersistentVolumeClaimVolumeSource, Pod, PodSecurityContext, PodSpec, ResourceRequirements,
    SeccompProfile, SecretEnvSource, SecurityContext, Volume, VolumeMount,
};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{
    api::{Api, DeleteParams, ListParams, ObjectMeta, PostParams},
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
    pub cli: String,
    pub extra_ca_file: Option<String>,
    pub priority_class: Option<String>,
    pub service_account: String,
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
            extra_ca_file: None,
            priority_class: Some("vogt-session".into()),
            service_account: "vogt-session".into(),
        }
    }
}
impl PodConfig {
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
pub fn pod_name(id: Uuid) -> String {
    format!("vogt-session-{}", id.simple())
}

fn pod_env(cfg: &PodConfig, id: Uuid, bootstrap_token: &[u8]) -> Vec<EnvVar> {
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
        EnvVar {
            name: "VOGT_BOOTSTRAP_TOKEN".into(),
            value: Some(String::from_utf8_lossy(bootstrap_token).into_owned()),
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
    if let Some(path) = &cfg.extra_ca_file {
        env.push(EnvVar {
            name: "VOGT_EXTRA_CA_FILE".into(),
            value: Some(path.clone()),
            ..Default::default()
        });
    }
    // Compatibility with the scaffold entrypoint contract.
    env.push(EnvVar {
        name: "VOGT_AGENT_BOOTSTRAP_TOKEN".into(),
        value: Some(String::from_utf8_lossy(bootstrap_token).into_owned()),
        ..Default::default()
    });
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
    bootstrap_token: &[u8],
) -> Pod {
    let mut labels = BTreeMap::from([
        ("vogt.session".into(), id.to_string()),
        ("vogt.kind".into(), kind.into()),
        ("vogt.project".into(), project.into()),
    ]);
    if let Some(account) = account {
        labels.insert("vogt.account".into(), account.into());
    }
    let image = template
        .env
        .iter()
        .find_map(|(k, v)| (k == "VOGT_SESSION_IMAGE").then_some(v.as_str()))
        .unwrap_or(&cfg.default_image);
    let env = pod_env(cfg, id, bootstrap_token);
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
                command: (!command.is_empty()).then(|| command.to_vec()),
                env: Some(env),
                env_from: Some(vec![
                    EnvFromSource {
                        config_map_ref: Some(ConfigMapEnvSource {
                            name: cfg.cache_env_config_map.clone(),
                            optional: Some(false),
                        }),
                        ..Default::default()
                    },
                    EnvFromSource {
                        secret_ref: Some(SecretEnvSource {
                            name: cfg.cache_secret.clone(),
                            optional: Some(false),
                        }),
                        ..Default::default()
                    },
                ]),
                volume_mounts: Some(vec![
                    VolumeMount {
                        name: "session-cache".into(),
                        mount_path: "/cache".into(),
                        ..Default::default()
                    },
                    VolumeMount {
                        name: cfg.kache_config_map.clone(),
                        mount_path: "/etc/kache".into(),
                        ..Default::default()
                    },
                ]),
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
            node_name: Some(cfg.cache_node.clone()),
            volumes: Some(vec![
                Volume {
                    name: "session-cache".into(),
                    persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                        claim_name: cfg.cache_pvc.clone(),
                        read_only: Some(false),
                    }),
                    ..Default::default()
                },
                Volume {
                    name: "kache-config".into(),
                    config_map: Some(ConfigMapVolumeSource {
                        name: cfg.kache_config_map.clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ]),
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

    pub async fn create_resources(&self, id: Uuid, token: &[u8], mut pod: Pod) -> Result<Pod> {
        // The token is embedded only in the pod's bootstrap environment. No
        // Secret API call is possible with the engine Role. Keep this helper
        // defensive so callers cannot accidentally create an uncredentialed
        // pod even when they built the template separately.
        let env = pod
            .spec
            .as_mut()
            .and_then(|spec| spec.containers.first_mut())
            .and_then(|container| container.env.as_mut())
            .ok_or_else(|| ApiError::BadRequest("session pod has no agent container".into()))?;
        for name in ["VOGT_BOOTSTRAP_TOKEN", "VOGT_AGENT_BOOTSTRAP_TOKEN"] {
            if let Some(variable) = env.iter_mut().find(|variable| variable.name == name) {
                variable.value = Some(String::from_utf8_lossy(token).into_owned());
                variable.value_from = None;
            } else {
                env.push(EnvVar {
                    name: name.into(),
                    value: Some(String::from_utf8_lossy(token).into_owned()),
                    ..Default::default()
                });
            }
        }
        crate::agent_stream::global().issue(id, token).await;
        self.create(pod).await
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .delete(&pod_name(id), &DeleteParams::default())
            .await
            .map_err(|e| ApiError::Internal(format!("deleting session pod: {e}")))?;
        Ok(())
    }
    pub async fn reconcile(&self) -> Result<Vec<Pod>> {
        let pods = Api::<Pod>::namespaced(self.client.clone(), &self.cfg.namespace)
            .list(&ListParams::default())
            .await
            .map_err(|e| ApiError::Internal(format!("listing session pods: {e}")))?;
        Ok(pods
            .items
            .into_iter()
            .filter(|p| {
                p.metadata
                    .labels
                    .as_ref()
                    .is_some_and(|l| l.contains_key("vogt.session"))
            })
            .collect())
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

pub struct PodBackend {
    id: Uuid,
    size: parking_lot::Mutex<(u16, u16)>,
}
impl PodBackend {
    pub fn new(_factory: PodBackendFactory, id: Uuid) -> Self {
        Self {
            id,
            size: parking_lot::Mutex::new((80, 24)),
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
        crate::agent_stream::global()
            .send(
                self.id,
                vogt_session_agent::Frame::Signal {
                    signal: "kill".into(),
                },
            )
            .map_err(|e| ApiError::Pty(format!("stop: {e}")))
    }
    fn current_size(&self) -> Result<(u16, u16)> {
        Ok(*self.size.lock())
    }
    fn wait(&self) -> std::io::Result<portable_pty::ExitStatus> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "pod sessions are reaped by the agent stream",
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
    fn pod_spec_has_session_identity_and_bootstrap_token_and_security_defaults() {
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
            b"bootstrap",
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
        assert_eq!(
            container.command.as_deref(),
            Some(["bash".to_string()].as_slice())
        );
        let token = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "VOGT_AGENT_BOOTSTRAP_TOKEN")
            .unwrap();
        assert_eq!(token.value.as_deref(), Some("bootstrap"));
        assert!(token.value_from.is_none());
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
            "VOGT_BOOTSTRAP_TOKEN",
            "VOGT_GRAFT_REPO",
            "VOGT_GRAFT_LINE",
            "VOGT_CLI",
        ] {
            assert!(
                env.iter().any(|variable| variable.name == name),
                "missing {name}"
            );
        }
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
    }

    #[test]
    fn pod_env_includes_optional_ca_contract() {
        let cfg = PodConfig {
            extra_ca_file: Some("/etc/ssl/custom.pem".into()),
            ..Default::default()
        };
        let env = pod_env(&cfg, Uuid::nil(), b"token");
        assert_eq!(
            env.iter()
                .find(|v| v.name == "VOGT_BOOTSTRAP_TOKEN")
                .unwrap()
                .value
                .as_deref(),
            Some("token")
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
}
