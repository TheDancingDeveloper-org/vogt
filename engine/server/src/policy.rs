//! Project agent-policy enforcement (WI-1243).
//!
//! A session carries the part of its project's policy the engine can check
//! ([`SessionPolicy`]): text that must never be written, and the rule that a
//! merge into a protected branch is a production deploy. The session's
//! PreToolUse hook posts each shell command to `POST
//! /api/agent-auth/policy-check`, which calls [`check`] and refuses the
//! command when it returns a reason.
//!
//! This is a guardrail, not a boundary. The session runs as the same user
//! that owns the hook, so it can edit the hook or its own settings, and the
//! check does not parse wrappers such as `bash -c`, `eval`, `xargs` or
//! `gh api`.
//!
//! The check looks at the command as a shell would run it. A line joined by
//! `&&`, `||`, `;` or `|`, or split over newlines, is several commands, and
//! any one of them refusing refuses the whole line. Assignments and the
//! `env`, `sudo`, `command`, `exec`, `nohup`, `nice` and `timeout` wrappers
//! in front of a command are stepped over, and so are git's global options,
//! so `sudo git commit` and `git -C ../other commit` are judged as the commit
//! they are. The program is matched by its basename, so `/usr/bin/git` is
//! `git`. The forbidden-text rule reads the whole line only when the line
//! contains a write verb (`git commit|push|merge|tag`, `gh pr
//! create|edit|merge`), which a `bash -c "git commit -m …"` still contains
//! as text; a read such as `git log --grep` is never refused by it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;
use vogt_engine_contract::SessionPolicy;

/// How much of a `git commit -F` message file is read. A commit message past
/// this is not one a person typed; refusing to read the rest bounds what a
/// hostile path can make the engine do.
const MESSAGE_FILE_LIMIT: u64 = 64 * 1024;

/// How long the branch lookup waits. A `git` that does not return is treated
/// as an unknown branch rather than allowed to stall the check.
const BRANCH_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

/// `Some(reason)` when `command`, run from `cwd`, breaks `policy`.
///
/// The reason names the rule and the pattern or the branch. It never repeats
/// the command or the text that matched: a forbidden pattern exists to keep a
/// secret out of a commit, and an error that quoted the match would put it in
/// the log instead.
///
/// A pattern that does not compile is skipped, never a blanket refusal: the
/// core already rejects a pattern it knows the engine cannot compile, and one
/// that still fails here must not turn every command into a denial.
pub fn check(policy: &SessionPolicy, command: &str, cwd: &Path) -> Option<String> {
    if line_writes(command) {
        for pattern in &policy.forbidden_text {
            let Ok(re) = Regex::new(pattern) else {
                tracing::warn!(
                    pattern,
                    "project policy pattern does not compile; skipping it"
                );
                continue;
            };
            if let Some(reason) = forbidden(&re, pattern, command, cwd) {
                return Some(reason);
            }
        }
    }
    if policy.merge_deploys_prod {
        let mains = mains(policy);
        for simple in split_commands(command) {
            if let Some(reason) = deploy(&simple, cwd, &mains) {
                return Some(reason);
            }
        }
    }
    None
}

/// Whether the line contains a write. Each simple command, and each quoted
/// word re-split (`bash -c "git commit -m x"`), is read the same way the
/// deploy check reads it: wrappers off, then git's global options and gh's
/// skipped, so `git -C . commit` and `gh -R o/r pr create` are writes.
fn line_writes(command: &str) -> bool {
    split_commands(command).iter().any(|simple| {
        let words = command_words(simple);
        writes(&words)
            || words.iter().any(|word| {
                split_commands(word)
                    .iter()
                    .any(|inner| writes(&command_words(inner)))
            })
    })
}

fn writes(words: &[String]) -> bool {
    match words.first().map(|w| base(w)) {
        Some("git") => matches!(
            git_subcommand(words).as_deref(),
            Some("commit" | "push" | "merge" | "tag")
        ),
        Some("gh") => matches!(gh_pr(words), Some("create" | "edit" | "merge")),
        _ => false,
    }
}

