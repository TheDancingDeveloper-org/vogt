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
//! `&&`, `||`, `;`, `|` or a background `&`, split over newlines or wrapped
//! in a `( … )` subshell, is several commands, and any one of them refusing
//! refuses the whole line. Reserved words (`if`, `then`, `do`, `{`, `!`,
//! `time`, …) in front of a command are stepped over. Assignments and the
//! `env`, `sudo`, `command`, `exec`, `nohup`, `nice` and `timeout` wrappers
//! in front of a command are stepped over, and so are git's global options,
//! so `sudo git commit` and `git -C ../other commit` are judged as the commit
//! they are. The program is matched by its basename, so `/usr/bin/git` is
//! `git`. The forbidden-text rule reads the whole line only when the line
//! contains a write verb (`git commit|push|merge|tag`, `gh pr
//! create|edit|merge`), which a `bash -c "git commit -m …"` still contains
//! as text; a read such as `git log --grep` is never refused by it.
//!
//! Two rules read the session's workspace (WI-1326). The discard guard, after
//! msp-klaudia's gitguard, refuses a git command in the session's own repo
//! that would throw away a path in `discard_baseline` — the paths that were
//! already dirty when the session started; `reset --hard`, `clean`, `stash`
//! and a command whose paths cannot be read (a glob, an expansion, `xargs`,
//! `eval`, a `cd` earlier on the line) count as reaching every one. A fresh
//! worktree has an empty baseline, so the guard is a no-op there. The
//! protected-checkout rule holds only a `worktree` session: it refuses a git
//! write whose repo is a registered project checkout other than the
//! session's own worktree. Repos are resolved by `git rev-parse
//! --show-toplevel`; a lookup that fails is the session's own repo for the
//! first rule and not protected for the second.

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
    let worktree = policy.workspace_kind.as_deref() == Some("worktree");
    if policy.discard_baseline.is_empty() && !(worktree && !policy.protected_checkouts.is_empty()) {
        return None;
    }
    let calls = git_calls(command, cwd);
    let mut repos = Repos::default();
    if !policy.discard_baseline.is_empty() {
        if let Some(reason) = discard(policy, &calls, &mut repos) {
            return Some(reason);
        }
    }
    if worktree {
        if let Some(reason) = protected(policy, &calls, &mut repos) {
            return Some(reason);
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
    let branch = git_line(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    (branch != "HEAD").then_some(branch)
}

/// The worktree toplevel `git rev-parse --show-toplevel` names for `dir`,
/// canonicalised. Never `--git-common-dir`: a linked worktree is its own
/// repo here, not the checkout it was cut from. `None` when the lookup fails
/// or times out — "unknown repo".
fn toplevel(dir: &Path) -> Option<PathBuf> {
    let top = git_line(dir, &["rev-parse", "--show-toplevel"])?;
    Some(canonical(Path::new(&top)))
}

/// What `git -C dir <args>` prints, trimmed, bounded by
/// [`BRANCH_LOOKUP_TIMEOUT`]. `None` on failure, empty output or timeout.
fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_output(dir, args, BRANCH_LOOKUP_TIMEOUT)?;
    let line = out.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// What `git -C dir <args>` prints, or `None` when it fails or does not
/// return within `timeout`. Stdout is drained while git runs, so a large
/// answer cannot fill the pipe and stall it into the timeout.
fn git_output(dir: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = std::process::Command::new("git")
        .args(["--no-optional-locks", "-C"])
        .arg(dir)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).map(|_| buf)
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().ok()?.ok()?;
                return status
                    .success()
                    .then(|| String::from_utf8_lossy(&buf).into_owned());
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `policy` with the session's workspace block (WI-1324) filled in for the
/// discard and protected-checkout rules (WI-1326): its kind, its toplevel and
/// the paths already dirty in it when the session first started. The engine
/// captures that block before the child runs — in the worktree it has just
/// cut for a worktree start (so the baseline is what the base ref left dirty,
/// usually nothing), in the cwd's tree otherwise — and a wake passes the
/// record's copy, so the session's own edits never become its baseline. With no
/// block (a session from before WI-1324, or a cwd outside any repository)
/// the policy is returned as it is: neither rule applies, and the agent's
/// own guard (Klaudia's gitguard) stays the one that judges discards.
pub fn with_workspace(
    policy: Option<SessionPolicy>,
    workspace: Option<&vogt_engine_contract::Workspace>,
) -> Option<SessionPolicy> {
    let Some(ws) = workspace else {
        return policy;
    };
    let mut policy = policy.unwrap_or_default();
    policy.workspace_kind = Some(ws.kind.as_str().to_string());
    let mut baseline = ws.baseline_dirty.clone();
    baseline.sort();
    baseline.dedup();
    policy.discard_baseline = baseline;
    policy.workspace_path = Some(ws.path.clone());
    Some(policy)
}

/// How deep `bash -c`, `eval` and `xargs` payloads are followed.
const MAX_DEPTH: usize = 4;

/// One git invocation on the line, with the directory it runs in as far as
/// it can be read.
#[derive(Debug)]
struct GitCall {
    words: Vec<String>,
    /// The session cwd, or the target of a literal `cd` earlier on the line.
    cwd: PathBuf,
    /// The paths the command names cannot be read: a `cd`, `eval` or `xargs`
    /// before it, an expansion in it, or git pointed elsewhere by
    /// `--git-dir`, `--work-tree`, `GIT_DIR` or `core.worktree`. The discard
    /// guard then counts every protected path as reached.
    unreadable: bool,
    /// The line defines a git alias (`git -c alias.x=…`): its subcommand is
    /// not the command that runs, so it may be any write or discard.
    aliased: bool,
}

/// The words after an expanded program (`$GIT`, `${GIT}`, `$(which git)`,
/// `` `which git` ``), or `None` when the program is a plain word.
fn expanded_program(words: &[String]) -> Option<Vec<String>> {
    let first = words.first()?;
    let end = if first.starts_with("$(") {
        words.iter().position(|w| w.ends_with(')'))?
    } else if let Some(tail) = first.strip_prefix('`') {
        if tail.contains('`') {
            0
        } else {
            words.iter().skip(1).position(|w| w.contains('`'))? + 1
        }
    } else {
        // `$GIT`, `${GIT_BIN}`: a variable named for git. `$EDITOR` is not.
        let name = first.strip_prefix('$')?;
        if !name.to_ascii_lowercase().contains("git") {
            return None;
        }
        0
    };
    Some(words[end + 1..].to_vec())
}

/// Whether `git`'s global options define an alias inline, or include a
/// config file that may define one (`include.path`, `includeIf.*.path`).
fn inline_alias(words: &[String]) -> bool {
    let is_alias = |v: &str| {
        let v = v.to_ascii_lowercase();
        v.starts_with("alias.") || v.starts_with("include.") || v.starts_with("includeif.")
    };
    words.iter().enumerate().any(|(i, w)| {
        (w == "-c" && words.get(i + 1).is_some_and(|v| is_alias(v)))
            || w.strip_prefix("-c").is_some_and(is_alias)
            || w.starts_with("--config-env")
    })
}

/// A `cd` target that names one directory without the shell's help: absolute,
/// with no expansion or glob.
fn literal_absolute(target: &str) -> bool {
    target.starts_with('/') && !target.contains(['$', '`', '*', '?', '[', '~'])
}

/// Every git invocation in `command`, following `sh -c` payloads, `eval`,
/// `xargs` and `cd` the conservative way gitguard does: whatever moves the
/// shell or hides the paths makes the paths after it unreadable.
fn git_calls(command: &str, cwd: &Path) -> Vec<GitCall> {
    let mut out = Vec::new();
    let moved = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "CDPATH",
    ]
    .iter()
    .any(|v| command.contains(v))
        || command.to_ascii_lowercase().contains("core.worktree");
    // Config from the environment (`GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n`,
    // `GIT_CONFIG_PARAMETERS`, another config file) may define an alias, so
    // every git on the line is read as one.
    let config_env = [
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG=",
    ]
    .iter()
    .any(|v| command.contains(v));
    walk(command, cwd, moved, config_env, 0, &mut out);
    out
}

fn walk(
    command: &str,
    cwd: &Path,
    unreadable: bool,
    config_env: bool,
    depth: usize,
    out: &mut Vec<GitCall>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut dir = cwd.to_path_buf();
    let mut unreadable = unreadable;
    for simple in split_commands(command) {
        // `$( … )` and backticks run their own commands, from this
        // directory, before this one.
        for body in substitutions(&simple) {
            walk(&body, &dir, unreadable, config_env, depth + 1, out);
        }
        let mut words = command_words(&simple);
        if words.is_empty() {
            continue;
        }
        // `$GIT`, `$(which git)` or `` `which git` `` as the program: read
        // it as git with paths that cannot be read.
        let mut expanded = false;
        if let Some(rest) = expanded_program(&words) {
            words = std::iter::once("git".to_string()).chain(rest).collect();
            expanded = true;
        }
        let first = words[0].clone();
        let first = first.as_str();
        match base(first) {
            "cd" if words.len() == 2 && literal_absolute(&words[1]) => {
                // An absolute literal target is not looked up in CDPATH, so
                // the paths after it resolve from there. If the cd fails,
                // `&&` runs nothing after it; a `;` line that runs on in
                // the old directory is the one reading this gets wrong.
                dir = normalize(Path::new(&words[1]));
            }
            "cd" | "pushd" | "popd" => {
                // Whether the cd ran, and where CDPATH sent it, is not
                // something this reading knows: the paths after it are
                // unreadable. The directory is followed for the
                // protected-checkout rule, which only needs the repo.
                unreadable = true;
                if base(first) == "cd" && words.len() == 2 {
                    let target = &words[1];
                    if !target.starts_with(['-', '~']) && !target.contains(['$', '`']) {
                        dir = resolve_dir(target, &dir);
                    }
                }
            }
            "bash" | "sh" | "zsh" | "dash" | "ksh" => {
                if let Some(payload) = shell_payload(&words) {
                    walk(payload, &dir, unreadable, config_env, depth + 1, out);
                }
            }
            "eval" => {
                walk(
                    &words[1..].join(" "),
                    &dir,
                    true,
                    config_env,
                    depth + 1,
                    out,
                );
                // eval runs in this shell: a cd inside it moves the rest.
                unreadable = true;
            }
            "xargs" => {
                // The paths arrive on stdin.
                walk(
                    &xargs_command(&words).join(" "),
                    &dir,
                    true,
                    config_env,
                    depth + 1,
                    out,
                );
            }
            "git" => {
                let hidden = expanded
                    || simple.contains(['$', '`'])
                    || words
                        .iter()
                        .any(|w| w.starts_with("--git-dir") || w.starts_with("--work-tree"));
                let aliased = config_env || inline_alias(&words);
                out.push(GitCall {
                    words,
                    cwd: dir.clone(),
                    unreadable: unreadable || hidden || aliased,
                    aliased,
                });
            }
            _ => {}
        }
    }
}

/// The bodies of the `$( … )` and backtick substitutions in `simple`, outside
/// single quotes. A nested one is found when its body is walked.
fn substitutions(simple: &str) -> Vec<String> {
    let chars: Vec<char> = simple.chars().collect();
    let mut out = Vec::new();
    let mut single = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if single {
            single = c != '\'';
        } else if c == '\\' {
            i += 1;
        } else if c == '\'' {
            single = true;
        } else if c == '$' && chars.get(i + 1) == Some(&'(') {
            let start = i + 2;
            let mut depth = 1;
            let mut j = start;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let end = if depth == 0 { j - 1 } else { j };
            out.push(chars[start..end].iter().collect());
            i = j;
            continue;
        } else if c == '`' {
            let start = i + 1;
            let end = chars[start..]
                .iter()
                .position(|&c| c == '`')
                .map_or(chars.len(), |p| start + p);
            out.push(chars[start..end].iter().collect());
            i = end + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// The script a `sh -c` runs: the word after the first option cluster that
/// contains `c` (`-c`, `-lc`, `-ec`).
fn shell_payload(words: &[String]) -> Option<&str> {
    let mut args = words.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c') {
            return args.next().map(String::as_str);
        }
        if !arg.starts_with('-') {
            return None;
        }
    }
    None
}

/// The command `xargs` runs, its own options removed.
fn xargs_command(words: &[String]) -> &[String] {
    let mut i = 1;
    while i < words.len() && words[i].starts_with('-') {
        if matches!(
            words[i].as_str(),
            "-I" | "-n" | "-P" | "-d" | "-L" | "-s" | "-a" | "-E" | "-e"
        ) {
            i += 1;
        }
        i += 1;
    }
    &words[i.min(words.len())..]
}

/// Toplevel lookups, one per directory per check: the whole check runs
/// under the policy-check route's bound.
#[derive(Default)]
struct Repos(std::collections::HashMap<PathBuf, Option<PathBuf>>);

impl Repos {
    fn top(&mut self, dir: &Path) -> Option<PathBuf> {
        self.0
            .entry(dir.to_path_buf())
            .or_insert_with(|| toplevel(dir))
            .clone()
    }
}

/// What a discarding git invocation can reach: every protected path, or the
/// pathspecs it names (an empty list reaches everything).
struct Reach {
    all: bool,
    paths: Vec<String>,
}

/// The discard guard (WI-1326), after msp-klaudia's gitguard: refuse a git
/// command that would throw away a path that had uncommitted changes when
/// the session started. Only commands in the session's own repo count; a
/// repo that cannot be resolved is taken to be the session's own.
fn discard(policy: &SessionPolicy, calls: &[GitCall], repos: &mut Repos) -> Option<String> {
    let own = policy
        .workspace_path
        .as_deref()
        .map(|p| canonical(Path::new(p)));
    for call in calls {
        let args = git_args(&call.words);
        let Some(sub) = args.first() else {
            continue;
        };
        // A subcommand git does not ship may be an alias from the config
        // (`git config alias.x 'checkout --'`): it may discard anything.
        let reach = if call.aliased || !builtin(sub) {
            Reach {
                all: true,
                paths: Vec::new(),
            }
        } else if let Some(reach) = discards(sub, &args[1..]) {
            reach
        } else {
            continue;
        };
        let dir = canonical(&git_dir(&call.words, &call.cwd));
        let top = if call.unreadable {
            None
        } else {
            repos.top(&dir)
        };
        if let (Some(top), Some(own)) = (&top, &own) {
            if top != own {
                continue;
            }
        }
        let root = top.or_else(|| own.clone()).unwrap_or_else(|| dir.clone());
        let unreadable = call.unreadable || reach.paths.iter().any(|p| !readable_spec(p));
        let hits = if reach.all || unreadable || reach.paths.is_empty() {
            policy.discard_baseline.len()
        } else {
            let specs: Vec<String> = reach
                .paths
                .iter()
                .filter_map(|p| repo_relative(p, &dir, &root))
                .collect();
            policy
                .discard_baseline
                .iter()
                .filter(|entry| specs.iter().any(|spec| reaches(spec, entry)))
                .count()
        };
        if hits > 0 {
            return Some(format!(
                "refusing: discard guard: git {sub} would discard {hits} path(s) that had \
                 uncommitted changes before this session started; name only files this \
                 session changed"
            ));
        }
    }
    None
}

/// Which discarding form `git <sub> <args>` is, if any. Mirrors gitguard's
/// `discard`, minus its staging rules.
fn discards(sub: &str, args: &[String]) -> Option<Reach> {
    let (valued, valued_short) = valued(sub);
    let (flags, paths, dashdash) = split_args(args, valued);
    // A long option may be abbreviated to any prefix git can tell apart
    // (`--ha` is `--hard`); a short one may sit in a cluster (`-qf`).
    let has = |names: &[&str]| {
        flags.iter().any(|f| {
            names.iter().any(|n| {
                if n.starts_with("--") {
                    long_matches(f, n)
                } else {
                    f == n || short_in_cluster(f, n.chars().nth(1).unwrap_or('-'), valued_short)
                }
            })
        })
    };
    let short = |letter: char| {
        flags
            .iter()
            .any(|f| short_in_cluster(f, letter, valued_short))
    };
    let all = || {
        Some(Reach {
            all: true,
            paths: Vec::new(),
        })
    };
    let these = |paths: Vec<String>| Some(Reach { all: false, paths });
    match sub {
        "checkout" => {
            if has(&["-f", "--force"]) {
                return all();
            }
            if has(&["-b", "-B", "--orphan"]) && !dashdash {
                // `-b` takes the branch name, so `-fb x` is still `-f`:
                // the force was checked above.
                return None;
            }
            if dashdash {
                return these(after_dashdash(args));
            }
            // `git checkout <x>` is a branch switch, which git refuses over
            // local changes, or a path restore: every word may be a path.
            (!paths.is_empty()).then_some(Reach { all: false, paths })
        }
        "restore" => {
            let staged = has(&["--staged", "-S"]) || short('S');
            let worktree = has(&["--worktree", "-W"]) || short('W');
            if staged && !worktree {
                return None; // unstages only; the working tree is left alone
            }
            these(paths)
        }
        "reset" => has(&["--hard", "--merge", "--keep"]).then_some(Reach {
            all: true,
            paths: Vec::new(),
        }),
        "clean" => {
            if has(&["-n", "--dry-run"]) || short('n') {
                return None;
            }
            (has(&["-f", "--force"]) || short('f')).then_some(Reach { all: false, paths })
        }
        "stash" => match paths.first().map(String::as_str) {
            None | Some("push") => these(paths.into_iter().skip(1).collect()),
            Some("save") => all(),
            // A bare `git stash -- <paths>` is a push.
            Some(_) if dashdash => these(after_dashdash(args)),
            _ => None,
        },
        "rm" => ((has(&["-f", "--force"]) || short('f')) && !has(&["--cached"]))
            .then_some(Reach { all: false, paths }),
        "switch" => has(&["-f", "--force", "--discard-changes"]).then_some(Reach {
            all: true,
            paths: Vec::new(),
        }),
        _ => None,
    }
}

/// The options of a discarding subcommand that take a value, long and short.
/// They differ per subcommand: `-m` is `stash`'s message but `checkout`'s
/// `--merge`, and `-c` is `switch --create`. In a cluster, the rest of the
/// word after a valued short option is its value (`-sSTABLE`, `-cfoo`).
fn valued(sub: &str) -> (&'static [&'static str], &'static [char]) {
    match sub {
        "checkout" => (&["-b", "-B", "--orphan", "--conflict"], &['b', 'B']),
        "switch" => (
            &[
                "-c",
                "-C",
                "--create",
                "--force-create",
                "--orphan",
                "--conflict",
            ],
            &['c', 'C'],
        ),
        "restore" => (
            &["-s", "--source", "--conflict", "--pathspec-from-file"],
            &['s'],
        ),
        "stash" => (&["-m", "--message", "--pathspec-from-file"], &['m']),
        "clean" => (&["-e", "--exclude"], &['e']),
        "rm" | "reset" => (&["--pathspec-from-file"], &[]),
        _ => (&[], &[]),
    }
}

