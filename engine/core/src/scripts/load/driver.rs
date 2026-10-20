//! The in-process driver of `scripts/load.py` (WI-1744).
//!
//! It builds one context over a fresh data directory and drives the real
//! operations through the registry (`Operation::run`), the same handlers the
//! CLI, REST and MCP surfaces reach, with a principal and a reason on every
//! write. The deployed mode serves the registry and MCP routes on a loopback
//! port with auth on and drives them with concurrent HTTP clients carrying a
//! real bearer token. The order of calls and their parameters follow
//! `scripts/load.py`, so a Rust number and a Python number measure the same
//! work.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{json, Value};

use super::{
    build_report, build_soak_report, current_rss_mb, peak_rss_mb, relation_triples, DatasetPlan,
    DeployedPlan, LatencyRecorder, SoakPlan, SoakRecorder, SoakReportInput, DEPLOYED_NOTE,
    SOAK_NOTE,
};
use crate::application::context::{build_context, Built};
use crate::config::{SqliteSynchronous, VogtConfig};
use crate::core::{ActorKind, Principal};
use crate::registry::{default_registry, OperationRegistry};

/// Cycled so the ranked views have variety to sort by rather than one bucket.
const KINDS: [&str; 4] = ["feature", "bug", "chore", "question"];
const PRIORITIES: [&str; 5] = ["p0", "p1", "p2", "p3", "p4"];

/// The request mix the deployed driver cycles through, one op per slot:
/// weighted to the hot read surfaces (`work.list` twice), plus a create and an
/// update so the authenticated write path is measured too.
const DEPLOYED_MIX: [&str; 8] = [
    "work.list",
    "backlog",
    "board.list",
    "inbox.list",
    "work.list",
    "bugs",
    "work.create",
    "work.update",
];

/// One context over one data directory, and the registry that drives it.
struct Driver {
    registry: OperationRegistry,
    built: Built,
    data_dir: PathBuf,
}

impl Driver {
    fn open(data_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|err| format!("{}: {err}", data_dir.display()))?;
        crate::application::instance::init(data_dir, &mut None, &mut None)
            .map_err(|err| format!("init: {err}"))?;
        let principal = Principal::new("local:load-generator", ActorKind::Agent, "load-generator")?;
        let built = build_context(
            VogtConfig {
                data_dir: data_dir.to_path_buf(),
                sqlite_synchronous: SqliteSynchronous::Off,
                ..VogtConfig::default()
            },
            Some(principal),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .map_err(|err| format!("context: {err}"))?;
        Ok(Self {
            registry: default_registry(),
            built,
            data_dir: data_dir.to_path_buf(),
        })
    }

    fn call(&self, operation: &str, params: Value) -> Result<Value, String> {
        self.registry
            .get(operation)
            .map_err(|err| format!("{operation}: {err}"))?
            .run(Some(&self.built), params)
            .map_err(|err| format!("{operation}: {err}"))
    }

    /// Run `operation` and record its latency; a failure aborts the run.
    fn timed(
        &self,
        recorder: &mut LatencyRecorder,
        operation: &str,
        params: Value,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self.call(operation, params)?;
        recorder.record(operation, started.elapsed().as_secs_f64() * 1000.0);
        Ok(result)
    }

    /// Register N projects, each a small on-disk tree a sweep can walk.
    fn register_projects(
        &self,
        recorder: &mut LatencyRecorder,
        plan: &DatasetPlan,
    ) -> Result<(), String> {
        let trees = self.data_dir.join("trees");
        for index in 0..plan.projects {
            let tree = trees.join(format!("project-{index:05}"));
            fs::create_dir_all(&tree).map_err(|err| format!("{}: {err}", tree.display()))?;
            // One real manifest so the dependency collector has a file to open.
            fs::write(
                tree.join("Cargo.toml"),
                format!("[package]\nname = \"project-{index:05}\"\n"),
            )
            .map_err(|err| format!("{}: {err}", tree.display()))?;
            self.timed(
                recorder,
                "project.register",
                json!({
                    "name": format!("Load Project {index}"),
                    "root_path": tree.to_string_lossy(),
                    "reason": "load generator: registering a project",
                }),
            )?;
        }
        Ok(())
    }

