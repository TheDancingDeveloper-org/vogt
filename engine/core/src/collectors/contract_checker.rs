//! `contract-checker` — evaluated when asked, and only when asked.
//!
//! Ports `src/vogt/collectors/contract_checker.py`. One of the core collectors,
//! with one difference: it runs on demand only. Nothing re-checks compliance on
//! a timer. The contract is evaluated when somebody asks, and the answer is
//! reported with its age like every other value.

use std::path::Path;

use serde_json::{json, Value};

use crate::config::VogtConfig;
use crate::core::Project;
use crate::decisions::{contract_from_settings, evaluate, Contract, ContractTree};

use super::base::{finding, Collection, Collector, CollectorContext, Finding};
use super::git_local::tracked_names;

const KIND_CONTRACT: &str = "contract.check";

/// Evaluates the project contract against a registered project.
pub struct ContractCheckerCollector;

impl ContractCheckerCollector {
    fn contract(config: &VogtConfig) -> Contract {
        let files: Vec<&str> = config
            .contract_required_files
            .iter()
            .map(String::as_str)
            .collect();
        let dirs: Vec<&str> = config
            .contract_required_dirs
            .iter()
            .map(String::as_str)
            .collect();
        let meta: Vec<&str> = config
            .contract_required_meta
            .iter()
            .map(String::as_str)
            .collect();
        contract_from_settings(&config.contract_version, &files, &dirs, &meta)
    }
}

/// A filesystem the evaluator can ask about. Paths arrive already normalised
/// by `evaluate`.
struct Filesystem;

impl ContractTree for Filesystem {
    fn is_dir(&self, path: &str) -> bool {
        Path::new(path).is_dir()
    }

    fn is_file(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }
}

impl Collector for ContractCheckerCollector {
    fn name(&self) -> &str {
        "contract-checker"
    }

    fn requires_network(&self) -> bool {
        false
    }

    fn on_demand_only(&self) -> bool {
        true
    }

    fn collect(
        &mut self,
        ctx: &mut CollectorContext<'_>,
        project: &Project,
        _now: &mut dyn FnMut() -> crate::core::Moment,
    ) -> Collection {
        let contract = Self::contract(ctx.config);
        let tracked = tracked_names(Path::new(&project.root_path));
        let result = evaluate(
            &project.root_path,
            &contract,
            &Filesystem,
            tracked.as_ref(),
            &[],
        );
        let failing: Vec<Value> = result
            .failing()
            .into_iter()
            .map(|criterion| {
                json!({
                    "rule": criterion.rule,
                    "target": criterion.target,
                    "detail": criterion.detail,
                })
            })
            .collect();
        let evaluated: Vec<Value> = result
            .criteria
            .iter()
            .map(|criterion| {
                json!({
                    "rule": criterion.rule,
                    "target": criterion.target,
                    "satisfied": criterion.satisfied,
                    "tracked": criterion.tracked,
                })
            })
            .collect();
        Collection::ok(vec![finding(
            KIND_CONTRACT,
            &format!("contract:{}", project.slug),
            json!({
                "contract_version": result.contract_version,
                "status": result.status,
                "failing": failing,
                "evaluated": evaluated,
            }),
            Some(project),
            None,
            false,
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Moment;
    use serde_json::Map;
    use std::path::PathBuf;
    use std::process::Command;

    fn project(root: &Path) -> Project {
        Project::new(
            "prj_fixture",
            "fixture",
            "fixture",
            &root.display().to_string(),
            Moment::from_unix(0, 0),
        )
    }

    fn collect(root: &Path) -> Finding {
        let config = VogtConfig::default();
        let mut context = CollectorContext {
            config: &config,
            options: Map::new(),
        };
        let mut now = || Moment::from_unix(0, 0);
        let found = ContractCheckerCollector
            .collect(&mut context, &project(root), &mut now)
            .findings;
        assert_eq!(found.len(), 1);
        found.into_iter().next().unwrap()
    }

    fn temp(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("vogt-contract-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn plant_stock(root: &Path) {
        for name in [
            "README.md",
            "AGENTS.md",
            "LICENSE",
            "name",
            "lifecycle_state",
            "owner",
        ] {
            std::fs::write(root.join(name), "x\n").unwrap();
        }
        for name in ["docs", "design", "src"] {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::write(root.join(name).join(".keep"), "\n").unwrap();
        }
    }

    #[test]
    fn it_is_on_demand_and_offline() {
        let collector = ContractCheckerCollector;
        assert_eq!(collector.name(), "contract-checker");
        assert!(collector.on_demand_only());
        assert!(!collector.requires_network());
    }

    #[test]
    fn a_plain_folder_is_filesystem_only() {
        let root = temp("plain");
        plant_stock(&root);
        let found = collect(&root);
        assert_eq!(found.kind, "contract.check");
        assert_eq!(found.subject_key, "contract:fixture");
        assert_eq!(found.payload["status"], "compliant");
        let evaluated = found.payload["evaluated"].as_array().unwrap();
        assert!(evaluated.iter().all(|row| row["tracked"].is_null()));
        assert!(found.payload["failing"].as_array().unwrap().is_empty());
        let text = serde_json::to_string(&found.payload).unwrap();
        let version = text.find("contract_version").unwrap();
        let status = text.find("\"status\"").unwrap();
        let failing = text.find("\"failing\"").unwrap();
        let evaluated_at = text.find("\"evaluated\"").unwrap();
        assert!(version < status && status < failing && failing < evaluated_at);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_untracked_required_file_fails() {
        let root = temp("repo");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        plant_stock(&root);
        git(
            &root,
            &[
                "add",
                "README.md",
                "AGENTS.md",
                "name",
                "lifecycle_state",
                "owner",
                "docs",
                "design",
                "src",
            ],
        );
        git(&root, &["commit", "-qm", "first"]);
        let found = collect(&root);
        assert_eq!(found.payload["status"], "non_compliant");
        let failing = found.payload["failing"].as_array().unwrap();
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0]["rule"], "required_file");
        assert_eq!(failing[0]["target"], "LICENSE");
        assert_eq!(
            failing[0]["detail"],
            "LICENSE is present in the working tree but not tracked, so no clone of this repository has it"
        );
        let license = found.payload["evaluated"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["target"] == "LICENSE")
            .unwrap();
        assert_eq!(license["tracked"], false);
        assert_eq!(license["satisfied"], false);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_directory_is_not_checked() {
        let root = temp("missing");
        let gone = root.join("nope");
        let found = collect(&gone);
        assert_eq!(found.payload["status"], "not_checked");
        let evaluated = found.payload["evaluated"].as_array().unwrap();
        assert_eq!(evaluated.len(), 1);
        assert_eq!(evaluated[0]["rule"], "path.exists");
        assert_eq!(evaluated[0]["satisfied"], false);
        assert_eq!(
            found.payload["failing"][0]["detail"],
            "the path does not exist or is not a directory, so no criterion below was evaluated — this is 'not checked', not 'does not comply'"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
