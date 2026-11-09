//! Reconciles the cluster's session pods with the engine's sessions.
//!
//! The engine holds each pod session's state in memory (its stream link and
//! credentials), the cluster holds the pods. They drift apart when a pod dies
//! without its agent reporting `Exited`, when a pod vanishes, and when the
//! engine restarts. One sweep lists the pods and settles every disagreement:
//!
//! - a live session whose pod failed, completed unreported, is stuck in a
//!   state that cannot heal, or has disappeared, is ended (the pod phase and
//!   reason become the session's last output and its exit code);
//! - a pod with no live session behind it is an orphan and is deleted.
//!
//! After an engine restart no session holds a link or credential, so the
//! pods the previous process left are orphans: the agent in such a pod cannot
//! authenticate again (its session token lived in the old process), so the
//! pod is marked lost by deleting it, and the session itself comes back from
//! its record as hibernated (`recover_records`).
use crate::{
    agent_stream::{AgentAuth, PodLink},
    chats::redact,
    error::Result,
    pod_backend::{
        pod_exit_code, pod_fatal_waiting_reason, pod_is_ours, pod_phase, pod_reason,
        session_id_from_pod, PodConfig, PodLauncher, LOG_TAIL_LINES,
    },
};
use k8s_openapi::api::core::v1::Pod;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// What a sweep decides about one pod.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to do yet.
    Keep,
    /// The session's pod is gone or beyond help: end the session.
    End { code: i32, note: String },
    /// No live session owns the pod: delete it.
    Orphan,
}

/// Judge one pod. `link` is the engine's stream link for the pod's session,
/// `None` when the engine holds none; `age` is how old the pod is.
pub fn judge(pod: &Pod, link: Option<&PodLink>, cfg: &PodConfig, age: Option<Duration>) -> Verdict {
    if pod.metadata.deletion_timestamp.is_some() {
        return Verdict::Keep;
    }
    let Some(link) = link else {
        // A pod younger than the connect grace may belong to a start still in
        // flight; the next sweep decides.
        return match age {
            Some(age) if age < cfg.connect_timeout => Verdict::Keep,
            _ => Verdict::Orphan,
        };
    };
    if link.is_finished() {
        // The session already ended; its start task deletes the pod. If that
        // delete failed, the link is forgotten and the pod is an orphan next.
        return Verdict::Keep;
    }
    let detail = |what: &str| match pod_reason(pod) {
        Some(reason) => format!("{what}: {reason}"),
        None => what.to_string(),
    };
    match pod_phase(pod) {
        "exited" => Verdict::End {
            code: pod_exit_code(pod).unwrap_or(0),
            note: detail("completed before the agent reported its exit"),
        },
        "errored" => Verdict::End {
            code: pod_exit_code(pod).filter(|c| *c != 0).unwrap_or(1),
            note: detail("failed"),
        },
        "pending" => {
            if let Some(reason) = pod_fatal_waiting_reason(pod) {
                return Verdict::End {
                    code: 1,
                    note: format!("could not start: {reason}"),
                };
            }
            match age {
                Some(age) if age > cfg.pending_timeout => Verdict::End {
                    code: 1,
                    note: format!("was still pending after {}s", age.as_secs()),
                },
                _ => Verdict::Keep,
            }
        }
        _ => Verdict::Keep,
    }
}

/// End a live session from outside its agent: the note becomes its last
/// output and the code its exit status, as if the agent had reported it.
fn end_session(link: &PodLink, code: i32, note: &str) {
    let text = format!("\r\nsession pod {note}\r\n");
    let _ = link.ingest(link.next_offset(), text.as_bytes());
    link.finish(code);
}

/// Bytes of container log one failed-session note keeps, so fifty long lines
/// cannot turn the note into a wall of text.
const LOG_NOTE_BYTES: usize = 8 * 1024;