    /// Create N × M native declared items, returning their refs in order.
    fn create_work_items(
        &self,
        recorder: &mut LatencyRecorder,
        plan: &DatasetPlan,
    ) -> Result<Vec<String>, String> {
        let total = plan.total_work_items() as usize;
        let mut refs = Vec::with_capacity(total);
        for index in 0..total {
            let result = self.timed(
                recorder,
                "work.create",
                json!({
                    "kind": KINDS[index % KINDS.len()],
                    "title": format!("Load work item {index}"),
                    "body": "Generated by scripts/load.py.",
                    "priority": PRIORITIES[index % PRIORITIES.len()],
                    "reason": "load generator: creating a work item",
                }),
            )?;
            refs.push(item_ref(&result)?);
        }
        Ok(refs)
    }

    /// Add R declared edges between the created items.
    fn relate_work_items(
        &self,
        recorder: &mut LatencyRecorder,
        refs: &[String],
        plan: &DatasetPlan,
        seed: i64,
    ) -> Result<(), String> {
        for (source, target, kind) in relation_triples(refs, plan.relations, seed)? {
            self.timed(
                recorder,
                "work.relate",
                json!({
                    "ref": source,
                    "kind": kind,
                    "target": target,
                    "reason": "load generator: relating two work items",
                }),
            )?;
        }
        Ok(())
    }

    /// Time the ranked views (the declared+observed merge) at scale.
    fn time_ranked_views(
        &self,
        recorder: &mut LatencyRecorder,
        iterations: i64,
    ) -> Result<(), String> {
        for _ in 0..iterations {
            self.timed(recorder, "backlog", json!({"limit": 20}))?;
            self.timed(recorder, "bugs", json!({"limit": 50}))?;
        }
        Ok(())
    }

    /// Run the offline collectors over every registered tree, once.
    fn sweep(&self, recorder: &mut LatencyRecorder) -> Result<(), String> {
        self.timed(
            recorder,
            "sweep",
            json!({"offline_only": true, "reason": "load generator: sweeping the estate"}),
        )?;
        Ok(())
    }

    /// The untimed seed the soak and deployed runs share.
    fn seed(&self, plan: &DatasetPlan, seed: i64) -> Result<Vec<String>, String> {
        let mut untimed = LatencyRecorder::default();
        self.register_projects(&mut untimed, plan)?;
        let refs = self.create_work_items(&mut untimed, plan)?;
        self.relate_work_items(&mut untimed, &refs, plan, seed)?;
        self.sweep(&mut untimed)?;
        Ok(refs)
    }

    /// One round of the soak's steady mix: create, mutate, read, list, rank.
    fn steady_iteration(&self, recorder: &mut SoakRecorder, refs: &mut Vec<String>, index: usize) {
        recorder.guard("work.create", || {
            let result = self.call(
                "work.create",
                json!({
                    "kind": KINDS[index % KINDS.len()],
                    "title": format!("Soak item {index}"),
                    "body": "Generated by scripts/load.py --mode soak.",
                    "priority": PRIORITIES[index % PRIORITIES.len()],
                    "reason": "soak: creating a work item",
                }),
            )?;
            refs.push(item_ref(&result)?);
            Ok::<(), String>(())
        });
        let target = refs[index % refs.len()].clone();
        let calls = [
            (
                "work.update",
                json!({
                    "ref": target,
                    "priority": PRIORITIES[index % PRIORITIES.len()],
                    "reason": "soak: updating a work item",
                }),
            ),
            ("work.get", json!({"ref": target})),
            ("work.list", json!({"limit": 50})),
            ("backlog", json!({"limit": 20})),
            ("bugs", json!({"limit": 50})),
        ];
        for (operation, params) in calls {
            recorder.guard(operation, || self.call(operation, params).map(drop));
        }
    }