/// The branches a deploy rule protects: the ones named, or `main` when the
/// project names none.
fn mains(policy: &SessionPolicy) -> Vec<&str> {
    if policy.main_branches.is_empty() {
        vec!["main"]
    } else {
        policy.main_branches.iter().map(String::as_str).collect()
    }
}

fn forbidden(re: &Regex, pattern: &str, command: &str, cwd: &Path) -> Option<String> {
    let hit = format!("refusing: matches forbidden pattern /{pattern}/");
    if re.is_match(command) {
        return Some(hit);
    }
    for simple in split_commands(command) {
        let words = command_words(&simple);
        if let Some(file) = commit_message_file(&words) {
            if file_matches(re, &file, &git_dir(&words, cwd)) {
                return Some(hit);
            }
        }
        if pushes_or_opens(&words) {
            if let Some(branch) = current_branch(&git_dir(&words, cwd)) {
                if re.is_match(&branch) {
                    return Some(hit);
                }
            }
        }
    }
    None
}

/// Whether the command is a `git push` or a `gh pr create`: the two that
/// publish the current branch's name without it appearing in the command.
fn pushes_or_opens(words: &[String]) -> bool {
    match (
        words.first().map(|w| base(w)),
        git_subcommand(words).as_deref(),
    ) {
        (Some("git"), Some("push")) => true,
        (Some("gh"), _) => gh_pr(words) == Some("create"),
        _ => false,
    }
}

fn deploy(simple: &str, cwd: &Path, mains: &[&str]) -> Option<String> {
    let words = command_words(simple);
    match words.first().map(|w| base(w)) {
        Some("git") => match git_subcommand(&words).as_deref() {
            Some("push") => push_deploys(&words, cwd, mains),
            _ => None,
        },
        Some("gh") => match gh_pr(&words) {
            Some("merge") => {
                Some("refusing: merging a pull request deploys to production".to_string())
            }
            _ => None,
        },
        _ => None,
    }
}

/// A `git push` deploys when it names a protected branch as a destination, or
/// when it names no ref at all and the branch it would push (the current one)
/// is protected. `HEAD` and `@` are the current branch.
fn push_deploys(words: &[String], cwd: &Path, mains: &[&str]) -> Option<String> {
    let args = git_args(words);
    let mut refs = Vec::new();
    let mut seen_remote = false;
    for arg in args.into_iter().skip(1) {
        if arg == "--" {
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        if !seen_remote && !arg.contains(':') && !arg.starts_with('+') {
            seen_remote = true;
            continue;
        }
        refs.push(arg);
    }
    let dir = git_dir(words, cwd);
    let branch = current_branch(&dir);
    if refs.is_empty() {
        let branch = branch?;
        return is_main(&branch, mains)
            .then(|| format!("refusing: pushing {branch} deploys to production"));
    }
    refs.into_iter().find_map(|r| {
        let dest = r.split(':').next_back().unwrap_or(&r);
        let dest = dest.trim_start_matches('+');
        let dest = dest.strip_prefix("refs/heads/").unwrap_or(dest);
        // `HEAD` and `@` name no branch of their own; they are the current one.
        let dest = if dest == "HEAD" || dest == "@" {
            branch.as_deref().unwrap_or(dest)
        } else {
            dest
        };
        is_main(dest, mains).then(|| format!("refusing: pushing to {dest} deploys to production"))
    })
}

fn is_main(branch: &str, mains: &[&str]) -> bool {
    mains.contains(&branch)
}

/// The `gh pr <verb>` verb, when this is a pull-request subcommand. Global
/// flags in front of the subcommand are skipped, so `gh -R o/r pr merge` is
/// still a merge.
fn gh_pr(words: &[String]) -> Option<&str> {
    let mut args = words.iter().map(String::as_str).skip(1);
    let mut sub = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            sub = args.next();
            break;
        }
        if arg.starts_with("--repo=") {
            continue;
        }
        if arg == "-R" || arg == "--repo" {
            args.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        sub = Some(arg);
        break;
    }
    if sub == Some("pr") {
        return args.find(|a| !a.starts_with('-'));
    }
    None
}

