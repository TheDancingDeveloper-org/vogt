//! Running collectors, and recording that they ran. Ports
//! `src/vogt/collectors/sweeper.py`.
//!
//! The coverage record is the point. A sweep row says which collector looked
//! at which projects, when it started and finished, and how it went — and that
//! is what makes "absent" different from "not collected".
//!
//! Failure is contained per project. One unreadable repository makes a sweep
//! `partial` and names the project; it does not cost the estate the others,
//! and it never touches declared data.

use std::collections::BTreeMap;

use crate::core::{Clock, Project, SweepOutcome};
use crate::errors::VogtError;
use crate::storage::interface::ObservedStore;
use crate::storage::observed_types::{ActivityBatch, SweepReport};

use super::base::{ActivityIndexer, Collector, CollectorContext, Finding};
use super::registry::Registered;

pub use super::base::ActivityIndexer as _;

/// Every collector's report from one `sweep` invocation.
pub struct SweepOutcomeSet {
    pub reports: Vec<SweepReport>,
    pub subjects: i64,
    pub dep_refs: i64,
}

impl SweepOutcomeSet {
    pub fn new_count(&self) -> i64 {
        self.reports.iter().map(|report| report.new).sum()
    }

    pub fn unchanged(&self) -> i64 {
        self.reports.iter().map(|report| report.unchanged).sum()
    }

    pub fn worst_outcome(&self) -> &str {
        // Python: the first of ("failed", "partial", "ok") present in the
        // set of report outcomes, else "ok".
        let outcomes: std::collections::BTreeSet<&str> = self
            .reports
            .iter()
            .map(|report| report.outcome.as_str())
            .collect();
        for outcome in ["failed", "partial", "ok"] {
            if outcomes.contains(outcome) {
                return outcome;
            }
        }
        "ok"
    }
}

/// Runs collectors over a scope and writes the coverage records.
pub struct Sweeper<'a, S, C: Clock> {
    store: &'a S,
    ctx: CollectorContext<'a>,
    clock: &'a mut C,
}

impl<'a, S: ObservedStore, C: Clock> Sweeper<'a, S, C> {
    pub fn new(store: &'a S, ctx: CollectorContext<'a>, clock: &'a mut C) -> Self {
        Self { store, ctx, clock }
    }

    /// Run each collector over every project in scope.
    ///
    /// One sweep row per collector, because that is the unit coverage is asked
    /// about.
    pub fn run(
        &mut self,
        collectors: Vec<Registered>,
        projects: &[Project],
    ) -> Result<Vec<SweepReport>, VogtError> {
        let mut reports = Vec::with_capacity(collectors.len());
        for collector in collectors {
            reports.push(self.run_one(collector, projects)?);
        }
        Ok(reports)
    }

    pub fn run_one(
        &mut self,
        collector: Registered,
        projects: &[Project],
    ) -> Result<SweepReport, VogtError> {
        match collector {
            Registered::Indexer(indexer) => self.run_index(indexer),
            Registered::Collector(mut collector) => self.run_projects(collector.as_mut(), projects),
        }
    }

