//! When an oversight session may answer a worker's permission prompt (WI-1831).
//!
//! `person_gate` keeps every permission prompt a person's to answer. That
//! stalled driven workers on routine commands — `git fetch && git log`, a
//! checkout of a remote ref, a test run — until someone tapped the prompt on
//! a phone. This is the one scoped exception: an oversight session may choose
//! the plain **Yes** on a prompt whose command is in an allow-listed class and
//! that runs in the worker's own git worktree. Everything else still needs a
//! person.
//!
//! The decision is made here, in the engine, from facts the engine reads
//! itself: the dialog on screen, the overseer's role, the target's working
//! directory and its checked-out branch. The core only says which session it
//! is relaying (`AnswerRequest::overseer`), and that is honoured only from the
//! stack secret, like `person`.
//!
//! The command classes match the allow list in the shipped driven-session
//! policy (`engine/deploy/driven-session-settings.json`): read-only git and
//! forge inspection, fetching, moving the worktree to a ref, rebasing the
//! session's own branch, pushing that branch (`--force-with-lease` at most),
//! and the project's build and test commands. The parse is deliberately
//! narrow: one line, parts joined only by `&&`, no substitution, redirection,
//! pipes, `cd`, `-C`, absolute or `..` paths. A command it cannot read is a
//! person's.

use std::path::Path;

use crate::approval::Detected;

/// Why an oversight answer was refused, for the refusal text and the audit.
pub type Refusal = String;

/// Decide whether an oversight session may choose `label` on `dialog`,
/// shown by a session working in `cwd`. `branch` is the branch checked out
/// there (`None` when detached or unreadable). `Ok` carries the command that
/// was approved, for the audit line.
pub fn may_answer(
    dialog: &Detected,
    label: &str,
    cwd: &Path,
    branch: Option<&str>,
) -> Result<String, Refusal> {
    if dialog.kind != "permission" {
        return Err(format!("a {} prompt is a person's to answer", dialog.kind));
    }
    if !label.trim().eq_ignore_ascii_case("yes") {
        // "Yes, and don't ask again" widens the session's rules for good;
        // "No" is the agent's own to give by stopping.
        return Err(format!(
            "an overseer may choose only \"Yes\", not {label:?}"
        ));
    }
    if !is_linked_worktree(cwd) {
        return Err(format!(
            "{} is not a linked git worktree, so the command is not confined to the session's own tree",
            cwd.display()
        ));
    }
    let command = bash_command(&dialog.command_excerpt)
        .ok_or_else(|| "the prompt is not a single-line Bash command".to_string())?;
    allowed(&command, branch, &|arg| cwd.join(arg).exists())?;
    Ok(command)
}

/// A linked worktree (`git worktree add`) has a `.git` *file* pointing at the
/// main repository; the shared checkout has a `.git` directory.
fn is_linked_worktree(cwd: &Path) -> bool {
    cwd.join(".git").is_file()
}

/// The command a Claude Code Bash prompt shows: the lines after `Bash
/// command`, which are the command and then a one-line description. Only a
/// one-line command is read; anything else (a multi-line script, an excerpt
/// cut short) is left to a person, so no line is ever approved unseen.
pub fn bash_command(excerpt: &str) -> Option<String> {
    let mut lines = excerpt.lines().map(str::trim);
    if lines.next()? != "Bash command" {
        return None;
    }
    let body: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
    let command = match body.as_slice() {
        [command] | [command, _] => *command,
        _ => return None,
    };
    if excerpt.ends_with('…') || command.ends_with('\\') {
        return None;
    }
    Some(command.to_string())
}