/// The tail of a failed pod's container log, or `None` when there is none to
/// show. The pod's status only ever says "Error" for a container that exited
/// without a message; the log is where the reason actually is.
async fn failed_log_tail(launcher: &dyn PodLauncher, auth: &AgentAuth, id: Uuid) -> Option<String> {
    match launcher.log_tail(id).await {
        Ok(tail) => {
            let tail = tail.trim_end();
            if tail.trim().is_empty() {
                return None;
            }
            let mut start = tail.len().saturating_sub(LOG_NOTE_BYTES);
            while !tail.is_char_boundary(start) {
                start += 1;
            }
            // Start at a line boundary so the first line is whole.
            if start > 0 {
                start = tail[start..]
                    .find('\n')
                    .map(|i| start + i + 1)
                    .unwrap_or(start);
            }
            let mut tail = redact(&tail[start..]);
            if let Some(token) = auth.session_token(id).await {
                tail = tail.replace(&token, "[redacted]");
            }
            Some(tail)
        }
        Err(e) => {
            tracing::debug!(session = %id, error = %e, "reading a failed pod's log failed");
            None
        }
    }
}

fn pod_age(pod: &Pod) -> Option<Duration> {
    let created = pod.metadata.creation_timestamp.as_ref()?;
    (k8s_openapi::chrono::Utc::now() - created.0).to_std().ok()
}

/// What one sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    pub ended: Vec<Uuid>,
    pub deleted: Vec<Uuid>,
}

pub async fn sweep(launcher: &dyn PodLauncher, auth: &AgentAuth, cfg: &PodConfig) -> Result<Sweep> {
    let pods = launcher.list().await?;
    let mut report = Sweep::default();
    let mut seen = HashSet::new();
    for pod in &pods {
        // The list is selected by label already; an engine's own label is
        // checked again so a pod of another deployment is never judged.
        if !pod_is_ours(pod, cfg) {
            continue;
        }
        let Some(id) = session_id_from_pod(pod) else {
            continue;
        };
        seen.insert(id);
        let link = auth.pod(id);
        match judge(pod, link.as_deref(), cfg, pod_age(pod)) {
            Verdict::Keep => {}
            Verdict::End { code, note } => {
                if let Some(link) = link.as_deref() {
                    tracing::warn!(session = %id, code, %note, "ending a session whose pod died");
                    let note = if code != 0 {
                        match failed_log_tail(launcher, auth, id).await {
                            Some(tail) => format!(
                                "{note}\n--- container log (last {LOG_TAIL_LINES} lines) ---\n{tail}"
                            ),
                            None => note,
                        }
                    } else {
                        note
                    };
                    end_session(link, code, &note);
                    report.ended.push(id);
                }
            }
            Verdict::Orphan => {
                // Delete exactly the pod that was judged: without a UID the
                // name could now belong to a replacement.
                let Some(uid) = pod.metadata.uid.clone() else {
                    tracing::warn!(session = %id, "an orphan pod without a uid is left alone");
                    continue;
                };
                match launcher.delete(id, Some(uid)).await {
                    Ok(()) => {
                        tracing::info!(session = %id, "deleted an orphan session pod");
                        report.deleted.push(id);
                    }
                    Err(e) => {
                        tracing::warn!(session = %id, error = %e, "deleting an orphan pod failed")
                    }
                }
            }
        }
    }
    // A live session whose pod is not in the cluster at all.
    for id in auth.pod_ids() {
        if seen.contains(&id) {
            continue;
        }
        let Some(link) = auth.pod(id) else { continue };
        if !link.is_finished() && link.age() > cfg.connect_timeout {
            tracing::warn!(session = %id, "ending a session whose pod disappeared");
            end_session(&link, 1, "disappeared from the cluster");
            report.ended.push(id);
        }
    }
    Ok(report)
}

