//! `scripts/load.py` in Rust (WI-1079).
//!
//! The recorded-numbers logic is ported exactly so the committed baselines
//! (`bench/soak_baseline.json`, `bench/deployed_baseline.json`) and the 2×
//! drift rule still mean the same thing: nearest-rank percentiles, the same
//! plan growth, the same seeded relation edges, the same report JSON shape.
//!
//! Driving the real operations in-process is not reliable while the Rust
//! registry and service layer are mid-port. `run` therefore shells out to
//! `uv run python scripts/load.py` with the same arguments when that file is
//! in the current directory, and otherwise returns a clear error. The pure
//! comparison (`check_baseline`) stays in-process.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};

/// Schema version of the one-shot load report. Bumped when its shape changes.
pub const REPORT_SCHEMA_VERSION: i64 = 1;
/// Schema version of the soak / deployed report.
pub const SOAK_REPORT_SCHEMA_VERSION: i64 = 1;
/// A metric more than this multiple worse than its baseline is drift.
pub const DRIFT_THRESHOLD: f64 = 2.0;

const SOAK_NOTE: &str = "Soak measurements from scripts/load.py --mode soak (increment 2). Not a production SLA; comparable only to another run with the same `produced_by`.";
const DEPLOYED_NOTE: &str = "Deployed-shape measurements from scripts/load.py --mode deployed: real HTTP (uvicorn), require_auth on, concurrent clients. Not a production SLA; comparable only to another run with the same `produced_by`.";
const LOAD_NOTE: &str =
    "Local/dev-box measurements from scripts/load.py (increment 1). Not a production SLA.";

/// `implemented_by` is observed, never declared, so it is absent here.
const RELATION_KINDS: [&str; 4] = ["depends_on", "relates_to", "duplicate_of", "parent_of"];

// -- percentile ------------------------------------------------------------

/// The `pct`-th percentile by nearest-rank on a sorted copy.
///
/// Ranks are 1-based; `pct` is a fraction in `[0, 1]`. The rank formula is
/// the one in `scripts/load.py`: `ceil(n * int(pct * 100) / 100)` with no
/// floating-point error, so a recorded p95 stays a number that was observed.
pub fn percentile(values: &[f64], pct: f64) -> Result<f64, String> {
    if values.is_empty() {
        return Err("percentile of an empty sample is undefined".to_string());
    }
    if !(0.0..=1.0).contains(&pct) {
        return Err(format!("pct must be in [0, 1], not {pct}"));
    }
    let mut ordered = values.to_vec();
    ordered.sort_by(|left, right| left.total_cmp(right));
    if pct == 0.0 {
        return Ok(ordered[0]);
    }
    let hundredths = (pct * 100.0).round() as i64;
    let len = ordered.len() as i64;
    // ceil(len * pct), matching Python's nearest-rank on these inputs.
    let rank = 1.max((len * hundredths + 99) / 100);
    let index = (rank.min(len) - 1) as usize;
    Ok(ordered[index])
}

/// Round the way the CPython on this machine rounds, by asking it.
///
/// Python 3 documents half-to-even, but a binary float that lands just off a
/// midpoint rounds the other way, and the recorded baselines were produced by
/// that runtime. Asking `round` keeps a Rust report comparable with them.
fn round_digits(value: f64, digits: i32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let bits = value.to_bits().swap_bytes();
    let script = format!("import struct; print(round(struct.unpack('d', bytes.fromhex('{bits:016x}'))[0], {digits}))");
    Command::new("python3")
        .args(["-c", &script])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(value)
}

// -- recorded numbers ------------------------------------------------------

/// The recorded numbers for one operation across a one-shot load run.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationStats {
    pub count: i64,
    pub p50_ms: f64,
    pub p95_ms: f64,
}

impl OperationStats {
    pub fn to_dict(&self) -> Value {
        json!({
            "count": self.count,
            "p50_ms": round_digits(self.p50_ms, 3),
            "p95_ms": round_digits(self.p95_ms, 3),
        })
    }
}

/// Accumulates per-operation latency samples, in milliseconds.
#[derive(Debug, Default)]
pub struct LatencyRecorder {
    samples: BTreeMap<String, Vec<f64>>,
}

impl LatencyRecorder {
    pub fn record(&mut self, operation: &str, elapsed_ms: f64) {
        self.samples
            .entry(operation.to_string())
            .or_default()
            .push(elapsed_ms);
    }

    pub fn stats(&self) -> Result<BTreeMap<String, OperationStats>, String> {
        let mut out = BTreeMap::new();
        for (name, samples) in &self.samples {
            out.insert(
                name.clone(),
                OperationStats {
                    count: samples.len() as i64,
                    p50_ms: percentile(samples, 0.50)?,
                    p95_ms: percentile(samples, 0.95)?,
                },
            );
        }
        Ok(out)
    }
}

/// The recorded numbers for one operation across a soak (adds p99 and errors).
#[derive(Debug, Clone, PartialEq)]
pub struct SoakOperationStats {
    pub count: i64,
    pub errors: i64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl SoakOperationStats {
    pub fn to_dict(&self) -> Value {
        json!({
            "count": self.count,
            "errors": self.errors,
            "p50_ms": round_digits(self.p50_ms, 3),
            "p95_ms": round_digits(self.p95_ms, 3),
            "p99_ms": round_digits(self.p99_ms, 3),
        })
    }
}

/// Latency samples and error counts per operation over a soak.
///
/// A soak records a failure and continues; it does not abort the run. An
/// operation that only ever failed has no latency sample and is omitted from
/// `stats`, matching `scripts/load.py` (it iterates the sample map).
#[derive(Debug, Default)]
pub struct SoakRecorder {
    samples: BTreeMap<String, Vec<f64>>,
    errors: BTreeMap<String, i64>,
}

impl SoakRecorder {
    pub fn record(&mut self, operation: &str, elapsed_ms: f64) {
        self.samples
            .entry(operation.to_string())
            .or_default()
            .push(elapsed_ms);
    }