/// The path a `git commit` reads its message from, when it uses `-F`.
fn commit_message_file(words: &[String]) -> Option<PathBuf> {
    if git_subcommand(words).as_deref() != Some("commit") {
        return None;
    }
    let args = git_args(words);
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        if let Some(path) = arg.strip_prefix("--file=") {
            return Some(PathBuf::from(path));
        }
        if arg == "-F" || arg == "--file" {
            return rest.next().map(PathBuf::from);
        }
    }
    None
}

/// Read at most [`MESSAGE_FILE_LIMIT`] of `file` and test it.
///
/// Only a regular file is opened, and it is opened `O_NONBLOCK`: a fifo
/// (`mkfifo m; git commit -F m`) or a device would otherwise block the engine
/// forever. A symlink is not followed. A file that cannot be read is not a
/// match: the commit will fail on its own, and a missing file is not the text
/// the rule exists to catch.
fn file_matches(re: &Regex, file: &Path, cwd: &Path) -> bool {
    let path = if file.is_absolute() {
        file.to_path_buf()
    } else {
        cwd.join(file)
    };
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return false;
    };
    if !meta.file_type().is_file() {
        return false;
    }
    let mut options = std::fs::File::options();
    options.read(true);
    // `O_NONBLOCK` so a file that is not a regular file despite the metadata
    // check — a race between the stat and the open — cannot stall the read.
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let Ok(mut f) = options.open(path) else {
        return false;
    };
    let mut buf = vec![0_u8; MESSAGE_FILE_LIMIT as usize];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&buf[..n]) else {
        return false;
    };
    re.is_match(text)
}

/// The directory git runs in: `cwd`, unless the command points elsewhere with
/// `-C`, `--git-dir` or `--work-tree`.
fn git_dir(words: &[String], cwd: &Path) -> PathBuf {
    let mut dir = cwd.to_path_buf();
    let mut args = words.iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(path) = arg.strip_prefix("-C") {
            let path = if path.is_empty() {
                args.next().map(String::as_str).unwrap_or("")
            } else {
                path
            };
            dir = resolve_dir(path, cwd);
        } else if let Some(path) = arg
            .strip_prefix("--git-dir=")
            .or_else(|| arg.strip_prefix("--work-tree="))
        {
            dir = resolve_dir(path, cwd);
        } else if !arg.starts_with('-') {
            break;
        }
    }
    dir
}

/// An absolute directory stands on its own; a relative one is relative to the
/// session's cwd, not to whatever directory the previous flag named.
fn resolve_dir(path: &str, cwd: &Path) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

/// `git rev-parse --abbrev-ref HEAD` in `dir`. `None` when there is no
/// repository, no git, or the lookup does not return within
/// [`BRANCH_LOOKUP_TIMEOUT`]: a rule that needs the branch then does not fire,
/// and the test that needs it to fire gives a directory with a HEAD.
fn current_branch(dir: &Path) -> Option<String> {
    let mut child = std::process::Command::new("git")
        .args(["--no-optional-locks", "-C"])
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = child.stdout.take()?;
                let mut buf = String::new();
                stdout.read_to_string(&mut buf).ok()?;
                let branch = buf.trim();
                return (!branch.is_empty() && branch != "HEAD").then(|| branch.to_string());
            }
            Ok(None) if started.elapsed() < BRANCH_LOOKUP_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The git subcommand: the first argument that is not a global option.
fn git_subcommand(words: &[String]) -> Option<String> {
    git_args(words).into_iter().next()
}

/// The arguments after git's global options.
fn git_args(words: &[String]) -> Vec<String> {
    let mut args = words.iter().skip(1);
    let mut out = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "-C" || arg == "-c" {
            args.next();
            continue;
        }
        if arg.starts_with("-C") || arg.starts_with("--git-dir") || arg.starts_with("--work-tree") {
            continue;
        }
        if arg == "--" {
            out.extend(args.cloned());
            break;
        }
        if arg.starts_with('-') {
            continue;
        }
        out.push(arg.clone());
        out.extend(args.cloned());
        break;
    }
    out
}