/// Whether the short option `-<letter>` is in the cluster `flag` (`-qf`),
/// reading no further than an option that takes the rest as its value.
fn short_in_cluster(flag: &str, letter: char, valued: &[char]) -> bool {
    let Some(cluster) = flag.strip_prefix('-') else {
        return false;
    };
    if cluster.starts_with('-') {
        return false;
    }
    for c in cluster.chars() {
        if c == letter {
            return true;
        }
        if valued.contains(&c) {
            return false;
        }
    }
    false
}

/// The shortest prefix git accepts for each long option the guard reads:
/// enough to tell it from the subcommand's other options (`--s` could be
/// restore's `--source`, `--d` switch's `--detach`).
const LONG_MIN: &[(&str, usize)] = &[
    ("--staged", 4),
    ("--discard-changes", 4),
    ("--dry-run", 3),
    ("--hard", 3),
    ("--merge", 3),
    ("--keep", 3),
    ("--force", 3),
    ("--worktree", 3),
    ("--cached", 3),
    ("--orphan", 3),
];

/// Whether `flag` is the long option `name`, whole or abbreviated, with or
/// without an attached `=value`.
fn long_matches(flag: &str, name: &str) -> bool {
    if !flag.starts_with("--") {
        return false;
    }
    let flag = flag.split_once('=').map_or(flag, |(f, _)| f);
    let min = LONG_MIN
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(name.len(), |(_, m)| *m);
    flag.len() >= min && name.starts_with(flag)
}

