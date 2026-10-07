//! Answering Claude Code's per-directory startup questions before it asks.
//!
//! A session the engine starts is often driven by another agent, or woken
//! by policy with nobody watching, and a modal at startup reads to both as a
//! hung session (WI-912). Claude Code asks two such questions per working
//! directory, and remembers each answer in its global config
//! (`~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`) under
//! `projects["<cwd>"]`:
//!
//! - **folder trust** — "Do you trust the files in this folder?"
//!   (`hasTrustDialogAccepted`);
//! - **external `CLAUDE.md` imports** — a `CLAUDE.md` that imports a file
//!   from outside the directory (`hasClaudeMdExternalIncludesApproved`, and
//!   `hasClaudeMdExternalIncludesWarningShown` so the warning is not shown).
//!
//! The engine only ever starts sessions inside `workspace_root`, which the
//! operator trusts by deploying it, so it records both answers for the
//! session's directory immediately before the spawn. Best effort, and
//! skipped with `ENGINE_AGENT_QUIET_ONBOARDING=0` (the same switch as the
//! entrypoint's "auto mode" dismissal): a config this cannot read or parse is
//! left exactly as it was, and losing a race with a running Claude Code that
//! rewrites the file only brings back the dialog it would have shown anyway.

use std::path::{Path, PathBuf};

const KEYS: &[&str] = &[
    "hasTrustDialogAccepted",
    "hasClaudeMdExternalIncludesApproved",
    "hasClaudeMdExternalIncludesWarningShown",
];

/// Whether, and where, the engine answers the questions. Off by default —
/// which is what a test's hand-built config gets, so a test never writes the
/// real `~/.claude.json` — and read from the environment by `config::load`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Onboarding {
    pub enabled: bool,
    /// The directory Claude Code's `.claude.json` is in when a session does
    /// not set `CLAUDE_CONFIG_DIR` itself: the engine's `CLAUDE_CONFIG_DIR`,
    /// else `$HOME`.
    pub default_dir: Option<PathBuf>,
    /// The driven-session permission policy (WI-926): a Claude Code settings
    /// file, passed to every engine-launched Claude session with
    /// `--settings`. See [`settings_file_from`].
    pub settings_file: Option<PathBuf>,
    /// The opencode half of that policy (WI-932): an opencode config whose
    /// `permission` block every engine-launched opencode session gets in the
    /// default posture, as `OPENCODE_CONFIG_CONTENT`. Read and checked once,
    /// at start. See [`opencode_config_from`].
    pub opencode_config: Option<String>,
}

/// `ENGINE_AGENT_CLAUDE_SETTINGS`, read: a path names a deployment's own
/// policy; unset or empty means the image's, when it is there (Compose
/// passes an unset `.env` value as empty, so empty must not mean off); `off`
/// turns the policy off.
pub fn settings_file_from(value: Option<&str>) -> Option<PathBuf> {
    match value.map(str::trim) {
        Some("off") => None,
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => Some(PathBuf::from(IMAGE_SETTINGS_FILE)).filter(|p| p.is_file()),
    }
}

/// Where the image installs the default driven-session policy.
pub const IMAGE_SETTINGS_FILE: &str = "/usr/local/share/vogt/driven-session-settings.json";

/// Where the image installs the default opencode driven-session policy.
pub const IMAGE_OPENCODE_CONFIG: &str = "/usr/local/share/vogt/driven-session-opencode.json";

/// The permission values opencode accepts. Anything else makes opencode
/// refuse its whole configuration and fail to start, so a policy with one is
/// not handed to any session.
const OPENCODE_ACTIONS: &[&str] = &["allow", "ask", "deny"];

/// `ENGINE_AGENT_OPENCODE_CONFIG`, read like `ENGINE_AGENT_CLAUDE_SETTINGS`
/// (a path, empty for the image's, `off`), and loaded: the file's text when it
/// is a JSON object whose `permission` values opencode would accept, else
/// `None` with a warning. A broken policy must not stop every opencode session
/// from starting.
pub fn opencode_config_from(value: Option<&str>) -> Option<String> {
    let path = match value.map(str::trim) {
        Some("off") => return None,
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(IMAGE_OPENCODE_CONFIG),
    };
    let text = std::fs::read_to_string(&path).ok()?;
    match check_opencode_config(&text) {
        Ok(()) => Some(text),
        Err(why) => {
            tracing::warn!(path = %path.display(), reason = %why, "opencode driven-session policy ignored");
            None
        }
    }
}