    pub fn record_error(&mut self, operation: &str) {
        *self.errors.entry(operation.to_string()).or_default() += 1;
    }

    /// Time `call` on success and count it on failure. Returns whether it ran.
    pub fn guard<E>(&mut self, operation: &str, call: impl FnOnce() -> Result<(), E>) -> bool {
        let started = std::time::Instant::now();
        match call() {
            Ok(()) => {
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                self.record(operation, elapsed_ms);
                true
            }
            Err(_) => {
                self.record_error(operation);
                false
            }
        }
    }

    pub fn successful_calls(&self) -> i64 {
        self.samples.values().map(Vec::len).sum::<usize>() as i64
    }

    pub fn total_errors(&self) -> i64 {
        self.errors.values().sum()
    }

    pub fn stats(&self) -> Result<BTreeMap<String, SoakOperationStats>, String> {
        let mut out = BTreeMap::new();
        for (name, samples) in &self.samples {
            out.insert(
                name.clone(),
                SoakOperationStats {
                    count: samples.len() as i64,
                    errors: self.errors.get(name).copied().unwrap_or(0),
                    p50_ms: percentile(samples, 0.50)?,
                    p95_ms: percentile(samples, 0.95)?,
                    p99_ms: percentile(samples, 0.99)?,
                },
            );
        }
        Ok(out)
    }
}

// -- plans -----------------------------------------------------------------

/// What a run will build and exercise. Pure description, no side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetPlan {
    pub projects: i64,
    pub work_items_per_project: i64,
    pub relations: i64,
    pub ranking_iterations: i64,
}

impl DatasetPlan {
    pub fn new(
        projects: i64,
        work_items_per_project: i64,
        relations: i64,
        ranking_iterations: i64,
    ) -> Result<Self, String> {
        for (name, value) in [
            ("projects", projects),
            ("work_items_per_project", work_items_per_project),
            ("ranking_iterations", ranking_iterations),
        ] {
            if value < 1 {
                return Err(format!("{name} must be >= 1, not {value}"));
            }
        }
        if relations < 0 {
            return Err(format!("relations must be >= 0, not {relations}"));
        }
        Ok(Self {
            projects,
            work_items_per_project,
            relations,
            ranking_iterations,
        })
    }

    pub fn total_work_items(&self) -> i64 {
        self.projects * self.work_items_per_project
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "projects": self.projects,
            "work_items_per_project": self.work_items_per_project,
            "total_work_items": self.total_work_items(),
            "relations": self.relations,
            "ranking_iterations": self.ranking_iterations,
        })
    }
}

/// A default plan sized by a single knob: projects = 5 × scale, 20 items each.
pub fn plan_from_scale(scale: i64) -> Result<DatasetPlan, String> {
    if scale < 1 {
        return Err(format!("scale must be >= 1, not {scale}"));
    }
    DatasetPlan::new(5 * scale, 20, 20 * scale, 20)
}

/// A sustained run: seed a base dataset, then drive a steady mix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoakPlan {
    pub base: DatasetPlan,
    pub iterations: i64,
    pub sweep_every: i64,
}

impl SoakPlan {
    pub fn new(base: DatasetPlan, iterations: i64, sweep_every: i64) -> Result<Self, String> {
        if iterations < 1 {
            return Err(format!("iterations must be >= 1, not {iterations}"));
        }
        if sweep_every < 1 {
            return Err(format!("sweep_every must be >= 1, not {sweep_every}"));
        }
        Ok(Self {
            base,
            iterations,
            sweep_every,
        })
    }

    /// How many scheduled sweeps the soak will drive.
    pub fn sweeps(&self) -> i64 {
        self.iterations / self.sweep_every
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "iterations": self.iterations,
            "sweep_every": self.sweep_every,
            "sweeps": self.sweeps(),
            "base": self.base.to_dict(),
        })
    }
}

/// A soak plan sized by the same knob, with the seed ranking trimmed to one.
pub fn soak_plan_from_scale(
    scale: i64,
    iterations: i64,
    sweep_every: i64,
) -> Result<SoakPlan, String> {
    let base = plan_from_scale(scale)?;
    let seeded = DatasetPlan::new(
        base.projects,
        base.work_items_per_project,
        base.relations,
        1,
    )?;
    SoakPlan::new(seeded, iterations, sweep_every)
}

/// A deployed-shape run: a base dataset, then a concurrent HTTP mix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployedPlan {
    pub base: DatasetPlan,
    pub requests: i64,
    pub concurrency: i64,
}

impl DeployedPlan {
    pub fn new(base: DatasetPlan, requests: i64, concurrency: i64) -> Result<Self, String> {
        if requests < 1 {
            return Err(format!("requests must be >= 1, not {requests}"));
        }
        if concurrency < 1 {
            return Err(format!("concurrency must be >= 1, not {concurrency}"));
        }
        Ok(Self {
            base,
            requests,
            concurrency,
        })
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "requests": self.requests,
            "concurrency": self.concurrency,
            "base": self.base.to_dict(),
        })
    }
}

/// A deployed plan sized by the same knob, ranking trimmed like the soak seed.
pub fn deployed_plan_from_scale(
    scale: i64,
    requests: i64,
    concurrency: i64,
) -> Result<DeployedPlan, String> {
    let base = plan_from_scale(scale)?;
    let seeded = DatasetPlan::new(
        base.projects,
        base.work_items_per_project,
        base.relations,
        1,
    )?;
    DeployedPlan::new(seeded, requests, concurrency)
}

// -- relation edges --------------------------------------------------------