    fn run_projects(
        &mut self,
        collector: &mut dyn Collector,
        projects: &[Project],
    ) -> Result<SweepReport, VogtError> {
        let started = self.clock.now();
        let scope: Vec<String> = projects.iter().map(|project| project.id.clone()).collect();
        let sweep = self.store.begin_sweep(collector.name(), &scope, started)?;

        let mut findings: Vec<Finding> = Vec::new();
        let mut failures: BTreeMap<String, String> = BTreeMap::new();
        for project in projects {
            // Python `findings.extend(collector.collect(...))` keeps every
            // finding the generator yielded before it raised. The project is
            // still a failure, so the sweep stays partial or failed.
            let collected = collector.collect(&mut self.ctx, project, &mut || self.clock.now());
            findings.extend(collected.findings);
            if let Some(err) = collected.error {
                failures.insert(project.slug.clone(), failure_text(&err));
            }
        }

        let stats = self.store.append(&sweep.id, &findings, self.clock.now())?;

        // Only now that the append has committed does the collector's own
        // bookkeeping advance — never ahead of the evidence it summarises.
        // Python reads the clock here only for PostAppend collectors.
        if collector.post_append() {
            collector.after_append(self.clock.now());
        }

        let outcome =
            if !failures.is_empty() && failures.len() == projects.len() && !projects.is_empty() {
                "failed"
            } else if !failures.is_empty() {
                "partial"
            } else {
                "ok"
            };
        let detail = if failures.is_empty() {
            None
        } else {
            Some(
                failures
                    .iter()
                    .map(|(slug, why)| format!("{slug}: {why}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };
        self.store.finish_sweep(
            &sweep.id,
            outcome.parse::<SweepOutcome>().expect("a known outcome"),
            &[
                ("projects", projects.len() as i64),
                ("new", stats.new),
                ("unchanged", stats.unchanged),
                ("failed_projects", failures.len() as i64),
            ],
            self.clock.now(),
            detail.as_deref(),
        )?;
        Ok(SweepReport {
            collector: collector.name().to_string(),
            sweep_id: sweep.id,
            outcome: outcome.to_string(),
            projects: projects.len() as i64,
            new: stats.new,
            unchanged: stats.unchanged,
            failures,
            detail,
        })
    }

    fn run_index(
        &mut self,
        mut indexer: Box<dyn ActivityIndexer>,
    ) -> Result<SweepReport, VogtError> {
        let name = indexer.name().to_string();
        let sweep = self.store.begin_sweep(&name, &[], self.clock.now())?;
        let cursors = match self.store.activity_cursors() {
            Ok(cursors) => cursors,
            Err(err) => return self.index_failed(&name, &sweep.id, &err),
        };
        let batch = match indexer.scan(&cursors) {
            Ok(batch) => batch,
            Err(err) => return self.index_failed(&name, &sweep.id, &err),
        };
        let stats = match self
            .store
            .index_activity(&sweep.id, &batch, self.clock.now())
        {
            Ok(stats) => stats,
            Err(err) => return self.index_failed(&name, &sweep.id, &err),
        };
        self.finish_index(&name, &sweep.id, &batch, stats.calls, stats.results)
    }

    fn index_failed(
        &mut self,
        name: &str,
        sweep_id: &str,
        err: &VogtError,
    ) -> Result<SweepReport, VogtError> {
        let why = failure_text(err);
        self.store.finish_sweep(
            sweep_id,
            SweepOutcome::Failed,
            &[("projects", 0), ("new", 0), ("unchanged", 0)],
            self.clock.now(),
            Some(&why),
        )?;
        Ok(SweepReport {
            collector: name.to_string(),
            sweep_id: sweep_id.to_string(),
            outcome: "failed".to_string(),
            projects: 0,
            new: 0,
            unchanged: 0,
            failures: BTreeMap::new(),
            detail: Some(why),
        })
    }

    fn finish_index(
        &mut self,
        name: &str,
        sweep_id: &str,
        batch: &ActivityBatch,
        calls: i64,
        results: i64,
    ) -> Result<SweepReport, VogtError> {
        let outcome = if batch.skipped.is_empty() {
            "ok"
        } else {
            "partial"
        };
        let detail = if batch.skipped.is_empty() {
            None
        } else {
            Some(
                batch
                    .skipped
                    .iter()
                    .map(|(path, why)| format!("{path}: {why}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };
        self.store.finish_sweep(
            sweep_id,
            outcome.parse::<SweepOutcome>().expect("a known outcome"),
            &[
                ("projects", 0),
                ("new", calls),
                ("unchanged", 0),
                ("results", results),
                ("files", batch.files),
                ("bytes_read", batch.bytes_read),
                ("backlog_bytes", batch.backlog_bytes),
            ],
            self.clock.now(),
            detail.as_deref(),
        )?;
        Ok(SweepReport {
            collector: name.to_string(),
            sweep_id: sweep_id.to_string(),
            outcome: outcome.to_string(),
            projects: 0,
            new: calls,
            unchanged: 0,
            failures: batch.skipped.clone(),
            detail,
        })
    }
}

/// Python records `f"{type(exc).__name__}: {exc}"`. A `CollectorFailed` already
/// carries that text; anything else is a `RuntimeError`.
fn failure_text(err: &VogtError) -> String {
    match err {
        VogtError::CollectorFailed(text) => text.clone(),
        other => format!("RuntimeError: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::{build_context, Built};
    use crate::config::VogtConfig;
    use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
    use crate::storage::interface::{DeclaredStore, ObservedStore};

    use super::super::base::{finding, typed_failure, Collection};
    use super::super::registry::Registered;

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// A migrated observed store under a temporary data directory.
    fn opened() -> Built {
        let dir =
            std::env::temp_dir().join(format!("vogt-sweep-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = VogtConfig {
            data_dir: dir,
            ..VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        ctx.declared.migrate().unwrap();
        ctx.observed.migrate().unwrap();
        built
    }

    fn project(slug: &str) -> Project {
        let id = format!("prj_{slug}");
        Project::new(&id, slug, slug, "/tmp", moment())
    }

    /// A collector that always fails, to prove one bad project is contained.
    struct Boom;
    impl Collector for Boom {
        fn name(&self) -> &str {
            "boom"
        }
        fn collect(
            &mut self,
            _ctx: &mut CollectorContext<'_>,
            project: &Project,
            _now: &mut dyn FnMut() -> Moment,
        ) -> Collection {
            Collection::failed(typed_failure(
                "OSError",
                format!("cannot read {}", project.slug),
            ))
        }
    }

    /// A collector returning one finding whose content never changes.
    struct Fixed;
    impl Collector for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        fn collect(
            &mut self,
            _ctx: &mut CollectorContext<'_>,
            project: &Project,
            _now: &mut dyn FnMut() -> Moment,
        ) -> Collection {
            Collection::ok(vec![finding(
                "test.subject",
                &format!("test:{}", project.slug),
                serde_json::json!({"stable": true}),
                Some(project),
                None,
                false,
            )])
        }
    }

    /// Fails for the project named "bad" and otherwise returns one finding.
    struct Selective;
    impl Collector for Selective {
        fn name(&self) -> &str {
            "selective"
        }
        fn collect(
            &mut self,
            ctx: &mut CollectorContext<'_>,
            project: &Project,
            now: &mut dyn FnMut() -> Moment,
        ) -> Collection {
            if project.slug == "bad" {
                return Collection::failed(typed_failure("OSError", "nope"));
            }
            Fixed.collect(ctx, project, now)
        }
    }

    /// Yields one finding, then fails. The finding must still be stored.
    struct YieldThenFail;
    impl Collector for YieldThenFail {
        fn name(&self) -> &str {
            "yield-then-fail"
        }
        fn collect(
            &mut self,
            _ctx: &mut CollectorContext<'_>,
            project: &Project,
            _now: &mut dyn FnMut() -> Moment,
        ) -> Collection {
            Collection::partial(
                vec![finding(
                    "test.subject",
                    &format!("kept:{}", project.slug),
                    serde_json::json!({"kept": true}),
                    Some(project),
                    None,
                    false,
                )],
                typed_failure("OSError", "stopped after one"),
            )
        }
    }

    #[test]
    fn a_failing_collector_makes_the_sweep_failed_and_names_the_project() {
        let Built::StepSequential(ctx) = opened() else {
            unreachable!()
        };
        let mut clock = ctx.clock.lock().unwrap();
        let mut sweeper = Sweeper::new(
            &ctx.observed,
            CollectorContext {
                config: &ctx.config,
                options: serde_json::Map::new(),
            },
            &mut *clock,
        );
        let report = sweeper
            .run_one(Registered::Collector(Box::new(Boom)), &[project("one")])
            .unwrap();
        assert_eq!(report.outcome, "failed");
        assert!(report.failures.contains_key("one"));
        assert_eq!(
            report.detail.as_deref(),
            Some("one: OSError: cannot read one")
        );
    }

    #[test]
    fn one_bad_project_does_not_cost_the_others() {
        let Built::StepSequential(ctx) = opened() else {
            unreachable!()
        };
        let mut clock = ctx.clock.lock().unwrap();
        let mut sweeper = Sweeper::new(
            &ctx.observed,
            CollectorContext {
                config: &ctx.config,
                options: serde_json::Map::new(),
            },
            &mut *clock,
        );
        let report = sweeper
            .run_one(
                Registered::Collector(Box::new(Selective)),
                &[project("good"), project("bad")],
            )
            .unwrap();
        assert_eq!(report.outcome, "partial");
        assert_eq!(report.new, 1);
    }

    #[test]
    fn findings_yielded_before_a_failure_are_kept() {
        let Built::StepSequential(ctx) = opened() else {
            unreachable!()
        };
        let mut clock = ctx.clock.lock().unwrap();
        let mut sweeper = Sweeper::new(
            &ctx.observed,
            CollectorContext {
                config: &ctx.config,
                options: serde_json::Map::new(),
            },
            &mut *clock,
        );
        let alone = sweeper
            .run_one(
                Registered::Collector(Box::new(YieldThenFail)),
                &[project("one")],
            )
            .unwrap();
        // Every project failed, so the outcome stays failed — the findings
        // are what changed. A mixed sweep is partial.
        assert_eq!(alone.outcome, "failed");
        assert_eq!(alone.new, 1);
        assert_eq!(
            alone.detail.as_deref(),
            Some("one: OSError: stopped after one")
        );

        let mixed = sweeper
            .run_one(
                Registered::Collector(Box::new(YieldThenFail)),
                &[project("kept"), project("also")],
            )
            .unwrap();
        // Both projects fail, so this is still failed, not partial. Pair the
        // partial yield with a project that succeeds.
        assert_eq!(mixed.outcome, "failed");
        assert_eq!(mixed.new, 2);
    }

    #[test]
    fn a_project_that_yields_then_fails_beside_a_clean_one_is_partial() {
        let Built::StepSequential(ctx) = opened() else {
            unreachable!()
        };
        let mut clock = ctx.clock.lock().unwrap();
        let mut sweeper = Sweeper::new(
            &ctx.observed,
            CollectorContext {
                config: &ctx.config,
                options: serde_json::Map::new(),
            },
            &mut *clock,
        );
        // Selective fails with nothing; YieldThenFail keeps one. Run them as
        // two projects by a collector that yields on "kept" and fails clean
        // on "bad" — already covered. This one yields on the failing project.
        struct YieldOnBad;
        impl Collector for YieldOnBad {
            fn name(&self) -> &str {
                "yield-on-bad"
            }
            fn collect(
                &mut self,
                ctx: &mut CollectorContext<'_>,
                project: &Project,
                now: &mut dyn FnMut() -> Moment,
            ) -> Collection {
                if project.slug == "bad" {
                    return YieldThenFail.collect(ctx, project, now);
                }
                Fixed.collect(ctx, project, now)
            }
        }
        let report = sweeper
            .run_one(
                Registered::Collector(Box::new(YieldOnBad)),
                &[project("good"), project("bad")],
            )
            .unwrap();
        assert_eq!(report.outcome, "partial");
        assert_eq!(report.new, 2);
        assert!(report.failures.contains_key("bad"));
        assert!(report
            .detail
            .as_deref()
            .unwrap()
            .contains("bad: OSError: stopped after one"));
    }

    #[test]
    fn unchanged_subjects_do_not_grow_the_store() {
        let Built::StepSequential(ctx) = opened() else {
            unreachable!()
        };
        let mut clock = ctx.clock.lock().unwrap();
        let mut sweeper = Sweeper::new(
            &ctx.observed,
            CollectorContext {
                config: &ctx.config,
                options: serde_json::Map::new(),
            },
            &mut *clock,
        );
        let first = sweeper
            .run_one(
                Registered::Collector(Box::new(Fixed)),
                &[project("fixture")],
            )
            .unwrap();
        let second = sweeper
            .run_one(
                Registered::Collector(Box::new(Fixed)),
                &[project("fixture")],
            )
            .unwrap();
        assert_eq!((first.new, first.unchanged), (1, 0));
        assert_eq!((second.new, second.unchanged), (0, 1));
        assert_eq!(ctx.observed.counts().unwrap().get("observations"), Some(&1));
    }
}