/// Why opencode would refuse this config, if it would.
pub fn check_opencode_config(text: &str) -> std::result::Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let permission = value
        .as_object()
        .ok_or("not a JSON object")?
        .get("permission");
    let Some(permission) = permission else {
        return Ok(());
    };
    let ok = |v: &serde_json::Value| v.as_str().is_some_and(|s| OPENCODE_ACTIONS.contains(&s));
    let rules = permission
        .as_object()
        .ok_or("`permission` is not an object")?;
    for (tool, rule) in rules {
        let valid = match rule {
            serde_json::Value::Object(patterns) => patterns.values().all(ok),
            other => ok(other),
        };
        if !valid {
            return Err(format!(
                "permission.{tool}: every value must be one of allow, ask, deny"
            ));
        }
    }
    Ok(())
}

impl Onboarding {
    /// On unless `ENGINE_AGENT_QUIET_ONBOARDING=0`.
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var("ENGINE_AGENT_QUIET_ONBOARDING")
                .map_or(true, |v| v.trim() != "0"),
            default_dir: std::env::var_os("CLAUDE_CONFIG_DIR")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from),
            settings_file: settings_file_from(
                std::env::var("ENGINE_AGENT_CLAUDE_SETTINGS")
                    .ok()
                    .as_deref(),
            ),
            opencode_config: opencode_config_from(
                std::env::var("ENGINE_AGENT_OPENCODE_CONFIG")
                    .ok()
                    .as_deref(),
            ),
        }
    }

    /// Claude Code's global config file for a session with `env`.
    pub fn config_path(&self, env: &[(String, String)]) -> Option<PathBuf> {
        env.iter()
            .rev()
            .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
            .map(|(_, v)| PathBuf::from(v))
            .or_else(|| self.default_dir.clone())
            .map(|dir| dir.join(".claude.json"))
    }

    /// [`accept_project`] for a Claude session about to start in `cwd`,
    /// logging rather than failing: the session starts either way.
    pub fn prepare(&self, env: &[(String, String)], cwd: &str) {
        if !self.enabled {
            return;
        }
        let Some(path) = self.config_path(env) else {
            return;
        };
        if let Err(e) = accept_project(&path, cwd) {
            tracing::info!(path = %path.display(), error = %e, "could not pre-accept Claude Code's folder trust; it may ask");
        }
    }
}