/// Whether every `&&`-joined part of `command` is in an allowed class.
/// `is_path` says whether an argument names a file in the worktree, so a
/// checkout of a path (which discards local changes) is told from one of a
/// ref.
pub fn allowed(
    command: &str,
    branch: Option<&str>,
    is_path: &dyn Fn(&str) -> bool,
) -> Result<(), Refusal> {
    const FORBIDDEN: [&str; 9] = ["$", "`", ">", "<", ";", "|", "\n", "&&&", "~"];
    if let Some(bad) = FORBIDDEN.iter().find(|f| command.contains(*f)) {
        return Err(format!("the command contains {bad:?}"));
    }
    if command.matches('&').count() % 2 == 1 {
        return Err("the command backgrounds a process".into());
    }
    for part in command.split("&&") {
        let words: Vec<&str> = part
            .split_whitespace()
            .map(|w| w.trim_matches(|c| c == '\'' || c == '"'))
            .collect();
        if words.is_empty() {
            return Err("the command has an empty part".into());
        }
        if let Some(path) = words
            .iter()
            .find(|w| w.starts_with('/') || w.contains("..") || w.contains("=/"))
        {
            return Err(format!("{path:?} reaches outside the worktree"));
        }
        part_allowed(&words, branch, is_path).map_err(|why| format!("`{}`: {why}", part.trim()))?;
    }
    Ok(())
}

fn part_allowed(
    words: &[&str],
    branch: Option<&str>,
    is_path: &dyn Fn(&str) -> bool,
) -> Result<(), Refusal> {
    let has = |flags: &[&str]| {
        words.iter().any(|w| {
            flags
                .iter()
                .any(|f| w == f || w.starts_with(&format!("{f}=")))
        })
    };
    match words {
        ["git", rest @ ..] => {
            // Global options (`-C`, `-c`) come before the subcommand, so the
            // match below already refuses them.
            if has(&[
                "--output",
                "--ext-diff",
                "--textconv",
                "--git-dir",
                "--work-tree",
                "--exec-path",
            ]) {
                return Err("git is pointed elsewhere or reconfigured".into());
            }
            match rest {
                ["status" | "log" | "diff" | "show" | "rev-parse" | "merge-base", ..] => Ok(()),
                ["branch", args @ ..] => {
                    if args.iter().all(|a| {
                        matches!(*a, "--show-current" | "-a" | "-r" | "-v" | "-vv" | "--list")
                    }) {
                        Ok(())
                    } else {
                        Err("only listing branches is allowed".into())
                    }
                }
                ["fetch", ..] => {
                    if has(&["--upload-pack", "--refmap"]) {
                        Err("fetch runs a custom upload-pack".into())
                    } else {
                        Ok(())
                    }
                }
                ["checkout" | "switch", args @ ..] => {
                    // Moving the worktree to a ref, never restoring paths
                    // over local changes.
                    let refs: Vec<&&str> = args.iter().filter(|a| !a.starts_with('-')).collect();
                    if refs
                        .iter()
                        .any(|r| **r == "." || r.contains('*') || is_path(r))
                    {
                        return Err("checking out a path discards local changes".into());
                    }
                    let refs = refs.len();
                    let flags_ok = args
                        .iter()
                        .filter(|a| a.starts_with('-'))
                        .all(|a| matches!(*a, "-b" | "-c" | "--detach" | "--track" | "-t" | "-q"));
                    if flags_ok && (1..=2).contains(&refs) {
                        Ok(())
                    } else {
                        Err("only a checkout of a ref or a new branch is allowed".into())
                    }
                }
                ["rebase", args @ ..] => {
                    let flags_ok = args.iter().filter(|a| a.starts_with('-')).all(|a| {
                        matches!(
                            *a,
                            "--continue" | "--abort" | "--skip" | "--onto" | "--autostash" | "-q"
                        )
                    });
                    match branch {
                        Some(b) if is_protected(b) => Err(format!(
                            "the worktree is on {b}, not the session's own branch"
                        )),
                        None => Err("the worktree is not on a branch".into()),
                        _ if flags_ok => Ok(()),
                        _ => Err("interactive, exec and other rebase options need a person".into()),
                    }
                }
                ["push", args @ ..] => push_allowed(args, branch),
                _ => Err("this git subcommand is not in the allowed classes".into()),
            }
        }
        ["gh", "pr", "view" | "checks" | "diff" | "list", ..]
        | ["gh", "run", "view" | "list", ..] => Ok(()),
        ["cargo", sub, ..] => {
            if has(&["--manifest-path", "--target-dir", "--config", "-Z"]) {
                return Err("cargo is pointed elsewhere or reconfigured".into());
            }
            match *sub {
                "build" | "check" | "test" | "clippy" | "fmt" | "nextest" => Ok(()),
                _ => Err("only cargo build, check, test, clippy and fmt are allowed".into()),
            }
        }
        ["pnpm", rest @ ..] => {
            if has(&["--dir", "-C", "--prefix", "--global", "-g"]) {
                return Err("pnpm is pointed elsewhere".into());
            }
            let rest = match rest {
                ["run", rest @ ..] => rest,
                rest => rest,
            };
            match rest {
                ["build" | "test" | "typecheck" | "lint" | "vitest", ..] => Ok(()),
                _ => Err("only pnpm build, test, typecheck and lint are allowed".into()),
            }
        }
        ["uv", "run", "pytest" | "ruff", ..] => Ok(()),
        _ => Err("not in the allowed classes".into()),
    }
}