/// Flags and positional words, the value of an option in `valued` consumed
/// with it. Everything after `--` is positional.
fn split_args(args: &[String], valued: &[&str]) -> (Vec<String>, Vec<String>, bool) {
    let mut flags = Vec::new();
    let mut paths = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            paths.extend(rest.cloned());
            return (flags, paths, true);
        }
        if arg.starts_with('-') && arg != "-" {
            flags.push(arg.clone());
            if valued.contains(&arg.as_str()) {
                rest.next();
            }
        } else {
            paths.push(arg.clone());
        }
    }
    (flags, paths, false)
}

fn after_dashdash(args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|a| *a != "--")
        .skip(1)
        .cloned()
        .collect()
}

/// A pathspec this reading can resolve: no glob, expansion or pathspec magic.
fn readable_spec(spec: &str) -> bool {
    !spec.starts_with([':', '~']) && !spec.contains(['*', '?', '[', '$', '`'])
}

/// `spec`, relative to `dir`, as a path relative to `root`; `""` is the
/// whole tree. `None` when it lies outside `root`.
fn repo_relative(spec: &str, dir: &Path, root: &Path) -> Option<String> {
    let path = normalize(&resolve_dir(spec, dir));
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().trim_end_matches('/').to_string())
}

/// Whether the repo-relative pathspec `spec` reaches the baseline `entry`:
/// the same path, a directory holding it, or a file inside an untracked
/// directory entry (`scratch/`).
fn reaches(spec: &str, entry: &str) -> bool {
    let entry_path = entry.trim_end_matches('/');
    spec.is_empty()
        || spec == entry_path
        || entry_path.starts_with(&format!("{spec}/"))
        || (entry.ends_with('/') && spec.starts_with(entry))
}