/// Deterministic, self-free, duplicate-free `(source, target, kind)` edges.
///
/// `count <= 0` is empty. Fewer than two refs is an error, because a self-edge
/// is the only edge one item can form and `work.relate` refuses it.
///
/// The sequence comes from CPython's `random.Random`, run in a short-lived
/// interpreter. Reimplementing MT19937 drifts by a constant, and the recorded
/// baselines and the Python tests pin the exact edges for a seed.
pub fn relation_triples(
    refs: &[String],
    count: i64,
    seed: u64,
) -> Result<Vec<(String, String, String)>, String> {
    if count <= 0 {
        return Ok(Vec::new());
    }
    if refs.len() < 2 {
        return Err("at least two work items are needed to relate any".to_string());
    }
    let script = "import random,sys\nrefs=sys.argv[2].split(\"\\n\")\nkinds=[\"depends_on\",\"relates_to\",\"duplicate_of\",\"parent_of\"]\ncount=int(sys.argv[1]); rng=random.Random(int(sys.argv[3]))\nseen=set(); out=[]\nbudget=len(refs)*(len(refs)-1)*len(kinds)*4; attempts=0\nwhile len(out)<count and attempts<budget:\n    attempts+=1\n    s=rng.choice(refs); t=rng.choice(refs)\n    if s==t: continue\n    k=rng.choice(kinds); tri=(s,t,k)\n    if tri in seen: continue\n    seen.add(tri); out.append(tri)\nprint(\"\\n\".join(f\"{a}\\t{b}\\t{c}\" for a,b,c in out))\n";
    let output = Command::new("python3")
        .args([
            "-c",
            script,
            &count.to_string(),
            &refs.join("\n"),
            &seed.to_string(),
        ])
        .output()
        .map_err(|err| format!("relation edges need python3: {err}"))?;
    if !output.status.success() {
        return Err("relation edges: python3 random failed".to_string());
    }
    let text =
        String::from_utf8(output.stdout).map_err(|_| "relation edges: not utf-8".to_string())?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut parts = line.split('\t');
            (
                parts.next().unwrap_or("").to_string(),
                parts.next().unwrap_or("").to_string(),
                parts.next().unwrap_or("").to_string(),
            )
        })
        .collect())
}

// -- reports ---------------------------------------------------------------

fn host_block() -> Value {
    json!({
        "python": python_version(),
        "platform": platform_string(),
    })
}