/// The program's name, without its directory.
fn base(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// One shell command, with the leading assignments and wrappers removed, split
/// into words.
fn command_words(simple: &str) -> Vec<String> {
    let mut words = shell_words(simple);
    while words.len() > 1 && is_assignment(&words[0]) {
        words.remove(0);
    }
    loop {
        match words.first().map(|w| base(w)) {
            Some("sudo") => {
                words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    words.remove(0);
                }
            }
            Some("command" | "exec" | "nohup") => {
                words.remove(0);
            }
            Some("nice") => {
                words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    let flag = words.remove(0);
                    // `nice -n 5`: the adjustment is the next word.
                    if matches!(flag.as_str(), "-n" | "--adjustment")
                        && words.first().is_some_and(|w| !w.starts_with('-'))
                    {
                        words.remove(0);
                    }
                }
            }
            Some("timeout") => {
                words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    words.remove(0);
                }
                // The duration (`timeout 10 git …`).
                if words.first().is_some_and(|w| !w.starts_with('-')) {
                    words.remove(0);
                }
            }
            Some("env") => {
                words.remove(0);
                while words
                    .first()
                    .is_some_and(|w| w.starts_with('-') || is_assignment(w))
                {
                    words.remove(0);
                }
            }
            _ => break,
        }
    }
    words
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Split a command line into the simple commands a shell would run. Quotes
/// suppress splitting, so `echo "a && b"` stays one command.
fn split_commands(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None if c == '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            None if c == '\n' || c == ';' => {
                push_command(&mut out, &mut cur);
            }
            None if c == '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                push_command(&mut out, &mut cur);
            }
            None if c == '&' && chars.peek() == Some(&'&') => {
                chars.next();
                push_command(&mut out, &mut cur);
            }
            None => cur.push(c),
        }
    }
    push_command(&mut out, &mut cur);
    out
}

fn push_command(out: &mut Vec<String>, cur: &mut String) {
    let trimmed = cur.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    cur.clear();
}