/// Run [`sweep`] every `cfg.reconcile_interval` until the engine stops.
pub fn spawn(
    launcher: Arc<dyn PodLauncher>,
    auth: Arc<AgentAuth>,
    cfg: PodConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut failing = false;
        loop {
            match sweep(launcher.as_ref(), &auth, &cfg).await {
                Ok(_) => failing = false,
                Err(e) if !failing => {
                    failing = true;
                    tracing::warn!(error = %e, "session pod reconcile failed");
                }
                Err(e) => tracing::debug!(error = %e, "session pod reconcile still failing"),
            }
            tokio::time::sleep(cfg.reconcile_interval).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pod_backend::{
        CreateFuture, GoneFuture, LaunchFuture, ListFuture, DEFAULT_ENGINE_ID, ENGINE_LABEL,
    };
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateTerminated, ContainerStateWaiting, ContainerStatus, PodStatus,
    };
    use std::{collections::BTreeMap, sync::Mutex};

    /// A stand-in for the kube API: serves a fixed pod list and records deletes.
    struct MockKube {
        pods: Mutex<Vec<Pod>>,
        deleted: Mutex<Vec<(Uuid, Option<String>)>>,
        list_fails: bool,
        /// What the session container's log returns; `None` fails like an
        /// unavailable log stream (a pending pod, an API error).
        log: Option<String>,
    }
    impl MockKube {
        fn with(pods: Vec<Pod>) -> Self {
            Self {
                pods: Mutex::new(pods),
                deleted: Mutex::new(Vec::new()),
                list_fails: false,
                log: None,
            }
        }
    }
    impl PodLauncher for MockKube {
        fn create<'a>(&'a self, _: Uuid, _: &'a [u8], _: Pod) -> CreateFuture<'a> {
            Box::pin(async { Ok(None) })
        }
        fn gone(&self, _: Uuid, _: Option<String>) -> GoneFuture<'_> {
            Box::pin(async { Ok(true) })
        }
        fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_> {
            Box::pin(async move {
                self.deleted.lock().unwrap().push((id, uid));
                Ok(())
            })
        }
        fn list(&self) -> ListFuture<'_> {
            Box::pin(async move {
                if self.list_fails {
                    return Err(crate::error::ApiError::Internal("api down".into()));
                }
                Ok(self.pods.lock().unwrap().clone())
            })
        }
        fn log_tail(&self, _: Uuid) -> crate::pod_backend::LogFuture<'_> {
            Box::pin(async move {
                self.log
                    .clone()
                    .ok_or_else(|| crate::error::ApiError::Internal("no log".into()))
            })
        }
    }

    fn pod(id: Uuid, status: PodStatus) -> Pod {
        let mut pod = Pod::default();
        pod.metadata.labels = Some(BTreeMap::from([
            ("vogt.session".into(), id.to_string()),
            (ENGINE_LABEL.into(), DEFAULT_ENGINE_ID.into()),
        ]));
        pod.metadata.uid = Some(format!("uid-{id}"));
        pod.status = Some(status);
        pod
    }
    fn phase(phase: &str) -> PodStatus {
        PodStatus {
            phase: Some(phase.into()),
            ..Default::default()
        }
    }
    fn terminated(phase_: &str, exit_code: i32, message: &str) -> PodStatus {
        PodStatus {
            container_statuses: Some(vec![ContainerStatus {
                state: Some(ContainerState {
                    terminated: Some(ContainerStateTerminated {
                        exit_code,
                        message: Some(message.into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..phase(phase_)
        }
    }
    fn waiting(reason: &str) -> PodStatus {
        PodStatus {
            container_statuses: Some(vec![ContainerStatus {
                state: Some(ContainerState {
                    waiting: Some(ContainerStateWaiting {
                        reason: Some(reason.into()),
                        message: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..phase("Pending")
        }
    }
    fn cfg() -> PodConfig {
        PodConfig {
            connect_timeout: Duration::ZERO,
            ..Default::default()
        }
    }
    fn live(id: Uuid) -> (AgentAuth, Arc<PodLink>) {
        let auth = AgentAuth::default();
        let (link, _reader) = auth.register_pod(id);
        // The reader is dropped: the session's reader thread is not needed to
        // judge the link.
        (auth, link)
    }

    #[test]
    fn phases_map_to_verdicts() {
        let id = Uuid::new_v4();
        let (_auth, link) = live(id);
        let cfg = cfg();
        let judge_of = |status| judge(&pod(id, status), Some(&link), &cfg, None);

        assert_eq!(judge_of(phase("Running")), Verdict::Keep);
        assert_eq!(judge_of(phase("Pending")), Verdict::Keep);
        assert_eq!(
            judge_of(terminated("Failed", 137, "OOMKilled")),
            Verdict::End {
                code: 137,
                note: "failed: OOMKilled".into()
            }
        );
        // A failed pod whose container reported no code still fails the session.
        assert!(matches!(
            judge_of(phase("Failed")),
            Verdict::End { code: 1, .. }
        ));
        assert!(matches!(
            judge_of(phase("Unknown")),
            Verdict::End { code: 1, .. }
        ));
        assert!(matches!(
            judge_of(terminated("Succeeded", 0, "")),
            Verdict::End { code: 0, .. }
        ));
        assert!(matches!(
            judge_of(waiting("ImagePullBackOff")),
            Verdict::End { code: 1, .. }
        ));
        // Waiting for a node is not fatal until the pending timeout passes.
        assert_eq!(judge_of(waiting("ContainerCreating")), Verdict::Keep);
        let late = Some(cfg.pending_timeout + Duration::from_secs(1));
        assert!(matches!(
            judge(&pod(id, phase("Pending")), Some(&link), &cfg, late),
            Verdict::End { code: 1, .. }
        ));
    }

    #[test]
    fn a_finished_link_or_a_terminating_pod_is_left_alone() {
        let id = Uuid::new_v4();
        let (_auth, link) = live(id);
        let failed = pod(id, phase("Failed"));
        link.finish(0);
        assert_eq!(judge(&failed, Some(&link), &cfg(), None), Verdict::Keep);

        let (_auth, link) = live(id);
        let mut terminating = pod(id, phase("Failed"));
        terminating.metadata.deletion_timestamp = Some(
            k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::chrono::Utc::now()),
        );
        assert_eq!(
            judge(&terminating, Some(&link), &cfg(), None),
            Verdict::Keep
        );
    }

    #[test]
    fn a_young_pod_with_no_link_waits_out_the_grace() {
        let id = Uuid::new_v4();
        let cfg = PodConfig {
            connect_timeout: Duration::from_secs(30),
            ..Default::default()
        };
        let p = pod(id, phase("Running"));
        assert_eq!(
            judge(&p, None, &cfg, Some(Duration::from_secs(5))),
            Verdict::Keep
        );
        assert_eq!(
            judge(&p, None, &cfg, Some(Duration::from_secs(31))),
            Verdict::Orphan
        );
        assert_eq!(judge(&p, None, &cfg, None), Verdict::Orphan);
    }

    #[tokio::test]
    async fn a_session_whose_pod_died_without_exited_ends_with_the_reason() {
        let id = Uuid::new_v4();
        let auth = AgentAuth::default();
        let (link, mut reader) = auth.register_pod(id);
        auth.issue(id, b"bootstrap").await;
        assert!(auth.consume(id, b"bootstrap").await);
        let session_token = auth.session_token(id).await.unwrap();
        link.ingest(0, b"hello").unwrap();
        let mut kube = MockKube::with(vec![pod(id, terminated("Failed", 137, "OOMKilled"))]);
        kube.log = Some(
            format!(
                "session-entrypoint: step clone\n\
                 session agent failed: no such file\n\
                 Authorization: Bearer bearer-secret-123456\n\
                 token=source-secret-123456\n\
                 session={session_token}\n"
            )
            .into(),
        );

        let report = sweep(&kube, &auth, &cfg()).await.unwrap();

        assert_eq!(report.ended, vec![id]);
        assert!(report.deleted.is_empty());
        assert!(link.is_finished());
        assert_eq!(link.wait_exit(), 137);
        // The reason was appended after the bytes already received.
        assert!(link.next_offset() > 5);
        // The container's own tail is in the note: a status of "Error" alone
        // is not a diagnosis (WI-3762).
        let mut output = String::new();
        std::io::Read::read_to_string(&mut reader, &mut output).unwrap();
        assert!(output.contains("session pod failed: OOMKilled"));
        assert!(output.contains("--- container log (last 50 lines) ---"));
        assert!(output.contains("session agent failed: no such file"));
        assert!(output.contains("Authorization: [redacted] [redacted]"));
        assert!(output.contains("token=[redacted]"));
        assert!(!output.contains("bearer-secret-123456"));
        assert!(!output.contains("source-secret-123456"));
        assert!(!output.contains(&session_token));
    }

    #[tokio::test]
    async fn an_unavailable_log_leaves_the_status_reason_alone() {
        let id = Uuid::new_v4();
        let auth = AgentAuth::default();
        let (_link, mut reader) = auth.register_pod(id);
        let kube = MockKube::with(vec![pod(id, terminated("Failed", 1, "Error"))]);

        let report = sweep(&kube, &auth, &cfg()).await.unwrap();

        assert_eq!(report.ended, vec![id]);
        let mut output = String::new();
        std::io::Read::read_to_string(&mut reader, &mut output).unwrap();
        assert!(output.contains("session pod failed: Error"));
        assert!(!output.contains("--- container log"));
    }

    #[tokio::test]
    async fn a_pod_with_no_live_session_is_deleted() {
        let id = Uuid::new_v4();
        let kube = MockKube::with(vec![
            pod(id, phase("Running")),
            // A pod without the session label is not ours to touch.
            Pod::default(),
        ]);
        let auth = AgentAuth::default();

        let report = sweep(&kube, &auth, &cfg()).await.unwrap();

        assert_eq!(report.deleted, vec![id]);
        // The delete is bound to the listed pod's uid.
        assert_eq!(
            *kube.deleted.lock().unwrap(),
            vec![(id, Some(format!("uid-{id}")))]
        );
    }

    #[tokio::test]
    async fn after_a_restart_every_pod_of_the_old_process_is_lost() {
        // The new process has no links: pods that were running are orphans,
        // pods that had already finished are too.
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let kube = MockKube::with(vec![pod(a, phase("Running")), pod(b, phase("Succeeded"))]);

        let report = sweep(&kube, &AgentAuth::default(), &cfg()).await.unwrap();

        let mut deleted = report.deleted;
        deleted.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(deleted, expected);
    }

    #[tokio::test]
    async fn a_live_pod_with_a_live_session_is_left_alone() {
        let id = Uuid::new_v4();
        let (auth, link) = live(id);
        let kube = MockKube::with(vec![pod(id, phase("Running"))]);

        let report = sweep(&kube, &auth, &cfg()).await.unwrap();

        assert_eq!(report, Sweep::default());
        assert!(!link.is_finished());
        assert!(kube.deleted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_session_whose_pod_vanished_ends_after_the_grace() {
        let id = Uuid::new_v4();
        let (auth, link) = live(id);
        let kube = MockKube::with(vec![]);
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Within the grace the pod may simply not be listed yet.
        let patient = PodConfig {
            connect_timeout: Duration::from_secs(60),
            ..Default::default()
        };
        assert_eq!(
            sweep(&kube, &auth, &patient).await.unwrap(),
            Sweep::default()
        );
        assert!(!link.is_finished());

        let report = sweep(&kube, &auth, &cfg()).await.unwrap();
        assert_eq!(report.ended, vec![id]);
        assert_eq!(link.wait_exit(), 1);
    }

    #[tokio::test]
    async fn a_failing_list_changes_nothing() {
        let id = Uuid::new_v4();
        let (auth, link) = live(id);
        let mut kube = MockKube::with(vec![pod(id, phase("Failed"))]);
        kube.list_fails = true;

        assert!(sweep(&kube, &auth, &cfg()).await.is_err());
        assert!(!link.is_finished());
    }

    #[tokio::test]
    async fn another_engines_pod_is_never_judged_or_deleted() {
        let (mine, theirs, unlabelled) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut other = pod(theirs, phase("Running"));
        other
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert(ENGINE_LABEL.into(), "vogt-prod".into());
        let mut legacy = pod(unlabelled, phase("Running"));
        legacy
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .remove(ENGINE_LABEL);
        let kube = MockKube::with(vec![pod(mine, phase("Running")), other, legacy]);

        let report = sweep(&kube, &AgentAuth::default(), &cfg()).await.unwrap();

        assert_eq!(report.deleted, vec![mine]);
        assert_eq!(kube.deleted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_orphan_without_a_uid_is_left_alone() {
        let id = Uuid::new_v4();
        let mut p = pod(id, phase("Running"));
        p.metadata.uid = None;
        let kube = MockKube::with(vec![p]);

        let report = sweep(&kube, &AgentAuth::default(), &cfg()).await.unwrap();

        assert!(report.deleted.is_empty());
        assert!(kube.deleted.lock().unwrap().is_empty());
    }
}