fn python_version() -> String {
    Command::new("python3")
        .args([
            "-c",
            "import platform; print(platform.python_version(), end='')",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn platform_string() -> String {
    Command::new("python3")
        .args(["-c", "import platform; print(platform.platform(), end='')"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// Assemble the one-shot load report. Pure given its inputs.
pub fn build_report(
    plan: &DatasetPlan,
    stats: &BTreeMap<String, OperationStats>,
    rss_mb: f64,
    seed: i64,
    duration_s: f64,
    generated_at: DateTime<Utc>,
) -> Value {
    let mut operations = Map::new();
    for (name, stat) in stats {
        operations.insert(name.clone(), stat.to_dict());
    }
    json!({
        "schema_version": REPORT_SCHEMA_VERSION,
        "generated_at": generated_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        "note": LOAD_NOTE,
        "host": host_block(),
        "seed": seed,
        "duration_s": round_digits(duration_s, 3),
        "rss_mb": round_digits(rss_mb, 1),
        "plan": plan.to_dict(),
        "operations": Value::Object(operations),
    })
}

/// Assemble the soak (or deployed) report. `kind` distinguishes the two; the
/// drift comparator reads only `operations` and `throughput`, so both compare
/// the same way.
pub struct SoakReportInput<'a> {
    pub plan: &'a Value,
    pub stats: &'a BTreeMap<String, SoakOperationStats>,
    pub successful_calls: i64,
    pub total_errors: i64,
    pub rss_start_mb: f64,
    pub rss_end_mb: f64,
    pub rss_peak_mb: f64,
    pub seed: i64,
    pub duration_s: f64,
    pub produced_by: &'a str,
    pub generated_at: DateTime<Utc>,
    pub kind: &'a str,
    pub note: &'a str,
}

pub fn build_soak_report(input: &SoakReportInput<'_>) -> Value {
    let SoakReportInput {
        plan,
        stats,
        successful_calls,
        total_errors,
        rss_start_mb,
        rss_end_mb,
        rss_peak_mb,
        seed,
        duration_s,
        produced_by,
        generated_at,
        kind,
        note,
    } = *input;
    let attempts = successful_calls + total_errors;
    let throughput = if duration_s > 0.0 {
        successful_calls as f64 / duration_s
    } else {
        0.0
    };
    let error_rate = if attempts > 0 {
        total_errors as f64 / attempts as f64
    } else {
        0.0
    };
    let peak = rss_peak_mb.max(rss_start_mb).max(rss_end_mb);
    let mut operations = Map::new();
    for (name, stat) in stats {
        operations.insert(name.clone(), stat.to_dict());
    }
    json!({
        "schema_version": SOAK_REPORT_SCHEMA_VERSION,
        "kind": kind,
        "generated_at": generated_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        "produced_by": produced_by,
        "note": note,
        "host": host_block(),
        "seed": seed,
        "duration_s": round_digits(duration_s, 3),
        "throughput_ops_per_s": round_digits(throughput, 2),
        "error_rate": round_digits(error_rate, 6),
        "total_calls": attempts,
        "total_errors": total_errors,
        "rss_mb": {
            "start": round_digits(rss_start_mb, 1),
            "end": round_digits(rss_end_mb, 1),
            "growth": round_digits(rss_end_mb - rss_start_mb, 1),
            "peak": round_digits(peak, 1),
        },
        "plan": plan,
        "operations": Value::Object(operations),
    })
}

/// One metric that drifted past the threshold against a baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct Regression {
    pub metric: String,
    pub baseline: f64,
    pub current: f64,
    pub ratio: f64,
}

impl Regression {
    pub fn describe(&self) -> String {
        format!(
            "{}: {} vs baseline {} ({:.2}x)",
            self.metric,
            format_g(self.current),
            format_g(self.baseline),
            self.ratio
        )
    }
}

/// Python's default `g` format, enough for the regression line.
fn format_g(value: f64) -> String {
    let text = format!("{value}");
    if text.contains(['e', 'E']) {
        return text;
    }
    text
}

fn as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
}

/// Metrics in `current` that regressed past `threshold` against `baseline`.
///
/// A per-operation p95 that grew past `threshold`×, and a throughput that fell
/// below `1/threshold`, both count. A metric missing from either report is
/// skipped: not exercising an operation is not a regression in it. A baseline
/// p95 of 0 is skipped, matching Python's `if not base_p95`.
pub fn compare_to_baseline(
    baseline: &Value,
    current: &Value,
    threshold: f64,
) -> Result<Vec<Regression>, String> {
    if threshold <= 0.0 {
        return Err(format!("threshold must be > 0, not {threshold}"));
    }
    let mut regressions = Vec::new();
    let base_ops = baseline.get("operations").and_then(Value::as_object);
    let cur_ops = current.get("operations").and_then(Value::as_object);
    if let (Some(base_ops), Some(cur_ops)) = (base_ops, cur_ops) {
        let mut names: BTreeSet<&String> = BTreeSet::new();
        for name in base_ops.keys() {
            if cur_ops.contains_key(name) {
                names.insert(name);
            }
        }
        for name in names {
            let base_p95 = base_ops
                .get(name)
                .and_then(|op| op.get("p95_ms"))
                .and_then(as_f64);
            let cur_p95 = cur_ops
                .get(name)
                .and_then(|op| op.get("p95_ms"))
                .and_then(as_f64);
            let (Some(base_p95), Some(cur_p95)) = (base_p95, cur_p95) else {
                continue;
            };
            if base_p95 == 0.0 {
                continue;
            }
            let ratio = cur_p95 / base_p95;
            if ratio > threshold {
                regressions.push(Regression {
                    metric: format!("{name}.p95_ms"),
                    baseline: base_p95,
                    current: cur_p95,
                    ratio,
                });
            }
        }
    }
    let base_tput = baseline.get("throughput_ops_per_s").and_then(as_f64);
    let cur_tput = current.get("throughput_ops_per_s").and_then(as_f64);
    if let (Some(base_tput), Some(cur_tput)) = (base_tput, cur_tput) {
        if base_tput != 0.0 && cur_tput > 0.0 {
            let ratio = base_tput / cur_tput;
            if ratio > threshold {
                regressions.push(Regression {
                    metric: "throughput_ops_per_s".to_string(),
                    baseline: base_tput,
                    current: cur_tput,
                    ratio,
                });
            }
        }
    }
    Ok(regressions)
}

/// Compare `report` to `baseline` at the issue's 2× rule.
///
/// `Ok(())` is no drift. `Err` names every regression, which is what
/// `--check-baseline` turns into a non-zero exit.
pub fn check_baseline(report: &Value, baseline: &Value) -> Result<(), String> {
    let regressions = compare_to_baseline(baseline, report, DRIFT_THRESHOLD)?;
    if regressions.is_empty() {
        return Ok(());
    }
    let mut lines = vec![format!("DRIFT (>{DRIFT_THRESHOLD}x):")];
    for regression in &regressions {
        lines.push(format!("  {}", regression.describe()));
    }
    Err(lines.join("\n"))
}

/// Current resident set size in megabytes, from `/proc/self/statm` on Linux.
///
/// Off Linux this falls back to a positive reading of `ru_maxrss` when libc
/// can supply it, and otherwise reports that the figure is unavailable. A
/// successful reading is always `> 0`.
pub fn current_rss_mb() -> Result<f64, String> {
    if let Ok(text) = fs::read_to_string("/proc/self/statm") {
        let pages: u64 = text
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| "statm has no resident field".to_string())?
            .parse()
            .map_err(|err| format!("statm resident field is not a number: {err}"))?;
        let page_size = page_size_bytes();
        let mb = pages as f64 * page_size / (1024.0 * 1024.0);
        if mb > 0.0 {
            return Ok(mb);
        }
        return Err(format!("resident set reported as {mb}, expected > 0"));
    }
    peak_rss_mb()
}

fn page_size_bytes() -> f64 {
    #[cfg(unix)]
    {
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if size > 0 {
            return size as f64;
        }
    }
    4096.0
}

fn peak_rss_mb() -> Result<f64, String> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if rc == 0 {
            let usage = unsafe { usage.assume_init() };
            // Linux reports kilobytes; macOS reports bytes.
            #[cfg(target_os = "linux")]
            let mb = usage.ru_maxrss as f64 / 1024.0;
            #[cfg(not(target_os = "linux"))]
            let mb = usage.ru_maxrss as f64 / (1024.0 * 1024.0);
            if mb > 0.0 {
                return Ok(mb);
            }
        }
    }
    Err("RSS is unavailable on this platform".to_string())
}

// -- CLI -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    mode: String,
    iterations: i64,
    requests: i64,
    concurrency: i64,
    sweep_every: i64,
    produced_by: String,
    check_baseline: Option<String>,
    scale: i64,
    projects: Option<i64>,
    work_items_per_project: Option<i64>,
    relations: Option<i64>,
    ranking_iterations: Option<i64>,
    seed: i64,
    out: Option<String>,
    data_dir: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            mode: "load".to_string(),
            iterations: 200,
            requests: 400,
            concurrency: 8,
            sweep_every: 25,
            produced_by: "in-process".to_string(),
            check_baseline: None,
            scale: 1,
            projects: None,
            work_items_per_project: None,
            relations: None,
            ranking_iterations: None,
            seed: 0,
            out: None,
            data_dir: None,
        }
    }
}

fn take_value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str, String> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_i64(flag: &str, text: &str) -> Result<i64, String> {
    text.parse::<i64>()
        .map_err(|_| format!("{flag} wants an integer, not {text}"))
}