/// The protected-checkout rule (WI-1326), only for a `worktree` session:
/// refuse a git write whose repo is a registered project's checkout and not
/// the session's own worktree. A repo that cannot be resolved is not
/// protected, so a broken git never refuses an ordinary command.
fn protected(policy: &SessionPolicy, calls: &[GitCall], repos: &mut Repos) -> Option<String> {
    let own = policy
        .workspace_path
        .as_deref()
        .map(|p| canonical(Path::new(p)));
    let checkouts: Vec<PathBuf> = policy
        .protected_checkouts
        .iter()
        .map(|p| canonical(Path::new(p)))
        .collect();
    for call in calls {
        let args = git_args(&call.words);
        let Some(sub) = args.first() else {
            continue;
        };
        if !call.aliased && !git_write(sub, &args[1..]) {
            continue;
        }
        let Some(top) = repos.top(&canonical(&git_dir(&call.words, &call.cwd))) else {
            continue;
        };
        if own.as_ref() == Some(&top) || !checkouts.contains(&top) {
            continue;
        }
        return Some(format!(
            "refusing: protected checkout: git {sub} writes to a project checkout, not this \
             session's worktree; run it in {}",
            own.as_deref()
                .map_or_else(|| "the worktree".into(), |p| p.display().to_string())
        ));
    }
    None
}