/// Record, in the config at `path`, that `cwd` is trusted and its external
/// `CLAUDE.md` imports approved. `Ok(false)` when nothing needed writing.
pub fn accept_project(path: &Path, cwd: &str) -> std::io::Result<bool> {
    let mut config: serde_json::Value = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            // Not ours to repair.
            Err(_) => return Ok(false),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    let Some(root) = config.as_object_mut() else {
        return Ok(false);
    };
    let projects = root
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    let Some(projects) = projects.as_object_mut() else {
        return Ok(false);
    };
    let project = projects
        .entry(cwd.to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(project) = project.as_object_mut() else {
        return Ok(false);
    };
    if KEYS
        .iter()
        .all(|k| project.get(*k) == Some(&serde_json::Value::Bool(true)))
    {
        return Ok(false);
    }
    for key in KEYS {
        project.insert((*key).to_string(), serde_json::Value::Bool(true));
    }
    let body = serde_json::to_vec_pretty(&config).map_err(std::io::Error::other)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".claude.json.vogt-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::write(&tmp, &body)?;
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_recorded_once_and_everything_else_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            r#"{"autoModeEnvSetup":{"dismissed":true},"projects":{"/w/other":{"x":1},"/w/repo":{"allowedTools":["Bash"]}}}"#,
        )
        .unwrap();
        assert!(accept_project(&path, "/w/repo").unwrap());
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let repo = &config["projects"]["/w/repo"];
        for key in KEYS {
            assert_eq!(repo[*key], true, "{key}");
        }
        assert_eq!(repo["allowedTools"][0], "Bash");
        assert_eq!(config["projects"]["/w/other"]["x"], 1);
        assert_eq!(config["autoModeEnvSetup"]["dismissed"], true);
        assert!(
            !accept_project(&path, "/w/repo").unwrap(),
            "nothing left to write"
        );
    }

    #[test]
    fn a_missing_config_is_created_and_a_broken_one_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(".claude.json");
        assert!(accept_project(&path, "/w/repo").unwrap());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("hasTrustDialogAccepted"));
        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, b"{nope").unwrap();
        assert!(!accept_project(&broken, "/w/repo").unwrap());
        assert_eq!(std::fs::read(&broken).unwrap(), b"{nope");
    }

    #[test]
    fn the_policy_setting_reads_a_path_off_or_the_image_default() {
        assert_eq!(
            settings_file_from(Some("/srv/policy.json")),
            Some(PathBuf::from("/srv/policy.json"))
        );
        assert_eq!(settings_file_from(Some("off")), None);
        // Unset and empty mean the same: the image's file, when it exists.
        assert_eq!(settings_file_from(Some("")), settings_file_from(None));
    }

    #[test]
    fn the_session_config_dir_wins() {
        let onboarding = Onboarding {
            enabled: true,
            default_dir: Some(PathBuf::from("/home/pod")),
            settings_file: None,
            opencode_config: None,
        };
        let env = vec![("CLAUDE_CONFIG_DIR".to_string(), "/cfg".to_string())];
        assert_eq!(
            onboarding.config_path(&env),
            Some(PathBuf::from("/cfg/.claude.json"))
        );
        assert_eq!(
            onboarding.config_path(&[]),
            Some(PathBuf::from("/home/pod/.claude.json"))
        );
    }

    #[test]
    fn the_shipped_opencode_policy_is_one_opencode_accepts() {
        let shipped = include_str!("../../deploy/driven-session-opencode.json");
        assert_eq!(check_opencode_config(shipped), Ok(()));
        assert_eq!(
            check_opencode_config(crate::agent_cli::OPENCODE_BYPASS),
            Ok(())
        );
        assert_eq!(
            check_opencode_config(crate::agent_cli::OPENCODE_ACCEPT_EDITS),
            Ok(())
        );
    }

    #[test]
    fn an_opencode_policy_opencode_would_refuse_is_never_handed_out() {
        assert!(check_opencode_config(r#"{"permission":{"bash":"maybe"}}"#).is_err());
        assert!(check_opencode_config(r#"{"permission":{"bash":{"*":"yes"}}}"#).is_err());
        assert!(check_opencode_config("not json").is_err());
        assert!(check_opencode_config("[]").is_err());
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("bad.json");
        std::fs::write(&bad, r#"{"permission":{"edit":"sometimes"}}"#).unwrap();
        assert_eq!(opencode_config_from(bad.to_str()), None);
        let good = tmp.path().join("good.json");
        std::fs::write(&good, r#"{"permission":{"edit":"allow"}}"#).unwrap();
        assert!(opencode_config_from(good.to_str()).is_some());
        assert_eq!(opencode_config_from(Some("off")), None);
    }
}

/// The shipped driven-session policy (`engine/deploy/driven-session-settings.json`)
/// is a security boundary, so its shape is pinned here (WI-926).
///
/// Two layers decide a driven Claude session's actions. Pattern rules
/// (`permissions.deny`, then `ask`, then `allow`) resolve first and never
/// reach the classifier; everything else goes to auto mode's classifier,
/// judged against its built-in rules plus this file's `autoMode` entries.
/// These tests prove what the file itself decides: it keeps every built-in
/// rule, it adds no pattern rule, so every command below still reaches the
/// classifier (a read is never pattern-denied or pattern-asked, and a merge,
/// force-push, `rm -rf`, Komodo write, Infisical read or `docker compose
/// down` is never pattern-allowed or downgraded to a prompt), and its allow
/// exceptions say what they clear and what they never cover.
///
/// What they cannot prove is the classifier's verdict: whether it actually
/// lets `docker ps` through under *Read-Only Inspection*, or still denies a
/// `.Config.Env` dump, is judged by a model at run time. That needs a check
/// on vogt-dev after merge.
#[cfg(test)]
mod driven_policy_tests {
    use serde_json::Value;

    const SHIPPED: &str = include_str!("../../deploy/driven-session-settings.json");

    fn policy() -> Value {
        serde_json::from_str(SHIPPED).expect("the shipped policy is JSON")
    }

    fn allow_rules() -> Vec<String> {
        policy()["autoMode"]["allow"]
            .as_array()
            .expect("autoMode.allow is a list")
            .iter()
            .map(|r| r.as_str().expect("every rule is prose").to_string())
            .collect()
    }

    fn rule(name: &str) -> String {
        let prefix = format!("{name}:");
        allow_rules()
            .into_iter()
            .find(|r| r.starts_with(&prefix))
            .unwrap_or_else(|| panic!("the shipped policy has no `{name}` rule"))
    }

    /// Claude Code's documented Bash rule matching, enough to ask whether
    /// any rule in the file would catch a command: `*` matches any text,
    /// a trailing ` *` (or `:*`) also matches the bare command, and a
    /// compound command is matched part by part.
    fn bash_rule_matches(rule: &str, command: &str) -> bool {
        let Some(pattern) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
            // A bare tool name (`Bash`) matches every call of that tool.
            return rule == "Bash";
        };
        let pattern = pattern
            .strip_suffix(":*")
            .map_or_else(|| pattern.to_string(), |p| format!("{p} *"));
        let glob = format!(
            "^{}$",
            pattern
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*")
        );
        let re = regex::Regex::new(&glob).unwrap();
        let bare = pattern
            .strip_suffix(" *")
            .filter(|p| !p.contains('*'))
            .map(str::to_string);
        regex::Regex::new(r"&&|\|\||;|\||\n")
            .unwrap()
            .split(command)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .any(|part| re.is_match(part) || bare.as_deref() == Some(part))
    }

    /// The pattern verdict for a command, or `None` when no pattern rule
    /// matches and the classifier decides.
    fn pattern_verdict(command: &str) -> Option<&'static str> {
        let policy = policy();
        for list in ["deny", "ask", "allow"] {
            let rules = policy["permissions"][list]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if rules
                .iter()
                .filter_map(Value::as_str)
                .any(|r| bash_rule_matches(r, command))
            {
                return Some(list);
            }
        }
        None
    }

    #[test]
    fn the_matcher_matches_the_way_claude_code_documents() {
        // Guard for the helper the other tests rely on.
        assert!(bash_rule_matches(
            "Bash(gh pr merge *)",
            "gh pr merge 12 --squash"
        ));
        assert!(bash_rule_matches("Bash(gh pr merge *)", "gh pr merge"));
        assert!(bash_rule_matches(
            "Bash(gh pr merge:*)",
            "cd x && gh pr merge 3"
        ));
        assert!(bash_rule_matches(
            "Bash(gh pr merge *--admin*)",
            "gh pr merge 3 --admin --rebase"
        ));
        assert!(bash_rule_matches("Bash", "ls"));
        assert!(!bash_rule_matches("Bash(ls *)", "lsof"));
        assert!(!bash_rule_matches("Bash(gh pr merge *)", "gh pr view 3"));
    }

    #[test]
    fn every_built_in_rule_is_kept() {
        // A list without "$defaults" replaces Claude Code's whole list for
        // that section: soft_deny would lose force-push, prod deploys and
        // credential rules; hard_deny would lose Data Exfiltration.
        let auto = &policy()["autoMode"];
        for list in ["environment", "allow", "soft_deny", "hard_deny"] {
            if let Some(rules) = auto.get(list) {
                let rules = rules.as_array().expect("a rule list");
                assert!(
                    rules.iter().any(|r| r == "$defaults"),
                    "autoMode.{list} must keep \"$defaults\""
                );
            }
        }
        assert!(auto.get("environment").is_some() && auto.get("allow").is_some());
        // The policy only adds exceptions: it never relaxes a built-in block
        // rule by redefining it, nor turns the classifier off for the shell.
        assert!(auto.get("soft_deny").is_none(), "no soft_deny override");
        assert!(auto.get("hard_deny").is_none(), "no hard_deny override");
        assert!(auto.get("classifyAllShell").is_none());
    }

    #[test]
    fn there_is_no_pattern_rule_so_the_classifier_judges_every_command() {
        // A pattern allow cannot tell `docker inspect --format '{{.State}}'`
        // from a `.Config.Env` dump, or a curl GET from one with `-d @file`,
        // and resolves before the classifier sees the command.
        let policy = policy();
        for list in ["allow", "ask", "deny"] {
            let rules = policy["permissions"][list]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                rules.is_empty(),
                "permissions.{list} must stay empty in the shipped policy: {rules:?}"
            );
        }
        // Nor may the file pick a posture: bypass is a person's grant, made
        // per spawn (`permission_mode`), never by the policy itself.
        assert!(policy["permissions"].get("defaultMode").is_none());
        assert!(policy.get("skipDangerousModePermissionPrompt").is_none());
    }

    #[test]
    fn read_only_commands_are_neither_denied_nor_asked_by_a_pattern() {
        for command in [
            "docker ps",
            "docker ps -a --format '{{.Names}} {{.Status}}'",
            "docker logs --tail 200 vogt-dev-engine-1",
            "docker stats --no-stream",
            "docker inspect --format '{{.State.Health.Status}}' vogt-dev-engine-1",
            "ss -ltnp",
            "netstat -tlnp",
            "curl -fsS http://127.0.0.1:8910/healthz",
            "wget -qO- http://localhost:8080/health",
            "git status --short",
            "git log --oneline -5",
            "git diff origin/main",
            "git show HEAD",
            "git branch -a",
            "gh pr view 873",
            "gh pr list --state open",
            "gh pr checks 873",
            "gh pr diff 873",
            "gh run view 12345 --log-failed",
            "ls -la",
            "cat README.md",
            "grep -rn autoMode engine/",
            "find . -name '*.rs'",
            "head -20 Cargo.toml",
            "tail -f /dev/null",
            "test -n \"${GH_TOKEN:+x}\" && echo set",
            "echo \"$AGENT_AUTH_GRANTED\"",
        ] {
            assert_eq!(pattern_verdict(command), None, "{command}");
        }
    }

    #[test]
    fn the_read_only_rule_clears_only_inspection_and_keeps_secrets_blocked() {
        let rule = rule("Read-Only Inspection");
        for command in [
            "`docker ps`",
            "`docker logs`",
            "`docker stats`",
            "`docker inspect --format`",
            "`ss`",
            "`netstat`",
            "`git` commands",
            "`gh pr view|list|checks|diff`",
            "`gh run view`",
            "`curl`/`wget`",
        ] {
            assert!(
                rule.contains(command),
                "Read-Only Inspection names {command}"
            );
        }
        // What it clears, and only that.
        assert!(rule.contains(
            "This clears Modify Shared Resources and Interfere With Workloads for those reads only"
        ));
        // What it never covers.
        for kept in [
            "no request body and no credentials",
            "printing a secret",
            "dumping environment variables",
            "`.Config.Env`",
            "`docker exec`",
            "creating/starting/stopping/removing anything",
            "writing to a shared service",
            "sending data off the host",
            "stay blocked",
        ] {
            assert!(
                rule.contains(kept),
                "Read-Only Inspection must keep: {kept}"
            );
        }
    }

    #[test]
    fn the_presence_rule_never_reads_or_fetches_a_credential() {
        let rule = rule("Credential Presence Check");
        for name in [
            "AGENT_AUTH_MODE",
            "AGENT_AUTH_GRANTED",
            "AGENT_AUTH_ONDEMAND",
            "AGENT_AUTH_WRITABLE",
        ] {
            assert!(rule.contains(name), "{name}");
        }
        assert!(rule.contains("presence checks only"));
        for kept in [
            "printing a credential value or any portion of one",
            "copying it into a file or another variable",
            "obtaining a credential the session does not already hold",
            "Vogt grant flow",
        ] {
            assert!(
                rule.contains(kept),
                "Credential Presence Check must keep: {kept}"
            );
        }
    }

    #[test]
    fn merge_is_not_converted_to_a_prompt_and_stays_with_the_classifier() {
        // Deferred (WI-983): any agent with work.write can answer a session's
        // permission prompt, so an ask rule would let one agent approve
        // another's merge rather than put a person in the loop.
        for command in [
            "gh pr merge 873 --rebase",
            "gh pr merge 873 --squash --delete-branch",
            "gh pr merge 873 --admin --rebase",
            "gh pr merge --auto 873",
            "cd /w/repo && gh pr merge 1",
        ] {
            assert_eq!(pattern_verdict(command), None, "{command}");
        }
        // The only merge exception is still Own Green PR Merge, last in the
        // list, for listed repositories, and never `--admin`.
        let rules = allow_rules();
        let merge: Vec<_> = rules.iter().filter(|r| r.contains("gh pr merge")).collect();
        assert_eq!(merge.len(), 1, "only Own Green PR Merge mentions a merge");
        assert!(merge[0].starts_with("Own Green PR Merge:"));
        assert!(rules.last().unwrap().starts_with("Own Green PR Merge:"));
        assert!(merge[0].contains("It never covers `--admin`"));
        assert!(merge[0].contains("Autonomous-merge repositories"));
        let env = policy()["autoMode"]["environment"].to_string();
        assert!(
            env.contains("**Autonomous-merge repositories**: none configured"),
            "the image ships no autonomous-merge repository"
        );
    }

    #[test]
    fn prod_destructive_and_secret_actions_still_go_to_the_classifier() {
        for command in [
            "git push --force origin main",
            "git push -f origin main",
            "git push origin +main",
            "git push origin --delete main",
            "git push origin feature/x",
            "rm -rf /",
            "rm -rf ~",
            "rm -rf /var/lib/docker/volumes",
            "docker compose down",
            "docker compose -p vogt-prod down -v",
            "docker rm -f vogt-prod-vogt-1",
            "docker exec vogt-prod-vogt-1 env",
            "docker inspect vogt-prod-vogt-1",
            "docker inspect --format '{{.Config.Env}}' vogt-prod-vogt-1",
            "infisical secrets get HOMELAB_VOGT_PROD_AGENT_TOKEN",
            "infisical export --env=prod",
            "infisical run -- env",
            "vogt-agent-auth fetch GH_TOKEN",
            "curl -X POST http://100.92.54.45:3011/execute/DeployStack -d '{}'",
            "curl -X POST http://100.92.54.45:3011/write/UpdateStack -d @stack.json",
            "printenv",
            "cat ~/.config/gh/hosts.yml",
            "sqlite3 vogt.db 'DROP TABLE work_items'",
        ] {
            assert_eq!(pattern_verdict(command), None, "{command}");
        }
        // And no exception this file adds names one of them as covered:
        // the new rules mention these only in what they never cover.
        let added: Vec<_> = allow_rules()
            .into_iter()
            .filter(|r| r != "$defaults" && !r.starts_with("Own Green PR Merge:"))
            .collect();
        assert_eq!(
            added.len(),
            2,
            "exactly the two read-only exceptions are added"
        );
        for rule in &added {
            for dangerous in [
                "git push",
                "--force",
                "rm -rf",
                "compose down",
                "docker rm",
                "infisical",
                "Komodo",
                "DeployStack",
                "UpdateStack",
                "vogt-agent-auth fetch",
                "DROP",
                "Production Deploy",
                "Credential Materialization",
                "Data Exfiltration",
            ] {
                assert!(
                    !rule.contains(dangerous),
                    "an added exception must not name {dangerous:?}: {rule}"
                );
            }
        }
    }
}