/// A conservative word split: whitespace-separated, with single and double
/// quotes kept intact and their contents unsplit. Enough for the verbs and
/// flags the rules read; it does not evaluate anything.
fn shell_words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(forbidden: &[&str], merge: bool, mains: &[&str]) -> SessionPolicy {
        SessionPolicy {
            forbidden_text: forbidden.iter().map(|s| s.to_string()).collect(),
            merge_deploys_prod: merge,
            main_branches: mains.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn cwd() -> PathBuf {
        std::env::temp_dir()
    }

    /// A throwaway git repository whose current branch is `branch`.
    fn repo(branch: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vogt-policy-{}-{}",
            std::process::id(),
            branch.replace('/', "-")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", branch],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        }
        std::fs::write(dir.join("f"), "x").unwrap();
        for args in [vec!["add", "f"], vec!["commit", "-q", "-m", "init"]] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        }
        dir
    }

    #[test]
    fn allows_a_read() {
        let p = policy(&["sekret"], true, &[]);
        for command in [
            "git status",
            "git log --oneline --grep=sekret",
            "git status # sekret",
            "gh pr view 7",
            "gh pr view 1 | grep sekret",
            "echo sekret",
            "echo hello",
        ] {
            assert_eq!(check(&p, command, &cwd()), None, "{command}");
        }
    }

    #[test]
    fn forbidden_text_matches_a_write_and_not_a_read() {
        let p = policy(&["sekret"], false, &[]);
        for command in [
            "git commit -m sekret",
            "git push -o sekret",
            "git merge sekret",
            "git tag sekret",
            "gh pr create --title sekret",
            "gh pr edit 1 --body sekret",
            "gh pr merge 1 --subject sekret",
            "bash -c \"git commit -m sekret\"",
            "git -C . commit -m sekret",
            "git -c x=y commit -m sekret",
            "git --no-pager commit -m sekret",
            "bash -c \"git -C x commit -m sekret\"",
            "bash -lc 'cd x && git commit -m sekret'",
            "gh -R o/r pr create --title sekret",
        ] {
            let reason = check(&p, command, &cwd()).unwrap_or_else(|| panic!("{command}"));
            assert!(reason.contains("/sekret/"), "{command}: {reason}");
            assert!(!reason.contains("commit"), "{command}: {reason}");
        }
    }

    #[test]
    fn quoting_does_not_hide_a_write() {
        let p = policy(&[], true, &[]);
        assert!(check(&p, "echo \"git push origin main\"", &cwd()).is_none());
        assert!(check(&p, "sudo git push origin main", &cwd()).is_some());
        assert!(check(&p, "FOO=1 env BAR=2 git push origin main", &cwd()).is_some());
        assert!(check(&p, "git status && git push origin main", &cwd()).is_some());
    }

    #[test]
    fn wrappers_and_absolute_paths_do_not_hide_a_push() {
        let p = policy(&[], true, &[]);
        for command in [
            "/usr/bin/git push origin main",
            "command git push origin main",
            "exec git push origin main",
            "nohup git push origin main",
            "nice git push origin main",
            "nice -n 5 git push origin main",
            "timeout 10 git push origin main",
            "timeout --foreground 10 git push origin main",
        ] {
            assert!(check(&p, command, &cwd()).is_some(), "{command}");
        }
    }

    #[test]
    fn gh_global_flags_do_not_hide_a_merge() {
        let p = policy(&[], true, &[]);
        for command in [
            "gh -R o/r pr merge 12",
            "gh --repo o/r pr merge 12",
            "gh --repo=o/r pr merge 12",
            "/usr/bin/gh pr merge 12",
        ] {
            assert!(check(&p, command, &cwd()).is_some(), "{command}");
        }
        assert!(check(&p, "gh -R o/r pr view 12", &cwd()).is_none());
    }

    #[test]
    fn commit_message_file_is_read() {
        let dir = std::env::temp_dir().join("x").join("wt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("msg.txt"), "the sekret token").unwrap();
        let p = policy(&["sekret"], false, &[]);
        assert!(check(&p, "git commit -F msg.txt", &dir).is_some());
        assert!(check(&p, "git commit --file=msg.txt -m fine", &dir).is_some());
        assert!(check(&p, "git commit -F absent.txt", &dir).is_none());
        assert!(check(
            &p,
            &format!("git -C {} commit -F msg.txt", dir.display()),
            &cwd()
        )
        .is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fifo_message_file_is_not_opened() {
        let dir = std::env::temp_dir().join(format!("vogt-policy-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(dir.join("m"))
            .status()
            .unwrap()
            .success());
        let p = policy(&["sekret"], false, &[]);
        let started = Instant::now();
        assert!(check(&p, "git commit -F m", &dir).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_is_always_refused() {
        let p = policy(&[], true, &[]);
        assert!(check(&p, "gh pr merge 7", &cwd()).is_some());
        assert!(check(&p, "gh pr merge --squash 7", &cwd()).is_some());
        assert!(check(&p, "gh pr view 7", &cwd()).is_none());
    }

    #[test]
    fn push_to_a_protected_ref_is_refused() {
        let p = policy(&[], true, &["main", "release"]);
        assert!(check(&p, "git push origin main", &cwd()).is_some());
        assert!(check(&p, "git push origin HEAD:refs/heads/release", &cwd()).is_some());
        assert!(check(&p, "git push origin +feature:main", &cwd()).is_some());
        assert!(check(&p, "git -C ../elsewhere push origin main", &cwd()).is_some());
        assert!(check(&p, "git push origin feature/wi-1", &cwd()).is_none());
    }

    #[test]
    fn pushing_head_pushes_the_current_branch() {
        let main = repo("main");
        let topic = repo("topic");
        let p = policy(&[], true, &[]);
        for command in [
            "git push origin HEAD",
            "git push -u origin HEAD",
            "git push origin @",
            "git push origin topic:HEAD",
        ] {
            assert!(check(&p, command, &main).is_some(), "{command}");
            assert!(check(&p, command, &topic).is_none(), "{command}");
        }
        std::fs::remove_dir_all(&main).ok();
        std::fs::remove_dir_all(&topic).ok();
    }

    #[test]
    fn a_bad_pattern_is_skipped() {
        let p = policy(&["["], false, &[]);
        assert!(check(&p, "git commit -m anything", &cwd()).is_none());
    }
}