/// Git's own commands (`git help -a`), plus the common external ones. Any
/// other subcommand may be an alias.
const BUILTIN: &[&str] = &[
    "add",
    "am",
    "annex",
    "annotate",
    "apply",
    "archimport",
    "archive",
    "backfill",
    "bisect",
    "blame",
    "branch",
    "bugreport",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "checkout",
    "checkout-index",
    "cherry",
    "cherry-pick",
    "citool",
    "clean",
    "clone",
    "column",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "credential",
    "cvsexportcommit",
    "cvsimport",
    "cvsserver",
    "daemon",
    "describe",
    "diagnose",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "fast-export",
    "fast-import",
    "fetch",
    "fetch-pack",
    "filter-branch",
    "filter-repo",
    "flow",
    "fmt-merge-msg",
    "for-each-ref",
    "for-each-repo",
    "format-patch",
    "fsck",
    "fsmonitor--daemon",
    "gc",
    "get-tar-commit-id",
    "gitk",
    "grep",
    "gui",
    "hash-object",
    "help",
    "hook",
    "http-backend",
    "imap-send",
    "index-pack",
    "init",
    "instaweb",
    "interpret-trailers",
    "lfs",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "maintenance",
    "mailinfo",
    "mailsplit",
    "merge",
    "merge-base",
    "merge-file",
    "merge-index",
    "merge-one-file",
    "merge-tree",
    "mergetool",
    "mktag",
    "mktree",
    "multi-pack-index",
    "mv",
    "name-rev",
    "notes",
    "p4",
    "pack-objects",
    "pack-redundant",
    "pack-refs",
    "patch-id",
    "prune",
    "prune-packed",
    "pull",
    "push",
    "quiltimport",
    "range-diff",
    "read-tree",
    "rebase",
    "reflog",
    "refs",
    "remote",
    "repack",
    "replace",
    "replay",
    "request-pull",
    "rerere",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "scalar",
    "send-email",
    "send-pack",
    "shortlog",
    "show",
    "show-branch",
    "show-index",
    "show-ref",
    "sparse-checkout",
    "stage",
    "stash",
    "status",
    "stripspace",
    "submodule",
    "subtree",
    "svn",
    "switch",
    "symbolic-ref",
    "tag",
    "unpack-file",
    "unpack-objects",
    "update-index",
    "update-ref",
    "update-server-info",
    "upload-archive",
    "upload-pack",
    "var",
    "verify-commit",
    "verify-pack",
    "verify-tag",
    "version",
    "whatchanged",
    "worktree",
    "write-tree",
];

fn builtin(sub: &str) -> bool {
    BUILTIN.contains(&sub)
}

/// Whether `git <sub> <args>` writes to the repository or its work tree.
fn git_write(sub: &str, args: &[String]) -> bool {
    match sub {
        "commit" | "push" | "merge" | "rebase" | "reset" | "checkout" | "switch" | "restore"
        | "clean" | "add" | "rm" | "mv" | "cherry-pick" | "revert" | "am" => true,
        "stash" => !matches!(args.first().map(String::as_str), Some("list" | "show")),
        "branch" => args.iter().any(|a| {
            matches!(
                a.as_str(),
                "-d" | "-D" | "--delete" | "-m" | "-M" | "--move" | "-f" | "--force"
            )
        }),
        "worktree" => matches!(
            args.first().map(String::as_str),
            Some("remove" | "move" | "prune")
        ),
        _ => false,
    }
}

/// `path` with symlinks resolved when it exists, else with `.` and `..`
/// folded lexically, so a path and its canonical spelling compare equal.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
}

fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
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
    loop {
        match words.first().map(String::as_str) {
            // Reserved words and a negation in front of the command.
            Some("{" | "}" | "!" | "if" | "then" | "elif" | "else" | "do" | "while" | "until") => {
                words.remove(0);
            }
            Some(w) if w.len() > 1 && w.starts_with(['{', '!']) => {
                words[0].remove(0);
            }
            Some("coproc") => {
                words.remove(0);
                // `coproc NAME { …; }`: the name is not the command.
                if words.get(1).is_some_and(|w| w.starts_with('{')) {
                    words.remove(0);
                }
            }
            Some("time") => {
                words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    words.remove(0);
                }
            }
            Some(w) if words.len() > 1 && is_assignment(w) => {
                words.remove(0);
            }
            _ => break,
        }
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
    // Open `$(` substitutions: their parentheses belong to the word.
    let mut substitution = 0usize;
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
            // A backtick substitution stays in its word, like a quote.
            None if c == '"' || c == '\'' || c == '`' => {
                quote = Some(c);
                cur.push(c);
            }
            None if c == '(' && cur.ends_with('$') => {
                substitution += 1;
                cur.push(c);
            }
            None if c == ')' && substitution > 0 => {
                substitution -= 1;
                cur.push(c);
            }
            // A subshell's parentheses end the command before and inside.
            None if c == '\n' || c == ';' || c == '(' || c == ')' => {
                push_command(&mut out, &mut cur);
            }
            None if c == '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                push_command(&mut out, &mut cur);
            }
            // `2>&1`, `>&2` and `&>file` are redirections, not a background `&`.
            None if c == '&' && (cur.ends_with(['>', '<']) || chars.peek() == Some(&'>')) => {
                cur.push(c);
            }
            None if c == '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
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
            ..SessionPolicy::default()
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
        let dir = std::env::temp_dir().join(format!("vogt-policy-msg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
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

    fn workspace(baseline: &[&str], kind: Option<&str>, path: &Path) -> SessionPolicy {
        SessionPolicy {
            discard_baseline: baseline.iter().map(|s| s.to_string()).collect(),
            workspace_kind: kind.map(str::to_string),
            workspace_path: Some(path.display().to_string()),
            ..SessionPolicy::default()
        }
    }

    /// gitguard's `TestCheckCommand` table, minus its staging cases. `/r`
    /// does not exist, so every repo lookup fails and resolves to the
    /// session's own — the case a broken git must not weaken.
    #[test]
    fn discard_guard_ports_gitguards_table() {
        let root = Path::new("/r");
        let p = workspace(
            &["README.md", "src/a.py", "scratch/", "todo.txt"],
            Some("checkout"),
            root,
        );
        for command in [
            "git checkout -- AGENTS.md README.md docs/DISPATCH.md src/a.py && git status --short",
            "git checkout -- .",
            "git checkout -- src",
            "git checkout -- 'src/*.py'",
            "git checkout HEAD -- src/a.py",
            "git checkout README.md",
            "git checkout -f main",
            "git restore src/a.py",
            "git restore --worktree --staged src/a.py",
            "git restore .",
            "git reset --hard",
            "git reset --hard HEAD~1",
            "git clean -fd",
            "git clean -f scratch",
            "git clean -f scratch/notes.txt",
            "git stash",
            "git stash push src/a.py",
            "git stash -u",
            "git rm -f README.md",
            "git switch --discard-changes main",
            "git -C src checkout -- a.py",
            "cd src && git checkout -- b.py",
            "git checkout -- $FILES",
            "echo README.md | xargs git checkout --",
            "sh -c 'git checkout -- README.md'",
            "bash -lc \"cd /r && git reset --hard\"",
            "eval git checkout -- other.md",
            "sudo git checkout -- README.md",
            "git status; git checkout -- src/a.py",
            "GIT_DIR=/elsewhere/.git git checkout -- other.md",
        ] {
            let reason = check(&p, command, root).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("discard guard"), "{command}: {reason}");
            assert!(reason.contains("path(s)"), "{command}: {reason}");
            assert!(
                !reason.contains("a.py") && !reason.contains("README"),
                "{reason}"
            );
        }
        for command in [
            "git status --short",
            "git diff",
            "git log --oneline",
            "git checkout -- src/b.py",
            "git checkout -- docs/other.md",
            "git restore --staged src/a.py",
            "git checkout -b klaudia/goal-x",
            "git switch main",
            "git reset HEAD~1",
            "git reset --soft HEAD~1",
            "git clean -n",
            "git clean -f build",
            "git stash list",
            "git stash show -p",
            "git rm --cached -f README.md",
            "git revert HEAD",
            "rm -rf build",
            "go test ./...",
            "echo 'git checkout -- README.md'",
            "git checkout -- /elsewhere/README.md",
        ] {
            assert_eq!(check(&p, command, root), None, "{command}");
        }
    }

    /// The second review's bypasses (WI-1326): each is refused with a
    /// non-empty baseline, and its look-alike that discards nothing passes.
    #[test]
    fn discard_guard_reads_what_the_review_found() {
        let root = Path::new("/r");
        let p = workspace(&["README.md", "src/a.py"], Some("checkout"), root);
        for command in [
            // 1. abbreviated long options
            "git reset --ha",
            "git reset --hard=",
            "git checkout --forc main",
            "git switch --disc main",
            "git restore --st --wor src/a.py",
            "git rm --fo README.md",
            // 2. short-flag clusters
            "git checkout -qf main",
            "git switch -qf main",
            "git restore -SW src/a.py",
            "git clean -qf",
            // 3. inline aliases
            "git -c alias.x='checkout --' x README.md",
            "git -calias.y=reset y --hard",
            // 4. reserved words, subshells and a background `&`
            "(git reset --hard)",
            "{ git reset --hard; }",
            "if true; then git reset --hard; fi",
            "for f in a; do git checkout -- README.md; done",
            "true || ! git reset --hard",
            "time git reset --hard",
            "sleep 1 & git reset --hard",
            // 5. an expanded program
            "$(which git) checkout -- README.md",
            "`which git` checkout -- README.md",
            "$GIT reset --hard",
            // 6. an attached -s value is not -S
            "git restore -sSTABLE src/a.py",
            // 7. a literal absolute cd is followed, not given up on
            "cd /r/src && git checkout -- a.py",
        ] {
            let reason = check(&p, command, root).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("discard guard"), "{command}: {reason}");
        }
        for command in [
            "git reset --soft HEAD~1",
            "git checkout -q main",
            "git switch --detach main",
            "git restore --staged src/a.py",
            "git restore -S src/a.py",
            "git clean -qn",
            "git -c color.ui=never status",
            "(git status)",
            "if git diff --quiet; then echo clean; fi",
            "git status 2>&1 | head",
            "git log &>/dev/null & git status",
            "$(which git) status",
            "$GIT log",
            "cd /r/src && git checkout -- b.py",
            "cd /r && git restore docs/other.md",
        ] {
            assert_eq!(check(&p, command, root), None, "{command}");
        }
    }

    /// The third review's routes (WI-1326): config aliases from the
    /// environment or an include, aliases from the config file, commands
    /// inside substitutions and `coproc`, and per-subcommand valued flags.
    #[test]
    fn discard_guard_reads_the_remaining_routes() {
        let root = Path::new("/r");
        let p = workspace(&["README.md", "src/a.py"], Some("checkout"), root);
        for command in [
            // 1. config aliases from the environment or an included file
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.x GIT_CONFIG_VALUE_0='checkout --' git x README.md",
            "export GIT_CONFIG_COUNT=1; git x README.md",
            "env GIT_CONFIG_PARAMETERS=\"'alias.x=reset'\" git x --hard",
            "git -c include.path=/tmp/a x README.md",
            "git -c includeIf.onbranch:main.path=/tmp/a x README.md",
            // 2. an alias defined in the config earlier on the line
            "git config alias.x 'checkout --' && git x README.md",
            "git wipe",
            // 3. inside substitutions and coproc
            "echo $(git checkout -- README.md)",
            "echo \"$(git reset --hard)\"",
            "echo `git stash`",
            "x=$(echo $(git reset --hard))",
            "coproc git reset --hard",
            "coproc W { git reset --hard; }",
            // 4. checkout -m is --merge, not a message
            "git checkout -m README.md",
            "git checkout -qm src/a.py",
        ] {
            let reason = check(&p, command, root).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("discard guard"), "{command}: {reason}");
        }
        for command in [
            "GIT_CONFIG_NOSYSTEM=1 git status",
            "git -c core.pager=cat log",
            "git config --get user.name",
            "git log --oneline",
            "git lfs pull",
            "echo $(git rev-parse HEAD)",
            "echo `git status --short`",
            "echo '$(git reset --hard)'",
            "coproc git status",
            "git checkout -m main other.md",
            "git stash list -m x",
            // `$VAR` is git only when it is named for git
            "$EDITOR README.md",
            "${PAGER} src/a.py",
            // `switch -c` takes the branch name: `-cf` is not a force
            "git switch -cf main",
            "git switch -c fresh",
        ] {
            assert_eq!(check(&p, command, root), None, "{command}");
        }
        assert!(check(&p, "git switch -fc fresh", root).is_some());
        assert!(check(&p, "${GIT_BIN} reset --hard", root).is_some());
    }

    #[test]
    fn a_config_env_alias_counts_as_a_write_to_a_protected_checkout() {
        let (main, wt) = checkout_and_worktree("cfgenv");
        let mut p = workspace(&[], Some("worktree"), &wt);
        p.protected_checkouts = vec![main.display().to_string()];
        let command = format!(
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.l GIT_CONFIG_VALUE_0=commit git -C {} l -m x",
            main.display()
        );
        assert!(check(&p, &command, &wt).is_some());
        let inner = format!("echo $(git -C {} commit -m x)", main.display());
        assert!(check(&p, &inner, &wt).is_some());
        std::fs::remove_dir_all(&main).ok();
        std::fs::remove_dir_all(&wt).ok();
    }

    #[test]
    fn the_shared_parser_keeps_the_older_rules() {
        let p = policy(&["SECRET"], true, &[]);
        assert!(check(&p, "(gh pr merge 1)", &cwd()).is_some());
        assert!(check(&p, "sleep 1 & gh pr merge 1", &cwd()).is_some());
        assert!(check(&p, "if true; then git commit -m SECRET; fi", &cwd()).is_some());
        assert!(check(&p, "git log 2>&1 | grep SECRET", &cwd()).is_none());
        assert!(check(&p, "echo \"(a & b)\"", &cwd()).is_none());
    }

    #[test]
    fn an_inline_alias_counts_as_a_write_to_a_protected_checkout() {
        let (main, wt) = checkout_and_worktree("alias");
        let mut p = workspace(&[], Some("worktree"), &wt);
        p.protected_checkouts = vec![main.display().to_string()];
        let command = format!("git -C {} -c alias.l=commit l -m x", main.display());
        assert!(check(&p, &command, &wt).is_some());
        let read = format!("git -C {} -c color.ui=never log", main.display());
        assert_eq!(check(&p, &read, &wt), None);
        std::fs::remove_dir_all(&main).ok();
        std::fs::remove_dir_all(&wt).ok();
    }

    #[test]
    fn a_fresh_worktree_allows_every_discard() {
        let root = Path::new("/r");
        let p = workspace(&[], Some("worktree"), root);
        for command in [
            "git checkout -- .",
            "git reset --hard",
            "git clean -fdx",
            "git stash -u",
            "git restore src/a.py",
            "echo x | xargs git checkout --",
        ] {
            assert_eq!(check(&p, command, root), None, "{command}");
        }
    }

    #[test]
    fn a_checkout_session_keeps_the_roots_dirty_files() {
        let root = repo("dirty-root");
        let p = workspace(&["engine/server/src/budget.rs"], Some("checkout"), &root);
        let reason = check(&p, "git restore engine/server/src/budget.rs", &root).unwrap();
        assert!(
            reason.contains("discard guard") && reason.contains("1 path(s)"),
            "{reason}"
        );
        std::fs::create_dir_all(root.join("engine")).unwrap();
        assert!(check(&p, "git restore server/src/budget.rs", &root.join("engine")).is_some());
        assert_eq!(check(&p, "git restore some_other_file", &root), None);
        // A repo the session does not work in is judged by its own baseline,
        // which this session does not hold.
        let other = repo("dirty-other");
        let command = format!(
            "git -C {} restore engine/server/src/budget.rs",
            other.display()
        );
        assert_eq!(check(&p, &command, &root), None);
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&other).ok();
    }

    /// A project checkout and a linked worktree cut from it.
    fn checkout_and_worktree(name: &str) -> (PathBuf, PathBuf) {
        let main = repo(name);
        let wt = main.with_file_name(format!(
            "{}-wt",
            main.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&wt);
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&main)
            .args(["worktree", "add", "-q", "-b", "wi-x"])
            .arg(&wt)
            .status()
            .unwrap();
        assert!(status.success());
        (main, wt)
    }

    #[test]
    fn a_worktree_session_may_not_write_to_the_project_checkout() {
        let (main, wt) = checkout_and_worktree("protected");
        let mut p = workspace(&[], Some("worktree"), &wt);
        p.protected_checkouts = vec![main.display().to_string()];
        let c = main.display();
        for command in [
            format!("git -C {c} commit --allow-empty -m x"),
            format!("git -C {c} push origin wi-x"),
            format!("git -C {c} reset --hard"),
            format!("git -C {c} branch -D old"),
            format!("git -C {c} worktree remove ../x"),
            format!("git -C {c} add ."),
            format!("git -C {c} stash"),
            format!("cd {c} && git commit -m x"),
            format!("bash -c 'git -C {c} rebase main'"),
        ] {
            let reason = check(&p, &command, &wt).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("protected checkout"), "{command}: {reason}");
        }
        // Run from inside the checkout too.
        assert!(check(&p, "git commit -m x", &main).is_some());
        for command in [
            format!("git -C {c} log"),
            format!("git -C {c} status"),
            format!("git -C {c} diff"),
            format!("git -C {c} stash list"),
            format!("git -C {c} branch -a"),
            format!("git -C {c} worktree list"),
            "git status".to_string(),
            "git commit --allow-empty -m x".to_string(),
            "git push origin wi-x".to_string(),
        ] {
            assert_eq!(check(&p, &command, &wt), None, "{command}");
        }
        // A broken lookup is not protected.
        assert_eq!(check(&p, "git -C /nonexistent commit -m x", &wt), None);
        std::fs::remove_dir_all(&wt).ok();
        std::fs::remove_dir_all(&main).ok();
    }

    #[test]
    fn a_checkout_session_may_write_to_the_project_checkout() {
        let (main, wt) = checkout_and_worktree("oversight");
        let mut p = workspace(&[], Some("checkout"), &main);
        p.protected_checkouts = vec![main.display().to_string()];
        let command = format!("git -C {} commit --allow-empty -m x", main.display());
        assert_eq!(check(&p, &command, &wt), None);
        assert_eq!(check(&p, &command, &main), None);
        // No workspace at all: only the existing rules apply.
        let none = SessionPolicy {
            protected_checkouts: vec![main.display().to_string()],
            ..SessionPolicy::default()
        };
        assert_eq!(check(&none, &command, &wt), None);
        assert_eq!(check(&none, "git reset --hard", &main), None);
        std::fs::remove_dir_all(&wt).ok();
        std::fs::remove_dir_all(&main).ok();
    }

    #[test]
    fn the_policy_is_filled_from_the_workspace_block() {
        let root = repo("capture");
        std::fs::write(root.join("f"), "changed").unwrap();
        std::fs::create_dir_all(root.join("scratch")).unwrap();
        std::fs::write(root.join("scratch/n"), "x").unwrap();
        std::fs::write(root.join("new.txt"), "x").unwrap();
        let ws = crate::git::capture_workspace(&root, None).unwrap();
        let p = with_workspace(None, Some(&ws)).unwrap();
        assert_eq!(p.workspace_kind.as_deref(), Some("checkout"));
        assert_eq!(p.workspace_path, Some(ws.path.clone()));
        assert_eq!(p.discard_baseline, ["f", "new.txt", "scratch/"]);
        // The rules judge it: the baseline file is refused, another allowed.
        assert!(check(&p, "git restore f", &root).is_some_and(|r| r.contains("discard")));
        assert_eq!(check(&p, "git restore other", &root), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_workspace_block_leaves_the_policy_as_it_is() {
        // A record from before WI-1324, or a start outside a repository: the
        // engine adds nothing, and the agent's own guard keeps judging.
        assert_eq!(with_workspace(None, None), None);
        let bare = SessionPolicy::default();
        assert_eq!(with_workspace(Some(bare.clone()), None), Some(bare));
    }

    #[test]
    fn a_linked_worktree_keeps_the_dirt_it_started_with() {
        let (main, wt) = checkout_and_worktree("linked");
        std::fs::write(wt.join("f"), "edited before the session").unwrap();
        let ws = crate::git::capture_workspace(&wt, None).unwrap();
        // The session then edits another file of its own.
        std::fs::write(wt.join("g"), "the session's own").unwrap();
        let p = with_workspace(None, Some(&ws)).unwrap();
        assert_eq!(p.workspace_kind.as_deref(), Some("worktree"));
        assert_eq!(p.discard_baseline, ["f"]);
        assert!(check(&p, "git checkout -- f", &wt).is_some());
        assert_eq!(check(&p, "git checkout -- g", &wt), None);
        std::fs::remove_dir_all(&wt).ok();
        std::fs::remove_dir_all(&main).ok();
    }
}