/// `git push` of the session's own branch: no refspec (its upstream), or
/// `origin HEAD`, `origin <branch>` or `origin HEAD:<branch>`, optionally
/// `-u` and `--force-with-lease`. Never `--force`, never another branch,
/// never a protected one.
fn push_allowed(args: &[&str], branch: Option<&str>) -> Result<(), Refusal> {
    let Some(branch) = branch else {
        return Err("the worktree is not on a branch".into());
    };
    if is_protected(branch) {
        return Err(format!(
            "the worktree is on {branch}, not the session's own branch"
        ));
    }
    let (flags, rest): (Vec<&str>, Vec<&str>) = args.iter().partition(|a| a.starts_with('-'));
    if let Some(flag) = flags
        .iter()
        .find(|f| !matches!(**f, "-u" | "--set-upstream" | "--force-with-lease" | "-q"))
    {
        return Err(format!("push option {flag} needs a person"));
    }
    let own_head = format!("HEAD:{branch}");
    let target_ok = match rest.as_slice() {
        [] => true,
        ["origin"] => true,
        ["origin", target] => *target == "HEAD" || *target == branch || *target == own_head,
        _ => false,
    };
    if target_ok {
        Ok(())
    } else {
        Err(format!(
            "only the session's own branch ({branch}) may be pushed"
        ))
    }
}

fn is_protected(branch: &str) -> bool {
    matches!(branch, "main" | "master" | "dev" | "develop" | "release")
        || branch.starts_with("release/")
}