    /// Create the benchmark's own actor and mint an admin bearer for it.
    fn mint_admin_token(&self) -> Result<String, String> {
        self.call(
            "actor.create",
            json!({
                "identity_ref": "agent:load-generator",
                "kind": "agent",
                "display_name": "Load Generator",
                "reason": "load generator: deployed-shape benchmark actor",
            }),
        )?;
        let issued = self.call(
            "token.issue",
            json!({
                "actor": "agent:load-generator",
                "name": "deployed-bench",
                "scopes": "admin",
                "reason": "load generator: deployed-shape benchmark token",
            }),
        )?;
        issued["secret"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("token.issue returned no secret: {issued}"))
    }
}

fn item_ref(result: &Value) -> Result<String, String> {
    result["item"]["ref"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("work.create returned no item ref: {result}"))
}

fn elapsed_s(started: Instant) -> f64 {
    started.elapsed().as_secs_f64()
}

/// Build the dataset through the real write path, time it, return a report.
pub(super) fn run_load(plan: &DatasetPlan, data_dir: &Path, seed: i64) -> Result<Value, String> {
    let driver = Driver::open(data_dir)?;
    let mut recorder = LatencyRecorder::default();
    let started = Instant::now();
    driver.register_projects(&mut recorder, plan)?;
    let refs = driver.create_work_items(&mut recorder, plan)?;
    driver.relate_work_items(&mut recorder, &refs, plan, seed)?;
    // Ranking before any sweep: the declared-only path.
    driver.time_ranked_views(&mut recorder, plan.ranking_iterations)?;
    driver.sweep(&mut recorder)?;
    // Ranking after the sweep: the merge now has observed evidence to fold in.
    driver.time_ranked_views(&mut recorder, plan.ranking_iterations)?;
    let duration_s = elapsed_s(started);
    Ok(build_report(
        plan,
        &recorder.stats()?,
        peak_rss_mb()?,
        seed,
        duration_s,
        Utc::now(),
    ))
}

/// Seed a base dataset, then drive a steady mix for N iterations, timed.
pub(super) fn run_soak(
    plan: &SoakPlan,
    data_dir: &Path,
    seed: i64,
    produced_by: &str,
) -> Result<Value, String> {
    let driver = Driver::open(data_dir)?;
    let mut refs = driver.seed(&plan.base, seed)?;
    let mut recorder = SoakRecorder::default();
    let rss_start = current_rss_mb()?;
    let started = Instant::now();
    for index in 0..plan.iterations as usize {
        driver.steady_iteration(&mut recorder, &mut refs, index);
        // The in-process stand-in for the live collector schedule.
        if (index as i64 + 1) % plan.sweep_every == 0 {
            recorder.guard("sweep", || {
                driver
                    .call(
                        "sweep",
                        json!({"offline_only": true, "reason": "soak: scheduled sweep"}),
                    )
                    .map(drop)
            });
        }
    }
    let duration_s = elapsed_s(started);
    let rss_end = current_rss_mb()?;
    Ok(build_soak_report(&SoakReportInput {
        plan: &plan.to_dict(),
        stats: &recorder.stats()?,
        successful_calls: recorder.successful_calls(),
        total_errors: recorder.total_errors(),
        rss_start_mb: rss_start,
        rss_end_mb: rss_end,
        rss_peak_mb: peak_rss_mb()?,
        seed,
        duration_s,
        produced_by,
        generated_at: Utc::now(),
        kind: "soak",
        note: SOAK_NOTE,
    }))
}