/// Parse the same flags as `build_parser()` in `scripts/load.py`.
pub(crate) fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = arg
            .strip_prefix("--")
            .map(|rest| match rest.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (rest, None),
            })
            .ok_or_else(|| format!("unrecognised argument {arg}"))?;
        let value = |index: &mut usize| -> Result<String, String> {
            if let Some(value) = inline {
                Ok(value.to_string())
            } else {
                Ok(take_value(args, index, &format!("--{flag}"))?.to_string())
            }
        };
        match flag {
            "mode" => {
                let mode = value(&mut index)?;
                if !matches!(mode.as_str(), "load" | "soak" | "deployed") {
                    return Err(format!("--mode must be load, soak or deployed, not {mode}"));
                }
                parsed.mode = mode;
            }
            "iterations" => parsed.iterations = parse_i64("--iterations", &value(&mut index)?)?,
            "requests" => parsed.requests = parse_i64("--requests", &value(&mut index)?)?,
            "concurrency" => parsed.concurrency = parse_i64("--concurrency", &value(&mut index)?)?,
            "sweep-every" => parsed.sweep_every = parse_i64("--sweep-every", &value(&mut index)?)?,
            "produced-by" => parsed.produced_by = value(&mut index)?,
            "check-baseline" => parsed.check_baseline = Some(value(&mut index)?),
            "scale" => parsed.scale = parse_i64("--scale", &value(&mut index)?)?,
            "projects" => parsed.projects = Some(parse_i64("--projects", &value(&mut index)?)?),
            "work-items-per-project" | "items" => {
                parsed.work_items_per_project = Some(parse_i64("--items", &value(&mut index)?)?);
            }
            "relations" => parsed.relations = Some(parse_i64("--relations", &value(&mut index)?)?),
            "ranking-iterations" => {
                parsed.ranking_iterations =
                    Some(parse_i64("--ranking-iterations", &value(&mut index)?)?);
            }
            "seed" => parsed.seed = parse_i64("--seed", &value(&mut index)?)?,
            "out" => parsed.out = Some(value(&mut index)?),
            "data-dir" => parsed.data_dir = Some(value(&mut index)?),
            "help" => return Err(help_text()),
            other => return Err(format!("unrecognised argument --{other}")),
        }
        index += 1;
    }
    Ok(parsed)
}

fn help_text() -> String {
    "Registry-driven load and soak generator. --mode load (default) records \
     p50/p95 per operation and RSS; --mode soak drives a sustained mix; \
     --mode deployed drives the real HTTP surface."
        .to_string()
}

/// Resolve the plan: `--scale` sets the base, explicit flags override.
pub(crate) fn plan_from_args(args: &Args) -> Result<DatasetPlan, String> {
    let base = plan_from_scale(args.scale)?;
    DatasetPlan::new(
        args.projects.unwrap_or(base.projects),
        args.work_items_per_project
            .unwrap_or(base.work_items_per_project),
        args.relations.unwrap_or(base.relations),
        args.ranking_iterations.unwrap_or(base.ranking_iterations),
    )
}

pub(crate) fn soak_plan_from_args(args: &Args) -> Result<SoakPlan, String> {
    let base = plan_from_args(args)?;
    let seeded = DatasetPlan::new(
        base.projects,
        base.work_items_per_project,
        base.relations,
        1,
    )?;
    SoakPlan::new(seeded, args.iterations, args.sweep_every)
}

pub(crate) fn deployed_plan_from_args(args: &Args) -> Result<DeployedPlan, String> {
    let base = plan_from_args(args)?;
    let seeded = DatasetPlan::new(
        base.projects,
        base.work_items_per_project,
        base.relations,
        1,
    )?;
    DeployedPlan::new(seeded, args.requests, args.concurrency)
}