/// The branch checked out in `cwd`, read with git itself; `None` when HEAD
/// is detached or git cannot say.
pub fn current_branch(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: Option<&str> = Some("wi-1831-fix");

    fn no_paths(_: &str) -> bool {
        false
    }

    fn check(command: &str, branch: Option<&str>) -> Result<(), Refusal> {
        allowed(command, branch, &|arg| arg == "src/lib.rs")
    }

    fn ok(command: &str) -> bool {
        check(command, OWN).is_ok()
    }

    #[test]
    fn the_routine_worker_loop_is_allowed() {
        for command in [
            "git fetch origin && git log --oneline -5 origin/main",
            "git checkout origin/rust-core",
            "git switch -c wi-1831-fix origin/main",
            "git status && git diff --stat",
            "git rebase origin/main",
            "git rebase --continue",
            "git push --force-with-lease origin HEAD",
            "git push -u origin wi-1831-fix",
            "git push origin HEAD:wi-1831-fix",
            "git push",
            "cargo test -p vogt-engine-server",
            "cargo clippy --all-targets -- -D warnings",
            "pnpm typecheck && pnpm run test",
            "uv run pytest tests/test_x.py --no-cov",
            "gh pr view 12 && gh run view 345",
            "git log --format='%h %s' -3",
        ] {
            assert!(ok(command), "{command}: {:?}", check(command, OWN));
        }
    }

    #[test]
    fn anything_else_needs_a_person() {
        for command in [
            "git push --force origin HEAD",
            "git push -f",
            "git push origin main",
            "git push origin HEAD:main",
            "git push origin other-branch",
            "git rebase -i origin/main",
            "git rebase --exec 'make' origin/main",
            "git checkout -- src/lib.rs",
            "git checkout src/lib.rs",
            "git checkout .",
            "git -C /home/x/other log",
            "git -c core.sshCommand=evil fetch",
            "git reset --hard origin/main",
            "git clean -fdx",
            "git branch -D wi-1831-fix",
            "git fetch --upload-pack=evil origin",
            "cd /tmp && git status",
            "git status; rm -rf x",
            "git log | sh",
            "git log > out.txt",
            "git log $(whoami)",
            "git log `whoami`",
            "cargo test & sleep 1",
            "cargo install ripgrep",
            "cargo test --manifest-path ../other/Cargo.toml",
            "pnpm --dir ../x test",
            "pnpm exec rm -rf .",
            "gh pr merge 12",
            "gh pr close 12",
            "docker builder prune",
            "rm -rf target",
            "cat ~/.ssh/id_ed25519",
            "git diff /etc/passwd",
            "git log --output=src/lib.rs",
            "git diff --ext-diff",
        ] {
            assert!(!ok(command), "{command} must need a person");
        }
    }

    #[test]
    fn rebase_and_push_need_the_sessions_own_branch() {
        assert!(allowed("git rebase origin/main", Some("main"), &no_paths).is_err());
        assert!(allowed("git push", Some("main"), &no_paths).is_err());
        assert!(allowed("git push", None, &no_paths).is_err());
        assert!(allowed("git rebase origin/main", None, &no_paths).is_err());
        // Reading is fine wherever the worktree is.
        assert!(allowed("git fetch origin", None, &no_paths).is_ok());
    }

    #[test]
    fn only_a_one_line_bash_command_is_read() {
        assert_eq!(
            bash_command("Bash command\n\n  git fetch origin\n  Fetch the remote").as_deref(),
            Some("git fetch origin")
        );
        assert_eq!(
            bash_command("Bash command\n  git status").as_deref(),
            Some("git status")
        );
        assert_eq!(bash_command("Bash command\n  a &&\n  b\n  Do both"), None);
        assert_eq!(bash_command("Edit file\n  src/lib.rs"), None);
        assert_eq!(bash_command("Bash command\n  git log …"), None);
    }

    fn dialog(kind: &'static str, excerpt: &str) -> Detected {
        Detected {
            question: "Do you want to proceed?".into(),
            command_excerpt: excerpt.into(),
            deadline_seconds: None,
            kind,
            options: Vec::new(),
            horizontal: false,
        }
    }

    #[test]
    fn the_whole_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("wt");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: /x/.git/worktrees/wt\n").unwrap();
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(shared.join(".git")).unwrap();

        let fetch = dialog("permission", "Bash command\n  git fetch origin\n  Fetch");
        assert_eq!(
            may_answer(&fetch, "Yes", &worktree, OWN).as_deref(),
            Ok("git fetch origin")
        );
        // Not the shared checkout, not "don't ask again", not another kind.
        assert!(may_answer(&fetch, "Yes", &shared, OWN).is_err());
        assert!(may_answer(&fetch, "Yes, and don't ask again for git", &worktree, OWN).is_err());
        assert!(may_answer(&fetch, "No", &worktree, OWN).is_err());
        let outside = dialog("read-outside-cwd", "Bash command\n  git fetch origin");
        assert!(may_answer(&outside, "Yes", &worktree, OWN).is_err());
        let merge = dialog("permission", "Bash command\n  gh pr merge 3\n  Merge");
        assert!(may_answer(&merge, "Yes", &worktree, OWN).is_err());
    }
}