/// Seed a dataset in-process, then drive the real HTTP surface under auth.
pub(super) fn run_deployed(
    plan: &DeployedPlan,
    data_dir: &Path,
    seed: i64,
    produced_by: &str,
) -> Result<Value, String> {
    let driver = Driver::open(data_dir)?;
    let refs = driver.seed(&plan.base, seed)?;
    let token = driver.mint_admin_token()?;

    let rss_start = current_rss_mb()?;
    let (recorder, duration_s) = {
        let server = Server::start(&driver)?;
        drive_deployed(&server.base_url, &token, plan, &refs)
    };
    let rss_end = current_rss_mb()?;
    Ok(build_soak_report(&SoakReportInput {
        plan: &plan.to_dict(),
        stats: &recorder.stats()?,
        successful_calls: recorder.successful_calls(),
        total_errors: recorder.total_errors(),
        rss_start_mb: rss_start,
        rss_end_mb: rss_end,
        rss_peak_mb: peak_rss_mb()?,
        seed,
        duration_s,
        produced_by,
        generated_at: Utc::now(),
        kind: "deployed",
        note: DEPLOYED_NOTE,
    }))
}

/// The registry and MCP routes on a loopback port, auth on, for a run's life.
/// No collector is scheduled: the run measures the request path, and a sweep
/// firing mid-run would make the numbers non-reproducible.
struct Server {
    base_url: String,
    runtime: Option<tokio::runtime::Runtime>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Server {
    fn start(driver: &Driver) -> Result<Self, String> {
        let router = crate::core_router(&driver.built, &driver.data_dir, false, true);
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|err| format!("binding a loopback port: {err}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("loopback listener: {err}"))?;
        let addr = listener
            .local_addr()
            .map_err(|err| format!("loopback listener: {err}"))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|err| format!("tokio runtime: {err}"))?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let listener = {
            let _guard = runtime.enter();
            tokio::net::TcpListener::from_std(listener)
                .map_err(|err| format!("loopback listener: {err}"))?
        };
        runtime.spawn(async move {
            let _ = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await;
        });
        Ok(Self {
            base_url: format!("http://{addr}"),
            runtime: Some(runtime),
            stop: Some(stop),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(15));
        }
    }
}

/// Fire one request for `operation`. A non-2xx answer is an error.
fn request_for(
    agent: &ureq::Agent,
    base: &str,
    auth: &str,
    operation: &str,
    index: usize,
    refs: &[String],
) -> Result<(), String> {
    let get = |path: &str, limit: &str| {
        agent
            .get(&format!("{base}{path}"))
            .set("Authorization", auth)
            .query("limit", limit)
            .call()
            .map(drop)
            .map_err(|err| err.to_string())
    };
    let post = |path: &str, body: Value| {
        agent
            .post(&format!("{base}{path}"))
            .set("Authorization", auth)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map(drop)
            .map_err(|err| err.to_string())
    };
    let response = match operation {
        "work.list" => get("/api/work", "50"),
        "backlog" => get("/api/backlog", "20"),
        "bugs" => get("/api/bugs", "50"),
        "inbox.list" => get("/api/inbox", "50"),
        "board.list" => post(
            "/api/board/list",
            json!({"cells": [{"state": "open"}, {"state": "in_progress"}]}),
        ),
        "work.create" => post(
            "/api/work",
            json!({
                "kind": KINDS[index % KINDS.len()],
                "title": format!("Deployed item {index}"),
                "body": "Generated by scripts/load.py --mode deployed.",
                "priority": PRIORITIES[index % PRIORITIES.len()],
                "reason": "deployed bench: creating a work item",
            }),
        ),
        "work.update" => post(
            "/api/work/update",
            json!({
                "ref": refs[index % refs.len()],
                "priority": PRIORITIES[index % PRIORITIES.len()],
                "reason": "deployed bench: updating a work item",
            }),
        ),
        other => return Err(format!("unknown deployed op {other:?}")),
    };
    // ureq reports a 4xx/5xx as an error, so only a 2xx/3xx arrives here.
    response
}

/// Drive `plan.requests` HTTP calls with `plan.concurrency` of them in flight.
/// Each call is timed on success and counted as an error otherwise.
fn drive_deployed(
    base_url: &str,
    token: &str,
    plan: &DeployedPlan,
    refs: &[String],
) -> (SoakRecorder, f64) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .max_idle_connections_per_host(plan.concurrency as usize)
        .build();
    let auth = format!("Bearer {token}");
    let recorder = Mutex::new(SoakRecorder::default());
    let next = AtomicUsize::new(0);
    let requests = plan.requests as usize;
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..plan.concurrency {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= requests {
                    break;
                }
                let operation = DEPLOYED_MIX[index % DEPLOYED_MIX.len()];
                let call_started = Instant::now();
                let outcome = request_for(&agent, base_url, &auth, operation, index, refs);
                let elapsed_ms = call_started.elapsed().as_secs_f64() * 1000.0;
                let mut recorder = recorder.lock().unwrap_or_else(|p| p.into_inner());
                match outcome {
                    Ok(()) => recorder.record(operation, elapsed_ms),
                    Err(_) => recorder.record_error(operation),
                }
            });
        }
    });
    let duration_s = elapsed_s(started);
    (
        recorder.into_inner().unwrap_or_else(|p| p.into_inner()),
        duration_s,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vogt-load-{name}-{}-{}",
            std::process::id(),
            crate::core::fresh_id("t")
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_load_run_drives_every_operation_and_reports_it() {
        let dir = scratch("load");
        let plan = DatasetPlan::new(2, 3, 2, 2).unwrap();
        let report = run_load(&plan, &dir.join("instance"), 0).unwrap();
        let _ = fs::remove_dir_all(&dir);
        let ops = report["operations"].as_object().unwrap();
        assert_eq!(ops["project.register"]["count"], 2, "{report}");
        assert_eq!(ops["work.create"]["count"], 6, "{report}");
        assert_eq!(ops["work.relate"]["count"], 2, "{report}");
        // Ranked views run before and after the sweep.
        assert_eq!(ops["backlog"]["count"], 4, "{report}");
        assert_eq!(ops["bugs"]["count"], 4, "{report}");
        assert_eq!(ops["sweep"]["count"], 1, "{report}");
        assert!(report["rss_mb"].as_f64().unwrap() > 0.0, "{report}");
    }

    #[test]
    fn a_soak_run_records_the_steady_mix_without_errors() {
        let dir = scratch("soak");
        let plan = SoakPlan::new(DatasetPlan::new(1, 2, 1, 1).unwrap(), 4, 2).unwrap();
        let report = run_soak(&plan, &dir.join("instance"), 0, "in-process").unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(report["total_errors"], 0, "{report}");
        let ops = report["operations"].as_object().unwrap();
        for op in [
            "work.create",
            "work.update",
            "work.get",
            "work.list",
            "backlog",
            "bugs",
        ] {
            assert_eq!(ops[op]["count"], 4, "{op}: {report}");
        }
        assert_eq!(ops["sweep"]["count"], 2, "{report}");
    }

    #[test]
    fn a_deployed_run_drives_the_http_surface_with_a_real_token() {
        let dir = scratch("deployed");
        let plan = DeployedPlan::new(DatasetPlan::new(1, 2, 1, 1).unwrap(), 16, 4).unwrap();
        let report = run_deployed(&plan, &dir.join("instance"), 0, "deployed").unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(report["total_errors"], 0, "{report}");
        let ops = report["operations"].as_object().unwrap();
        // Sixteen requests over the eight-slot mix: work.list twice per cycle.
        assert_eq!(ops["work.list"]["count"], 4, "{report}");
        for op in [
            "backlog",
            "board.list",
            "inbox.list",
            "bugs",
            "work.create",
            "work.update",
        ] {
            assert_eq!(ops[op]["count"], 2, "{op}: {report}");
        }
    }

    #[test]
    fn a_deployed_request_without_the_token_is_refused() {
        let dir = scratch("refused");
        let driver = Driver::open(&dir.join("instance")).unwrap();
        let server = Server::start(&driver).unwrap();
        let agent = ureq::AgentBuilder::new().build();
        let err =
            request_for(&agent, &server.base_url, "Bearer nope", "work.list", 0, &[]).unwrap_err();
        drop(server);
        let _ = fs::remove_dir_all(&dir);
        assert!(err.contains("401"), "{err}");
    }
}