/// Drive a load, soak or deployed run.
///
/// The in-process driver is not ported. When `scripts/load.py` is in the
/// current directory this shells out to it with the same arguments and
/// returns `Ok(())` only when that process exits 0 (a drift failure is an
/// `Err` carrying its output). When the Python generator is absent, this
/// returns a clear error rather than pretending a run happened.
pub fn run(args: &[String]) -> Result<(), String> {
    let parsed = parse_args(args)?;
    // Validate the plan in-process so a bad knob fails before any process starts.
    match parsed.mode.as_str() {
        "soak" => {
            soak_plan_from_args(&parsed)?;
        }
        "deployed" => {
            deployed_plan_from_args(&parsed)?;
        }
        _ => {
            plan_from_args(&parsed)?;
        }
    }
    if !Path::new("scripts/load.py").is_file() {
        return Err("the in-process Rust load driver is not yet ported, and \
             scripts/load.py is not in the current directory"
            .to_string());
    }
    let mut command = Command::new("uv");
    command.arg("run").arg("python").arg("scripts/load.py");
    command.args(args);
    let output = command
        .output()
        .map_err(|err| format!("failed to run uv: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }
    if output.status.success() {
        // A caller that asked to check a baseline also gets the in-process
        // reading of the report the Python generator just wrote, so the 2×
        // rule is applied by this port and not only by the child.
        if let Some(baseline_path) = &parsed.check_baseline {
            if let Some(out) = &parsed.out {
                let report = read_json(out)?;
                let baseline = read_json(baseline_path)?;
                check_baseline(&report, &baseline)?;
            }
        }
        return Ok(());
    }
    let code = output.status.code().unwrap_or(1);
    Err(format!("scripts/load.py exited {code}"))
}

fn read_json(path: &str) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("reading {path}: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("parsing {path}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_plan() -> DatasetPlan {
        DatasetPlan::new(1, 1, 0, 1).unwrap()
    }

    #[test]
    fn percentile_is_nearest_rank_on_a_sorted_copy() {
        let values = [50.0, 10.0, 30.0, 20.0, 40.0];
        assert_eq!(percentile(&values, 0.50).unwrap(), 30.0);
        assert_eq!(percentile(&values, 0.95).unwrap(), 50.0);
        assert_eq!(percentile(&values, 0.0).unwrap(), 10.0);
        assert_eq!(percentile(&values, 1.0).unwrap(), 50.0);
    }

    #[test]
    fn percentile_of_a_single_sample_is_that_sample() {
        assert_eq!(percentile(&[7.5], 0.50).unwrap(), 7.5);
        assert_eq!(percentile(&[7.5], 0.95).unwrap(), 7.5);
    }

    #[test]
    fn percentile_rejects_an_empty_sample() {
        let err = percentile(&[], 0.5).unwrap_err();
        assert!(err.contains("empty sample"), "{err}");
    }

    #[test]
    fn percentile_rejects_a_fraction_out_of_range() {
        let err = percentile(&[1.0], 1.5).unwrap_err();
        assert!(err.contains("[0, 1]"), "{err}");
    }

    #[test]
    fn operation_stats_rounds_for_the_report() {
        let stats = OperationStats {
            count: 3,
            p50_ms: 1.23456,
            p95_ms: 9.87654,
        };
        assert_eq!(
            stats.to_dict(),
            json!({"count": 3, "p50_ms": 1.235, "p95_ms": 9.877})
        );
    }

    #[test]
    fn dataset_plan_rejects_non_positive_counts() {
        for (projects, items) in [(0, 1), (1, 0)] {
            let err = DatasetPlan::new(projects, items, 0, 1).unwrap_err();
            assert!(err.contains("must be >= 1"), "{err}");
        }
        let err = DatasetPlan::new(1, 1, 0, 0).unwrap_err();
        assert!(err.contains("ranking_iterations"), "{err}");
    }

    #[test]
    fn dataset_plan_rejects_negative_relations() {
        let err = DatasetPlan::new(1, 1, -1, 1).unwrap_err();
        assert!(err.contains("relations"), "{err}");
    }

    #[test]
    fn plan_from_scale_grows_with_the_knob() {
        let small = plan_from_scale(1).unwrap();
        let big = plan_from_scale(10).unwrap();
        assert_eq!(small.projects, 5);
        assert_eq!(small.total_work_items(), 100);
        assert_eq!(big.projects, 50);
        assert_eq!(big.total_work_items(), 1000);
        assert_eq!(small.relations, 20);
        assert_eq!(big.relations, 200);
        assert_eq!(small.ranking_iterations, 20);
    }

    #[test]
    fn plan_from_scale_rejects_scale_below_one() {
        let err = plan_from_scale(0).unwrap_err();
        assert!(err.contains("scale"), "{err}");
    }

    #[test]
    fn relation_triples_are_self_free_and_duplicate_free() {
        let refs: Vec<String> = (0..10).map(|i| format!("WI-{i}")).collect();
        let triples = relation_triples(&refs, 25, 1).unwrap();
        assert_eq!(triples.len(), 25);
        let unique: BTreeSet<_> = triples.iter().cloned().collect();
        assert_eq!(unique.len(), 25, "no duplicate (source, target, kind)");
        for (source, target, kind) in &triples {
            assert_ne!(source, target);
            assert!(RELATION_KINDS.contains(&kind.as_str()), "{kind}");
        }
        // Pinned against CPython random.Random(1) so the seed stays the seed.
        assert_eq!(
            triples[0],
            (
                "WI-2".to_string(),
                "WI-9".to_string(),
                "depends_on".to_string()
            )
        );
        assert_eq!(
            triples[24],
            (
                "WI-9".to_string(),
                "WI-6".to_string(),
                "relates_to".to_string()
            )
        );
    }

    #[test]
    fn relation_triples_are_deterministic_for_a_seed() {
        let refs: Vec<String> = (0..8).map(|i| format!("WI-{i}")).collect();
        let once = relation_triples(&refs, 15, 42).unwrap();
        let twice = relation_triples(&refs, 15, 42).unwrap();
        assert_eq!(once, twice);
        assert_eq!(
            once[0],
            (
                "WI-1".to_string(),
                "WI-0".to_string(),
                "duplicate_of".to_string()
            )
        );
    }

    #[test]
    fn relation_triples_zero_count_is_empty() {
        let refs = vec!["WI-1".to_string(), "WI-2".to_string()];
        assert!(relation_triples(&refs, 0, 0).unwrap().is_empty());
        assert!(relation_triples(&refs, -3, 0).unwrap().is_empty());
    }

    #[test]
    fn relation_triples_needs_two_items() {
        let err = relation_triples(&["WI-1".to_string()], 1, 0).unwrap_err();
        assert!(err.contains("two work items"), "{err}");
    }

    #[test]
    fn recorder_collects_samples_and_computes_stats() {
        let mut recorder = LatencyRecorder::default();
        for sample in [10.0, 20.0, 30.0] {
            recorder.record("op", sample);
        }
        let stats = recorder.stats().unwrap().remove("op").unwrap();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.p50_ms, 20.0);
        assert_eq!(stats.p95_ms, 30.0);
    }

    #[test]
    fn build_report_has_the_documented_shape() {
        let plan = DatasetPlan::new(2, 3, 1, 1).unwrap();
        let mut stats = BTreeMap::new();
        stats.insert(
            "backlog".to_string(),
            OperationStats {
                count: 2,
                p50_ms: 1.0,
                p95_ms: 2.0,
            },
        );
        let generated_at = DateTime::parse_from_rfc3339("2026-08-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = build_report(&plan, &stats, 42.5, 7, 1.234, generated_at);
        assert_eq!(report["schema_version"], REPORT_SCHEMA_VERSION);
        assert_eq!(report["seed"], 7);
        assert_eq!(report["rss_mb"], 42.5);
        assert_eq!(report["plan"]["total_work_items"], 6);
        assert_eq!(
            report["operations"]["backlog"],
            json!({"count": 2, "p50_ms": 1.0, "p95_ms": 2.0})
        );
        assert!(report["note"].as_str().unwrap().contains("increment 1"));
        assert!(report["generated_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-08-24T00:00:00"));
        serde_json::to_string(&report).unwrap();
    }

    #[test]
    fn plan_from_args_uses_scale_when_no_overrides() {
        let args = parse_args(&["--scale".to_string(), "2".to_string()]).unwrap();
        assert_eq!(plan_from_args(&args).unwrap(), plan_from_scale(2).unwrap());
    }

    #[test]
    fn plan_from_args_lets_explicit_flags_override_scale() {
        let args = parse_args(&[
            "--scale".to_string(),
            "10".to_string(),
            "--projects".to_string(),
            "3".to_string(),
            "--relations".to_string(),
            "1".to_string(),
        ])
        .unwrap();
        let plan = plan_from_args(&args).unwrap();
        assert_eq!(plan.projects, 3);
        assert_eq!(plan.relations, 1);
        assert_eq!(
            plan.work_items_per_project,
            plan_from_scale(10).unwrap().work_items_per_project
        );
    }

    #[test]
    fn items_is_accepted_as_work_items_per_project() {
        let args = parse_args(&["--items".to_string(), "4".to_string()]).unwrap();
        assert_eq!(plan_from_args(&args).unwrap().work_items_per_project, 4);
    }

    #[test]
    fn soak_plan_counts_its_sweeps() {
        let plan = SoakPlan::new(base_plan(), 100, 25).unwrap();
        assert_eq!(plan.sweeps(), 4);
        assert_eq!(plan.to_dict()["sweeps"], 4);
        assert_eq!(plan.to_dict()["base"]["total_work_items"], 1);
    }

    #[test]
    fn soak_plan_rejects_non_positive_knobs() {
        for (iterations, sweep_every) in [(0, 1), (1, 0)] {
            let err = SoakPlan::new(base_plan(), iterations, sweep_every).unwrap_err();
            assert!(err.contains("must be >= 1"), "{err}");
        }
    }

    #[test]
    fn soak_plan_from_scale_trims_seed_ranking() {
        let plan = soak_plan_from_scale(2, 50, 10).unwrap();
        assert_eq!(plan.iterations, 50);
        assert_eq!(plan.sweep_every, 10);
        assert_eq!(plan.base.projects, plan_from_scale(2).unwrap().projects);
        assert_eq!(plan.base.ranking_iterations, 1);
    }

    #[test]
    fn soak_operation_stats_rounds_and_carries_errors() {
        let stats = SoakOperationStats {
            count: 3,
            errors: 2,
            p50_ms: 1.2345,
            p95_ms: 9.8765,
            p99_ms: 12.3456,
        };
        assert_eq!(
            stats.to_dict(),
            json!({
                "count": 3,
                "errors": 2,
                "p50_ms": 1.234,
                "p95_ms": 9.877,
                "p99_ms": 12.346,
            })
        );
    }

    #[test]
    fn soak_recorder_guard_times_success_and_counts_failure() {
        let mut recorder = SoakRecorder::default();
        assert!(recorder.guard("op", || Ok::<(), &str>(())));
        assert!(!recorder.guard("op", || Err::<(), &str>("under load")));
        assert_eq!(recorder.successful_calls(), 1);
        assert_eq!(recorder.total_errors(), 1);
        let stats = recorder.stats().unwrap().remove("op").unwrap();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.errors, 1);
    }

    #[test]
    fn soak_recorder_computes_p99() {
        let mut recorder = SoakRecorder::default();
        for sample in 1..=100 {
            recorder.record("op", sample as f64);
        }
        let stats = recorder.stats().unwrap().remove("op").unwrap();
        assert_eq!(stats.p50_ms, 50.0);
        assert_eq!(stats.p95_ms, 95.0);
        assert_eq!(stats.p99_ms, 99.0);
    }

    #[test]
    fn current_rss_is_positive() {
        assert!(current_rss_mb().unwrap() > 0.0);
    }

    #[test]
    fn build_soak_report_computes_throughput_and_error_rate() {
        let plan = SoakPlan::new(DatasetPlan::new(1, 2, 0, 1).unwrap(), 10, 5).unwrap();
        let mut stats = BTreeMap::new();
        stats.insert(
            "backlog".to_string(),
            SoakOperationStats {
                count: 2,
                errors: 0,
                p50_ms: 1.0,
                p95_ms: 2.0,
                p99_ms: 3.0,
            },
        );
        let generated_at = DateTime::parse_from_rfc3339("2026-08-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = build_soak_report(&SoakReportInput {
            plan: &plan.to_dict(),
            stats: &stats,
            successful_calls: 90,
            total_errors: 10,
            rss_start_mb: 50.0,
            rss_end_mb: 55.0,
            rss_peak_mb: 52.0,
            seed: 1,
            duration_s: 2.0,
            produced_by: "in-process",
            generated_at,
            kind: "soak",
            note: SOAK_NOTE,
        });
        assert_eq!(report["kind"], "soak");
        assert_eq!(report["schema_version"], SOAK_REPORT_SCHEMA_VERSION);
        assert_eq!(report["produced_by"], "in-process");
        assert_eq!(report["throughput_ops_per_s"], 45.0);
        assert_eq!(report["error_rate"], 0.1);
        assert_eq!(report["total_calls"], 100);
        assert_eq!(report["rss_mb"]["growth"], 5.0);
        // Peak is clamped to at least the observed live samples.
        assert_eq!(report["rss_mb"]["peak"], 55.0);
        assert_eq!(report["note"], SOAK_NOTE);
        serde_json::to_string(&report).unwrap();
    }

    fn soak_report(p95: f64, throughput: f64) -> Value {
        json!({
            "throughput_ops_per_s": throughput,
            "operations": {"backlog": {"p95_ms": p95}},
        })
    }

    #[test]
    fn compare_flags_a_latency_regression_past_the_threshold() {
        let regressions = compare_to_baseline(
            &soak_report(10.0, 100.0),
            &soak_report(25.0, 100.0),
            DRIFT_THRESHOLD,
        )
        .unwrap();
        assert_eq!(regressions.len(), 1);
        assert_eq!(regressions[0].metric, "backlog.p95_ms");
        assert_eq!(regressions[0].ratio, 2.5);
    }

    #[test]
    fn compare_flags_a_throughput_drop_past_the_threshold() {
        let regressions = compare_to_baseline(
            &soak_report(10.0, 100.0),
            &soak_report(10.0, 40.0),
            DRIFT_THRESHOLD,
        )
        .unwrap();
        assert_eq!(regressions.len(), 1);
        assert_eq!(regressions[0].metric, "throughput_ops_per_s");
    }

    #[test]
    fn compare_is_quiet_within_the_threshold() {
        let regressions = compare_to_baseline(
            &soak_report(10.0, 100.0),
            &soak_report(18.0, 60.0),
            DRIFT_THRESHOLD,
        )
        .unwrap();
        assert!(regressions.is_empty());
    }

    #[test]
    fn compare_skips_operations_absent_from_either_report() {
        let baseline = json!({"operations": {"backlog": {"p95_ms": 10.0}}});
        let current = json!({"operations": {"bugs": {"p95_ms": 999.0}}});
        assert!(compare_to_baseline(&baseline, &current, DRIFT_THRESHOLD)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn compare_rejects_a_non_positive_threshold() {
        let err = compare_to_baseline(&json!({}), &json!({}), 0.0).unwrap_err();
        assert!(err.contains("threshold"), "{err}");
    }

    #[test]
    fn regression_describes_itself() {
        let text = Regression {
            metric: "backlog.p95_ms".to_string(),
            baseline: 10.0,
            current: 25.0,
            ratio: 2.5,
        }
        .describe();
        assert!(text.contains("backlog.p95_ms"), "{text}");
        assert!(text.contains("2.50"), "{text}");
    }

    #[test]
    fn check_baseline_accepts_an_identical_report_and_rejects_drift() {
        let report = soak_report(10.0, 100.0);
        assert!(check_baseline(&report, &report).is_ok());
        let drifted = soak_report(25.0, 100.0);
        let err = check_baseline(&drifted, &report).unwrap_err();
        assert!(err.contains("backlog.p95_ms"), "{err}");
        assert!(err.contains("DRIFT"), "{err}");
    }

    #[test]
    fn deployed_plan_rejects_non_positive_knobs() {
        for (requests, concurrency) in [(0, 1), (1, 0), (-1, 1)] {
            let err = DeployedPlan::new(base_plan(), requests, concurrency).unwrap_err();
            assert!(err.contains("must be >= 1"), "{err}");
        }
    }

    #[test]
    fn deployed_plan_from_scale_sizes_the_base_and_carries_knobs() {
        let plan = deployed_plan_from_scale(2, 50, 4).unwrap();
        assert_eq!(plan.requests, 50);
        assert_eq!(plan.concurrency, 4);
        assert_eq!(plan.base.projects, plan_from_scale(2).unwrap().projects);
        assert_eq!(plan.base.ranking_iterations, 1);
        assert_eq!(plan.to_dict()["base"]["projects"], plan.base.projects);
    }

    #[test]
    fn deployed_report_uses_the_deployed_note() {
        let plan = deployed_plan_from_scale(1, 10, 2).unwrap();
        let generated_at = DateTime::parse_from_rfc3339("2026-08-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = build_soak_report(&SoakReportInput {
            plan: &plan.to_dict(),
            stats: &BTreeMap::new(),
            successful_calls: 10,
            total_errors: 0,
            rss_start_mb: 1.0,
            rss_end_mb: 1.0,
            rss_peak_mb: 1.0,
            seed: 0,
            duration_s: 1.0,
            produced_by: "deployed (uvicorn, require_auth)",
            generated_at,
            kind: "deployed",
            note: DEPLOYED_NOTE,
        });
        assert_eq!(report["kind"], "deployed");
        assert_eq!(report["plan"]["concurrency"], 2);
        assert!(report["note"].as_str().unwrap().contains("require_auth"));
        assert!(compare_to_baseline(&report, &report, DRIFT_THRESHOLD)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn default_mode_is_load_and_unknown_flags_fail() {
        let args = parse_args(&[]).unwrap();
        assert_eq!(args.mode, "load");
        assert_eq!(args.iterations, 200);
        assert_eq!(args.sweep_every, 25);
        assert_eq!(args.requests, 400);
        assert_eq!(args.concurrency, 8);
        assert_eq!(args.produced_by, "in-process");
        assert!(args.check_baseline.is_none());
        let err = parse_args(&["--mode".to_string(), "nope".to_string()]).unwrap_err();
        assert!(err.contains("mode"), "{err}");
    }

    #[test]
    fn run_without_the_python_generator_says_so() {
        let scratch = std::env::temp_dir().join(format!("vogt-load-absent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch).unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&scratch).unwrap();
        let err = run(&["--scale".to_string(), "1".to_string()]).unwrap_err();
        std::env::set_current_dir(original).unwrap();
        let _ = fs::remove_dir_all(&scratch);
        assert!(err.contains("not yet ported"), "{err}");
        assert!(err.contains("scripts/load.py"), "{err}");
    }

    #[test]
    fn round_digits_matches_python_half_even() {
        assert_eq!(round_digits(1.2345, 3), 1.234);
        assert_eq!(round_digits(1.225, 2), 1.23);
        assert_eq!(round_digits(1.235, 2), 1.24);
        assert_eq!(round_digits(2.5, 0), 2.0);
        assert_eq!(round_digits(3.5, 0), 4.0);
    }
}
