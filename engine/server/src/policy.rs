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
//! that owns the hook, so it can edit the hook or its own settings, run a
//! script file, or write files with a non-shell tool; and `gh api` is not
//! read.
//!
//! The check looks at the command as a shell would run it. A line joined by
//! `&&`, `||`, `;`, `&`, `|` or `|&`, or split over newlines, is several
//! commands, and so are the bodies of `( … )` subshells, `{ …; }` groups,
//! `if`/`for`/`while` bodies and `$(…)`, backtick and `<(…)` substitutions;
//! any one of them refusing refuses the whole line. Comments and quoted
//! heredoc bodies are not commands. Assignments, reserved words (`!`,
//! `time`, `then`, `do`, …) and the `env` (with `-u NAME`), `sudo`,
//! `command`, `exec`, `nohup`, `nice` and `timeout` wrappers in front of a
//! command are stepped over, and so are git's global options,
//! so `sudo git commit` and `git -C ../other commit` are judged as the commit
//! they are. The program is matched by its basename, so `/usr/bin/git` is
//! `git`. The forbidden-text rule reads the whole line only when the line
//! contains a write verb (`git commit|push|merge|tag`, `gh pr
//! create|edit|merge`), which a `bash -c "git commit -m …"` still contains
//! as text; a read such as `git log --grep` is never refused by it.
//!
//! Two rules read the session's workspace (WI-1326). The discard guard, after
//! msp-klaudia's gitguard, refuses a git command or `rm` that would throw
//! away a path in `discard_baseline` — the paths in the session's own tree
//! that were already dirty when it started; `reset --hard`, `clean`, `stash`
//! and a command whose paths cannot be read (a glob, an expansion, `xargs`,
//! `eval`, a `cd` earlier on the line) count as reaching every one. An `rm`
//! is judged by each path it deletes, canonicalised, not by the directory it
//! was typed in. A fresh worktree has an empty baseline, so the guard is a
//! no-op there. The protected-checkout rule holds only a `worktree` session:
//! it refuses a git write whose repo is a registered project checkout other
//! than the session's own worktree, and a write whose repo the line hides
//! (`GIT_DIR`, `--git-dir`, an unfollowed `cd`). Repos are resolved by `git
//! rev-parse --show-toplevel`; a lookup that fails is the session's own repo
//! for the first rule and not protected for the second.
//!
//! What this reading cannot parse with certainty fails closed rather than
//! open: an alias or function definition, a `case`, `env -C`/`-S`, an
//! unbalanced quote or paren, or nesting past its bounds. When such a part
//! names git, rm or another deleting verb, the discard guard counts it as
//! reaching every baseline path, and when the line names a git write the
//! protected-checkout rule refuses it. A program that is a variable, a
//! command substitution or a brace expansion, a shell fed its script on
//! stdin or `<(…)`, and a git subcommand that may be an alias reach every
//! baseline path whatever the line names (WI-4001); only `eval "$(ssh-agent
//! -s)"` is read as safe. With an empty baseline neither rule applies, so a
//! fresh clone is never refused for its shape.

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
    if worktree && !policy.protected_checkouts.is_empty() {
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
        // `-c key=value` and `--config-env name=var` take a value, which
        // is not the subcommand: `git -c x=y -C <dir> commit` runs in dir.
        if arg == "-c" || arg == "--config-env" {
            args.next();
            continue;
        }
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
/// captures that block before the child runs, and a wake passes the record's
/// copy, so the session's own edits never become its baseline. With no
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

/// How deep `bash -c`, `eval`, `xargs` and `$(…)` payloads are followed.
/// Past it the line counts as one this check cannot read.
const MAX_DEPTH: usize = 4;

/// One git invocation on the line, with the directory it runs in as far as
/// it can be read. A plain `rm` (or `unlink`) is one too, with `rm` set and
/// `words` holding only the paths it deletes.
#[derive(Debug)]
struct GitCall {
    words: Vec<String>,
    /// The session cwd, or the target of a literal `cd` earlier on the line
    /// (inside a `( … )` subshell, only until it closes).
    cwd: PathBuf,
    /// The paths the command names cannot be read: a `cd`, `eval` or `xargs`
    /// before it, an expansion in it, or git pointed elsewhere by
    /// `--git-dir`, `--work-tree`, `GIT_DIR` or `core.worktree`. The discard
    /// guard then counts every protected path as reached.
    unreadable: bool,
    /// The repo git runs against cannot be read from the directory: the line
    /// sets `GIT_DIR`, `GIT_WORK_TREE`, `--git-dir`, `--work-tree` or
    /// `core.worktree`, or `-C` names an expansion. The protected-checkout
    /// rule refuses a write it cannot place.
    redirected: bool,
    /// A plain `rm`/`unlink`: `words` are the paths, each resolved on its own,
    /// and `redirected` means a `cd` ran first (relative paths unreadable).
    rm: bool,
    /// The subcommand may be an alias this reading cannot see (WI-4001
    /// bypass 4): `-c alias.<sub>=…`, `--config-env=alias.<sub>=…`, an
    /// `include.path`, `GIT_CONFIG_*` on the line, or a `git config` that
    /// sets an alias. Its verb is then unknown: it may discard or write.
    aliased: bool,
}

/// What the line does, as far as this reading can tell.
#[derive(Debug, Default)]
struct Calls {
    calls: Vec<GitCall>,
    /// Text this reading could not parse with certainty: an unbalanced quote
    /// or paren, a `case`, a function definition, an `alias`, a variable as
    /// the program, a shell reading its script from stdin, `env -C`/`-S`, or
    /// nesting past [`MAX_DEPTH`]. A git or rm word in it may be a call the
    /// walker never saw.
    opaque: Vec<String>,
    /// A shell reads its script from somewhere other than the line: stdin
    /// (`… | bash`, `bash <<< …`, `bash /dev/stdin`) or a process
    /// substitution (`bash <(…)`, `source <(…)`). What it runs is not on
    /// the line as words.
    blind: bool,
    /// A program is a variable or holds one (`$R a.txt`, `${R}m`, `eval
    /// "$CMD"`, `bash -c "$CMD"`): what it runs is decided when the line
    /// runs, from text that can be spelled any way at all.
    dynamic: bool,
    /// The whole line, for the verbs an opaque part may act on later in it
    /// (`alias g='git -C x'; g commit`).
    line: String,
}

impl Calls {
    /// The discard guard counts the line as reaching every baseline path
    /// when a program is a variable or a command substitution (whatever it
    /// holds; only `eval "$(ssh-agent -s)"` is read as safe); when a shell
    /// runs a script that is not on the line (stdin, a heredoc, `<(…)`),
    /// whatever else the line names, since `echo … | base64 -d | bash`
    /// spells its script in no word (WI-4001 bypass 2); or when an opaque
    /// part names git, rm or a deleting verb.
    fn blind_discard(&self) -> bool {
        self.dynamic || self.blind || self.opaque.iter().any(|text| mentions(text, DISCARD_WORDS))
    }

    /// An opaque part names git or a git write verb (or a shell reads a
    /// piped script) on a line that names both: the protected-checkout rule cannot
    /// place it, so it refuses.
    fn blind_write(&self) -> bool {
        (self.blind
            || self
                .opaque
                .iter()
                .any(|text| mentions(text, &["git"]) || mentions(text, GIT_WRITE_WORDS)))
            && mentions(&self.line, &["git"])
            && mentions(&self.line, GIT_WRITE_WORDS)
    }
}

/// Words that make an opaque part a possible discard: the programs that
/// delete or overwrite, and git's discarding verbs (for `$GIT checkout …`).
const DISCARD_WORDS: &[&str] = &[
    "git", "rm", "mv", "unlink", "shred", "truncate", "checkout", "restore", "reset", "clean",
    "stash", "switch",
];

/// Git subcommands [`git_write`] counts as writes, as words.
const GIT_WRITE_WORDS: &[&str] = &[
    "commit",
    "push",
    "merge",
    "rebase",
    "reset",
    "checkout",
    "switch",
    "restore",
    "clean",
    "stash",
    "add",
    "rm",
    "mv",
    "cherry-pick",
    "revert",
    "am",
    "branch",
    "worktree",
];

/// Whether `text`, lowercased and its quotes and backslashes removed (so
/// `g""it` and `GIT` are `git`), contains one of `words` as a whole word.
fn mentions(text: &str, words: &[&str]) -> bool {
    let joined = text.replace("\\\n", "");
    // As typed, with `$'…'` decoded, and with every backslash escape decoded
    // (what `printf '\x72m'` or `echo -e` prints).
    let unescaped = unescape(&mut joined.chars().peekable(), false);
    [ansi_c_decoded(&joined), unescaped].iter().any(|spelling| {
        spelling
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '\\'))
            .collect::<String>()
            .to_ascii_lowercase()
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .any(|w| words.contains(&w))
    })
}

/// Every git invocation and `rm` in `command`, following subshells, groups,
/// compound-command bodies, `$(…)`/backtick/process substitutions, `sh -c`
/// payloads, `eval`, `xargs` and `cd` the conservative way gitguard does:
/// whatever moves the shell or hides the paths makes the paths after it
/// unreadable, and whatever cannot be parsed is recorded as opaque.
fn git_calls(command: &str, cwd: &Path) -> Calls {
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
    let redirected = [
        "GIT_DIR=",
        "GIT_WORK_TREE=",
        "GIT_COMMON_DIR=",
        "export GIT_",
    ]
    .iter()
    .any(|v| command.contains(v))
        || command.to_ascii_lowercase().contains("core.worktree");
    let configured = [
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG=",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
    ]
    .iter()
    .any(|v| command.contains(v))
        || command_env(command);
    let mut walker = Walker {
        moved: redirected,
        configured,
        out: Calls {
            line: command.to_string(),
            ..Calls::default()
        },
    };
    walker.walk(command, cwd, moved, 0);
    walker.out
}

struct Walker {
    /// The line points git elsewhere (see [`GitCall::redirected`]).
    moved: bool,
    /// The line hands git configuration through the environment
    /// (`GIT_CONFIG_COUNT`/`_KEY_n`, `GIT_CONFIG_PARAMETERS`, `GIT_CONFIG`,
    /// `GIT_CONFIG_GLOBAL`/`_SYSTEM`): any subcommand may be an alias.
    configured: bool,
    out: Calls,
}

impl Walker {
    fn walk(&mut self, command: &str, cwd: &Path, unreadable: bool, depth: usize) {
        if depth > MAX_DEPTH {
            self.out.opaque.push(command.to_string());
            return;
        }
        let (pieces, opaque) = lex(command);
        if opaque {
            self.out.opaque.push(command.to_string());
        }
        self.pieces(&pieces, cwd, unreadable, depth);
    }

    fn pieces(&mut self, pieces: &[Piece], cwd: &Path, unreadable: bool, depth: usize) {
        let state = State {
            dir: cwd.to_path_buf(),
            unreadable,
            cd: false,
            lost: false,
        };
        self.pieces_from(pieces, state, depth);
    }

    fn pieces_from(&mut self, pieces: &[Piece], mut state: State, depth: usize) {
        if depth > MAX_DEPTH {
            self.out.opaque.push(flatten(pieces).join("\n"));
            return;
        }
        // A `( … )` subshell's cd does not outlive it.
        let mut saved: Vec<State> = Vec::new();
        // The substitutions in the simple command being read: the lexer
        // emits them just before the command that holds them.
        let mut subs: Vec<&[Piece]> = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Open => {
                    subs.clear();
                    saved.push(state.clone());
                }
                Piece::Close => {
                    subs.clear();
                    if let Some(outer) = saved.pop() {
                        state = outer;
                    }
                }
                // A substitution runs in a subshell from the same directory.
                Piece::Sub(inner) => {
                    subs.push(inner);
                    self.pieces_from(inner, state.clone(), depth + 1);
                }
                Piece::Cmd(simple) => {
                    self.simple(simple, &mut state, depth, &subs);
                    subs.clear();
                }
            }
        }
    }

    fn simple(&mut self, simple: &str, state: &mut State, depth: usize, subs: &[&[Piece]]) {
        let (words, wrapped_opaque) = command_words_ext(simple);
        if wrapped_opaque {
            self.out.opaque.push(simple.to_string());
        }
        let Some(first) = words.first() else {
            return;
        };
        if words.len() == 1 && is_assignment(first) {
            // `X=$(…)` runs nothing itself; its substitution is walked as a
            // piece of its own.
            return;
        }
        let home = home_program(first);
        let first = home.as_ref().unwrap_or(first);
        if brace_expands(first) {
            // `{rm,a.txt}`, `{git,restore,a.txt}`, `{r,}m a.txt`: brace
            // expansion spells the program (and its arguments) out of
            // pieces this reading does not join (WI-4001 bypass 3).
            self.out.opaque.push(simple.to_string());
            self.out.dynamic = true;
            return;
        }
        if first.contains(['$', '`']) {
            // The program is an expansion (`$GIT checkout …`, `${R}m`, the
            // `$CMD` an `eval`/`sh -c` payload holds, or a command
            // substitution, which the lexer leaves as a lone `$`): what it
            // runs is not on the line. `$(printf %s%s r m) a.txt` prints its
            // program out of pieces no word names (WI-4001 bypass 1).
            self.out.opaque.push(simple.to_string());
            self.out.dynamic = true;
            return;
        }
        match base(first) {
            "alias" | "function" | "case" | "coproc" => {
                self.out.opaque.push(simple.to_string());
            }
            "cd" | "pushd" | "popd" => {
                // Whether the cd ran, and where CDPATH sent it, is not
                // something this reading knows: the paths after it are
                // unreadable. A literal directory is followed for the
                // protected-checkout rule, which only needs the repo; any
                // other target loses the directory altogether.
                state.cd = true;
                let target = (base(first) == "cd" && words.len() == 2)
                    .then(|| expand_home(&words[1]))
                    .flatten()
                    .filter(|t| !t.starts_with('-') && !t.contains(['$', '`']));
                match target {
                    Some(target) => state.dir = resolve_dir(&target, &state.dir),
                    None => state.lost = true,
                }
            }
            "bash" | "sh" | "zsh" | "dash" | "ksh" => match shell_payload(&words) {
                Payload::Script(payload) => {
                    let (pieces, opaque) = lex(payload);
                    if opaque {
                        self.out.opaque.push(payload.to_string());
                    }
                    // A new shell: its cd does not move this one.
                    self.pieces_from(&pieces, state.clone(), depth + 1);
                }
                // `bash file.sh`: the script is not on the line, and this
                // check reads lines, not files.
                Payload::File => {}
                // `… | bash`, `bash <(…)`, `bash /dev/stdin <<< …`: the
                // script arrives from the line, out of sight.
                Payload::Stdin => self.out.blind = true,
            },
            // `source <(…)`, `. /dev/stdin`: this shell runs a script the
            // line feeds it. A plain file is not read, as for `bash f.sh`.
            "source" | "." => {
                if words[1..].first().is_some_and(|w| reads_stdin(w))
                    || words[1..].iter().any(|w| stdin_redirect(w))
                {
                    self.out.blind = true;
                }
            }
            "eval" => {
                let payload = words[1..].join(" ");
                // `eval "$(ssh-agent -s)"`: the one substitution whose output
                // is known to be variable assignments, spelled literally.
                // Anything else a substitution feeds eval is unreadable.
                if payload == "$" && subs.len() == 1 && ssh_agent(subs[0]) {
                    state.unreadable = true;
                    return;
                }
                let (pieces, opaque) = lex(&payload);
                if opaque {
                    self.out.opaque.push(payload.clone());
                }
                let mut inner = state.clone();
                inner.unreadable = true;
                self.pieces_from(&pieces, inner, depth + 1);
                // eval runs in this shell: a cd inside it moves the rest. A
                // payload that names no cd leaves the directory as it was,
                // so `eval "$(ssh-agent -s)"; git push` stays readable.
                state.unreadable = true;
                if mentions(&payload, &["cd", "pushd", "popd"]) {
                    state.lost = true;
                }
            }
            "xargs" => {
                // The paths arrive on stdin.
                let payload = xargs_command(&words).join(" ");
                let (pieces, opaque) = lex(&payload);
                if opaque {
                    self.out.opaque.push(payload.clone());
                }
                let mut inner = state.clone();
                inner.unreadable = true;
                self.pieces_from(&pieces, inner, depth + 1);
            }
            // `su [user] -c '…'`, `runuser -c '…'`, `script -c '…' log`: a
            // new shell runs the payload, read like `sh -c`.
            "su" | "runuser" | "script" => {
                if let Some(payload) = dash_c_payload(&words) {
                    let (pieces, opaque) = lex(payload);
                    if opaque {
                        self.out.opaque.push(payload.to_string());
                    }
                    self.pieces_from(&pieces, state.clone(), depth + 1);
                }
            }
            // `trap 'rm a.txt' EXIT`: the action runs later in this shell,
            // after whatever `cd` the rest of the line makes (WI-4001
            // bypass 6). It is read like a `sh -c` payload whose relative
            // paths and repo cannot be placed; an absolute path still can.
            "trap" => {
                if let Some(payload) = trap_action(&words) {
                    let (pieces, opaque) = lex(payload);
                    if opaque {
                        self.out.opaque.push(payload.to_string());
                    }
                    let mut inner = state.clone();
                    inner.cd = true;
                    inner.lost = true;
                    self.pieces_from(&pieces, inner, depth + 1);
                }
            }
            "git" => {
                let elsewhere = words
                    .iter()
                    .any(|w| w.starts_with("--git-dir") || w.starts_with("--work-tree"));
                let hidden_dir = git_dir_words(&words)
                    .iter()
                    .any(|d| d.contains(['$', '`']) || expand_home(d).is_none());
                let hidden = simple.contains(['$', '`']) || elsewhere;
                let aliased = self.configured || git_aliased(&words) || git_runs_command(&words);
                self.out.calls.push(GitCall {
                    words,
                    cwd: state.dir.clone(),
                    unreadable: state.unreadable || state.cd || hidden,
                    redirected: self.moved || state.lost || elsewhere || hidden_dir,
                    rm: false,
                    aliased,
                });
            }
            // A plain `rm` deletes the same files `git rm` does (WI-3985's
            // sibling case). Its `-f`/`-r` flags are not relevant: the delete
            // itself is the discard. Each path is resolved on its own, so the
            // directory it was typed from does not decide whose file it is.
            "rm" | "unlink" => {
                let mut paths = Vec::new();
                let mut operands = false;
                for w in &words[1..] {
                    if operands || w == "-" || !w.starts_with('-') {
                        paths.push(w.clone());
                    } else if w == "--" {
                        operands = true;
                    }
                }
                self.out.calls.push(GitCall {
                    words: paths,
                    cwd: state.dir.clone(),
                    // Each path's own readability is judged in `rm_reach`;
                    // this is what holds for all of them.
                    unreadable: state.unreadable || simple.contains('`'),
                    redirected: state.cd,
                    rm: true,
                    aliased: false,
                });
            }
            _ => {}
        }
    }
}

/// Where the walker stands on the line.
#[derive(Clone)]
struct State {
    dir: PathBuf,
    /// Every path named after this point is unreadable: an `eval` or
    /// `xargs` supplies or re-reads them.
    unreadable: bool,
    /// A `cd` ran earlier: a relative path no longer resolves from the
    /// session cwd. An absolute path still reads as itself.
    cd: bool,
    /// A `cd` to a target this reading cannot follow: the directory is no
    /// longer known, so the repo cannot be read from it.
    lost: bool,
}

/// What a shell invocation runs.
enum Payload<'a> {
    /// The `-c` script.
    Script(&'a str),
    /// A script file operand.
    File,
    /// No operand and no `-c`: the script is read from stdin.
    Stdin,
}

/// The script a `sh -c` runs: the first operand when an option cluster
/// contains `c` (`-c`, `-lc`, `-ec`, `-c --`). `-o`/`-O` and the long options
/// that take a value consume it.
fn shell_payload(words: &[String]) -> Payload<'_> {
    let mut args = words.iter().skip(1);
    let mut command = false;
    let mut from_file = false;
    let mut piped = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        // A redirection: `bash < script.sh` reads a file, like `bash
        // script.sh`; `2>&1` and `>out` change nothing about the script.
        // `bash < <(…)` and `bash </dev/stdin` read it from the line.
        let redirect = arg.trim_start_matches(|c: char| c.is_ascii_digit());
        if redirect.starts_with(['<', '>']) {
            let mut target = redirect.trim_start_matches(['<', '>', '&']).to_string();
            if matches!(redirect, "<" | ">" | ">>" | "<<<" | "&>" | ">&") {
                target = args.next().cloned().unwrap_or_default();
            }
            if redirect.starts_with('<') && !redirect.starts_with("<<") {
                if reads_stdin(&target) {
                    piped = true;
                } else {
                    from_file = true;
                }
            }
            continue;
        }
        if matches!(
            arg.as_str(),
            "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file"
        ) {
            args.next();
            continue;
        }
        if arg.starts_with("--") {
            continue;
        }
        if arg.starts_with(['-', '+']) && arg.len() > 1 {
            if arg.starts_with('-') && arg.contains('c') {
                command = true;
            }
            continue;
        }
        return if command {
            Payload::Script(arg)
        } else if reads_stdin(arg) || piped {
            Payload::Stdin
        } else {
            Payload::File
        };
    }
    match args.next() {
        Some(arg) if command => Payload::Script(arg),
        Some(arg) if reads_stdin(arg) || piped => Payload::Stdin,
        Some(_) => Payload::File,
        None if piped => Payload::Stdin,
        None if from_file => Payload::File,
        None => Payload::Stdin,
    }
}

/// Whether a git call's subcommand may be an alias (see
/// [`GitCall::aliased`]): a `-c`/`--config-env` that defines an alias of the
/// very subcommand it runs (git ignores an alias that shadows a builtin, so
/// `-c alias.st=status status` is a plain `status`) or sets `include.path`
/// / `includeIf.*`, or a `git config` that sets an alias or include.
fn git_aliased(words: &[String]) -> bool {
    let args = git_args(words);
    let sub = args.first().map(|s| s.to_ascii_lowercase());
    let mut rest = words.iter().skip(1);
    while let Some(arg) = rest.next() {
        let setting = match arg.as_str() {
            "-c" | "--config-env" => rest.next().map(String::as_str),
            _ if arg.starts_with("--config-env=") => arg.strip_prefix("--config-env="),
            _ if arg.starts_with("-c") && arg.len() > 2 && !arg.starts_with("-C") => {
                arg.strip_prefix("-c")
            }
            "-C" => {
                rest.next();
                None
            }
            _ if arg.starts_with('-') => None,
            _ => break,
        };
        let Some(setting) = setting else { continue };
        let key = setting
            .split_once('=')
            .map_or(setting, |(k, _)| k)
            .to_ascii_lowercase();
        if key.starts_with("include.") || key.starts_with("includeif.") {
            return true;
        }
        if let Some(name) = key.strip_prefix("alias.") {
            if sub.as_deref() == Some(name) {
                return true;
            }
        }
    }
    if sub.as_deref() == Some("config") {
        let reads = args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--get"
                    | "--get-all"
                    | "--get-regexp"
                    | "--get-urlmatch"
                    | "--list"
                    | "-l"
                    | "get"
                    | "list"
            )
        });
        let names = args[1..].iter().any(|a| {
            let a = a.to_ascii_lowercase();
            a.starts_with("alias.") || a.starts_with("include.") || a.starts_with("includeif.")
        });
        return names && !reads;
    }
    false
}

/// Environment variables through which git (or the pager it starts) runs
/// a command of the caller's choosing (WI-4001 round 4, blocker 4).
const COMMAND_ENV: &[&str] = &[
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "EDITOR",
    "VISUAL",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "LESSOPEN",
    "LESSCLOSE",
];

/// Whether the line hands git a command through the environment: a
/// [`COMMAND_ENV`] name assigned anything but a known-harmless program
/// (`GIT_PAGER=cat`, `GIT_EDITOR=true`), or handed to a declaration
/// builtin (`export NAME`, `printf -v NAME …`, `read NAME`), since its value
/// then comes from somewhere this reading does not follow.
fn command_env(command: &str) -> bool {
    let bytes = command.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for name in COMMAND_ENV {
        let mut from = 0;
        while let Some(at) = command[from..].find(name) {
            let start = from + at;
            let end = start + name.len();
            from = end;
            if (start > 0 && ident(bytes[start - 1])) || bytes.get(end).is_some_and(|b| ident(*b)) {
                continue;
            }
            // `$NAME` / `${NAME}` reads it; `unset NAME` takes it away.
            let before = command[..start].trim_end_matches('{');
            if before.ends_with('$') {
                continue;
            }
            if command[..start].trim_end().ends_with("unset") {
                continue;
            }
            if bytes.get(end) != Some(&b'=') {
                // A bare mention (a commit message naming GIT_PAGER) sets
                // nothing; a declaration builtin may give it a value from
                // somewhere else (`export NAME`, `read NAME`, `printf -v`).
                let word = command[..start]
                    .trim_end()
                    .rsplit(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '('))
                    .next()
                    .unwrap_or("");
                if matches!(
                    word,
                    "export" | "declare" | "typeset" | "readonly" | "local" | "read" | "-v"
                ) || word.starts_with('-')
                {
                    return true;
                }
                continue;
            }
            if !harmless_command(&assigned_value(&command[end + 1..]), name) {
                return true;
            }
        }
    }
    false
}

/// The value an assignment's text starts with, its quotes removed: up to
/// the first unquoted blank or command separator.
fn assigned_value(text: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => out.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() || matches!(c, ';' | '&' | '|' | ')') => break,
            None => out.push(c),
        }
    }
    out
}

/// Whether `value`, given to the setting `key` that names a command, runs
/// nothing but a known-harmless program: empty, `cat`, `less`/`more` with
/// flags, `true`, `false`, `:`, or for an ssh setting `ssh` with flags. No
/// expansion, separator or redirection. A hooks path is harmless only as
/// `/dev/null`; the paths git searches for programs never are.
fn harmless_command(value: &str, key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if key.ends_with("hookspath") {
        return value.is_empty() || value == "/dev/null";
    }
    if matches!(
        key.as_str(),
        "git_exec_path" | "git_template_dir" | "lessopen" | "lessclose"
    ) {
        return value.is_empty();
    }
    if key.contains("credential") && key.ends_with("helper") {
        return matches!(value, "" | "store" | "cache");
    }
    if value.contains(['$', '`', ';', '&', '|', '<', '>', '(', ')', '\n', '\\', '!']) {
        return false;
    }
    let words = shell_words(value);
    let Some(program) = words.first() else {
        return true;
    };
    let ssh = key.contains("ssh");
    let safe = match program.as_str() {
        "cat" | "less" | "more" | "true" | "false" | ":" => true,
        "ssh" | "/usr/bin/ssh" => ssh,
        _ => false,
    };
    safe && words[1..].iter().all(|w| w.starts_with('-') || ssh)
}

/// Whether a git config key names a command git runs (WI-4001 round 4,
/// blocker 4): `diff.external`, `core.fsmonitor`, the pager and editor
/// settings, ssh and askpass commands, credential helpers, the hooks
/// path, filter and merge drivers, textconv and external diff drivers,
/// difftool/mergetool commands, `gpg.program`, proxies and remote
/// upload/receive programs.
fn command_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let last = key.rsplit('.').next().unwrap_or("");
    matches!(
        key.as_str(),
        "diff.external"
            | "core.fsmonitor"
            | "core.pager"
            | "core.editor"
            | "sequence.editor"
            | "core.sshcommand"
            | "core.askpass"
            | "core.hookspath"
            | "core.gitproxy"
            | "gpg.program"
            | "interactive.difffilter"
            | "uploadpack.packobjectshook"
            | "credential.helper"
    ) || key.starts_with("pager.")
        || (key.starts_with("filter.") && matches!(last, "smudge" | "clean" | "process"))
        || (key.starts_with("diff.") && matches!(last, "textconv" | "command"))
        || (key.starts_with("merge.") && last == "driver")
        || ((key.starts_with("difftool.") || key.starts_with("mergetool.")) && last == "cmd")
        || (key.starts_with("credential.") && last == "helper")
        || (key.starts_with("gpg.") && last == "program")
        || (key.starts_with("remote.") && matches!(last, "uploadpack" | "receivepack" | "proxy"))
        || key.starts_with("ssh.")
}

/// Whether a git call runs a command of the caller's choosing (WI-4001
/// round 4, blocker 4), which may discard or write anything, like an alias
/// does: a `-c`/`--config-env` that sets a [`command_key`] to anything but
/// a harmless program, `--exec-path=<dir>`, a `git config` that sets one,
/// `grep -O`/`--open-files-in-pager`, `difftool -x`/`--extcmd`, `rebase
/// -x`/`--exec`, `bisect run` and `submodule foreach`.
fn git_runs_command(words: &[String]) -> bool {
    let mut rest = words.iter().skip(1);
    while let Some(arg) = rest.next() {
        let setting = match arg.as_str() {
            "-c" => rest.next().map(String::as_str),
            // The value comes from an environment variable this reading
            // does not follow.
            "--config-env" => {
                if rest
                    .next()
                    .is_some_and(|s| command_key(s.split_once('=').map_or(s, |p| p.0)))
                {
                    return true;
                }
                None
            }
            _ if arg.starts_with("--config-env=") => {
                let s = &arg["--config-env=".len()..];
                if command_key(s.split_once('=').map_or(s, |p| p.0)) {
                    return true;
                }
                None
            }
            _ if arg.starts_with("--exec-path=") => return true,
            _ if arg.starts_with("-c") && arg.len() > 2 => arg.strip_prefix("-c"),
            "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                rest.next();
                None
            }
            _ if arg.starts_with('-') => None,
            _ => break,
        };
        let Some(setting) = setting else { continue };
        let (key, value) = setting.split_once('=').unwrap_or((setting, ""));
        if command_key(key) && !harmless_command(value, key) {
            return true;
        }
    }
    let args = git_args(words);
    let Some(sub) = args.first().map(String::as_str) else {
        return false;
    };
    let rest = &args[1..];
    // Whether a short option `letter` is in a cluster, stopping at a letter
    // in `valued` that takes the rest as its value; or a long option on
    // the line is a prefix (git's abbreviation) of `long`. Any prefix of one
    // letter or more counts (WI-4001 round 5, blocker 1: `--ex` is `--exec`):
    // an ambiguous one (`rebase --e`) is an error in git, so counting it only
    // refuses a command git refuses too.
    let short = |letter: char, valued: &[char]| {
        rest.iter().take_while(|a| *a != "--").any(|a| {
            a.len() > 1
                && a.starts_with('-')
                && !a.starts_with("--")
                && a[1..]
                    .chars()
                    .take_while(|c| *c == letter || !valued.contains(c))
                    .any(|c| c == letter)
        })
    };
    let long = |name: &str, min: usize| {
        rest.iter().take_while(|a| *a != "--").any(|a| {
            let flag = a.split_once('=').map_or(a.as_str(), |p| p.0);
            flag.len() >= min && name.starts_with(flag)
        })
    };
    match sub {
        "grep" => {
            short('O', &['e', 'f', 'A', 'B', 'C', 'm'])
                || long("--open-files-in-pager", "--o".len())
        }
        "difftool" => short('x', &['t']) || long("--extcmd", "--e".len()),
        "rebase" => short('x', &['s', 'X', 'C']) || long("--exec", "--e".len()),
        "bisect" => rest.first().is_some_and(|a| a == "run"),
        "submodule" => rest.iter().any(|a| a == "foreach"),
        "config" => {
            let reads = rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--get"
                        | "--get-all"
                        | "--get-regexp"
                        | "--get-urlmatch"
                        | "--list"
                        | "-l"
                        | "get"
                        | "list"
                        | "--unset"
                        | "--unset-all"
                        | "unset"
                )
            });
            if reads {
                return false;
            }
            rest.iter().enumerate().any(|(i, a)| {
                !a.starts_with('-')
                    && command_key(a)
                    && !harmless_command(rest.get(i + 1).map_or("", String::as_str), a)
            })
        }
        _ => false,
    }
}

/// Whether a substitution's pieces are exactly one literal `ssh-agent` call
/// (`ssh-agent`, `ssh-agent -s`, `/usr/bin/ssh-agent -t 3600`): flags and
/// numbers only, no expansion, no second command.
fn ssh_agent(pieces: &[Piece]) -> bool {
    let [Piece::Cmd(simple)] = pieces else {
        return false;
    };
    if simple.contains(['$', '`', '<', '>', '\\', '\'', '"']) {
        return false;
    }
    let words = shell_words(simple);
    words.first().is_some_and(|w| base(w) == "ssh-agent")
        && words[1..].iter().all(|w| {
            (w.len() > 1 && w.starts_with('-') && w[1..].chars().all(|c| c.is_ascii_alphabetic()))
                || w.chars().all(|c| c.is_ascii_digit())
        })
}

/// The command a `su`, `runuser` or `script` call runs: the value of `-c`
/// (as its own word, attached as `-c'…'`, or at the end of or inside a
/// cluster: `-lc '…'`, `-qc'…'`), or of `--command` / `--session-command`
/// by any unique prefix (`--comm=…`). A cluster stops at the first letter
/// that takes a value, as getopt reads it.
fn dash_c_payload(words: &[String]) -> Option<&str> {
    let program = base(&words[0]);
    // Short options that take a value other than `-c`.
    let valued: &[char] = if program == "script" {
        &['E', 'I', 'O', 'B', 'T', 'm', 'o']
    } else {
        &['s', 'g', 'G', 'w']
    };
    let mut args = words[1..].iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            return None;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let runs = !name.is_empty()
                && ("command".starts_with(name)
                    || (program != "script"
                        && name.len() > 1
                        && "session-command".starts_with(name)));
            if runs {
                return value.or_else(|| args.next().map(String::as_str));
            }
            continue;
        }
        if arg.len() < 2 || !arg.starts_with('-') {
            continue;
        }
        for (i, letter) in arg.char_indices().skip(1) {
            let rest = &arg[i + letter.len_utf8()..];
            if letter == 'c' {
                return if rest.is_empty() {
                    args.next().map(String::as_str)
                } else {
                    Some(rest)
                };
            }
            if valued.contains(&letter) {
                if rest.is_empty() {
                    args.next();
                }
                break;
            }
        }
    }
    None
}

/// The action `trap` installs: its first operand after any `--`, unless
/// that is `-` (reset) or the call only lists (`-p`, `-l`).
fn trap_action(words: &[String]) -> Option<&str> {
    let mut args = words[1..].iter().map(String::as_str).peekable();
    if args.peek() == Some(&"--") {
        args.next();
    } else if args.peek().is_some_and(|a| a.starts_with('-') && *a != "-") {
        return None;
    }
    args.next().filter(|a| *a != "-" && !a.is_empty())
}

/// Whether `word` holds a brace expansion: a `{` later closed by `}` with a
/// `,` or `..` between them. As a program it is not the word it looks like.
fn brace_expands(word: &str) -> bool {
    word.char_indices()
        .filter(|(_, c)| *c == '{')
        .any(|(i, _)| {
            word[i + 1..].find('}').is_some_and(|end| {
                let inner = &word[i + 1..i + 1 + end];
                inner.contains(',') || inner.contains("..")
            })
        })
}

/// A script operand that is not a file on disk but what the line feeds in:
/// a process substitution (the lexer spells it `/dev/fd/$`), stdin, or a
/// file descriptor.
fn reads_stdin(word: &str) -> bool {
    word == "-"
        || word.starts_with("/dev/fd/")
        || word.starts_with("/proc/self/fd/")
        || matches!(word, "/dev/stdin" | "/proc/self/fd/0")
}

/// A `<` redirection from a process substitution or stdin, attached to its
/// word (`</dev/stdin`).
fn stdin_redirect(word: &str) -> bool {
    let w = word.trim_start_matches(|c: char| c.is_ascii_digit());
    w.starts_with('<') && !w.starts_with("<<") && reads_stdin(w.trim_start_matches('<'))
}

/// A program spelled from `$HOME` (`$HOME/bin/tool`, `${HOME}/bin/tool`),
/// with `$HOME` put in, so it is read as the program it names.
fn home_program(word: &str) -> Option<String> {
    let rest = word
        .strip_prefix("$HOME/")
        .or_else(|| word.strip_prefix("${HOME}/"))?;
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    (!rest.contains(['$', '`'])).then(|| format!("{home}/{rest}"))
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

/// The directories git's `-C` options name.
fn git_dir_words(words: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut args = words.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "-C" {
            if let Some(d) = args.next() {
                out.push(d.as_str());
            }
        } else if let Some(d) = arg.strip_prefix("-C") {
            out.push(d);
        } else if arg == "-c" || arg == "--config-env" {
            args.next();
        } else if !arg.starts_with('-') {
            break;
        }
    }
    out
}

/// A command line as the lexer reads it: simple commands, the bounds of
/// `( … )` subshells, and the commands inside `$(…)`, backticks and `<(…)`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Cmd(String),
    Open,
    Close,
    Sub(Vec<Piece>),
}

/// How deep parens and substitutions nest before the lexer stops reading and
/// calls the line opaque, so a hostile `((((…` cannot exhaust the stack.
const MAX_NEST: usize = 64;

/// Leading words that open a compound command or negate one. A `(` after
/// only these still opens a subshell.
const RESERVED: &[&str] = &[
    "!", "{", "}", "if", "then", "else", "elif", "while", "until", "do", "time",
];

/// Split `command` into the pieces a shell would run: `;`, `&`, `&&`, `|`,
/// `||`, `|&` and newlines end a command; `(`/`)` bound a subshell; `$(…)`,
/// backticks and process substitutions are read as commands of their own
/// (also inside double quotes), leaving a `$` in the word; heredoc bodies
/// are skipped (an unquoted one holding an expansion is opaque); `#` starts
/// a comment. The bool is true when something could not be read with
/// certainty: an unterminated quote or substitution, a stray `)` (`case`),
/// a `(` inside a word (a function definition or array), or nesting past
/// [`MAX_NEST`].
fn lex(command: &str) -> (Vec<Piece>, bool) {
    let mut lexer = Lexer {
        s: command.chars().collect(),
        i: 0,
        opaque: false,
        heredocs: Vec::new(),
    };
    let pieces = lexer.seq(None, 0);
    (pieces, lexer.opaque)
}

struct Lexer {
    s: Vec<char>,
    i: usize,
    opaque: bool,
    /// Heredocs opened on the current line: delimiter, `<<-`, quoted.
    heredocs: Vec<(String, bool, bool)>,
}

impl Lexer {
    fn peek(&self, n: usize) -> Option<char> {
        self.s.get(self.i + n).copied()
    }

    fn sub(&mut self, end: char, nest: usize, out: &mut Vec<Piece>, cur: &mut String) {
        cur.push('$');
        let inner = self.seq(Some(end), nest + 1);
        out.push(Piece::Sub(inner));
    }

    fn seq(&mut self, end: Option<char>, nest: usize) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut cur = String::new();
        if nest > MAX_NEST {
            self.opaque = true;
            self.i = self.s.len();
            return out;
        }
        while let Some(c) = self.peek(0) {
            if Some(c) == end {
                self.i += 1;
                push_command(&mut out, &mut cur);
                return out;
            }
            self.i += 1;
            match c {
                // A backslash-newline is a line continuation: bash deletes
                // both, so `gi\<newline>t` is `git`.
                '\\' if self.peek(0) == Some('\n') => self.i += 1,
                '\\' => {
                    cur.push(c);
                    if let Some(n) = self.peek(0) {
                        cur.push(n);
                        self.i += 1;
                    }
                }
                // An ANSI-C string ends at the first quote no backslash
                // escapes, so `$'\''` does not close early here either.
                '$' if self.peek(0) == Some('\'') => {
                    cur.push(c);
                    cur.push('\'');
                    self.i += 1;
                    loop {
                        let Some(n) = self.peek(0) else {
                            self.opaque = true;
                            break;
                        };
                        self.i += 1;
                        cur.push(n);
                        if n == '\\' {
                            if let Some(e) = self.peek(0) {
                                cur.push(e);
                                self.i += 1;
                            }
                        } else if n == '\'' {
                            break;
                        }
                    }
                }
                '\'' => {
                    cur.push(c);
                    loop {
                        let Some(n) = self.peek(0) else {
                            self.opaque = true;
                            break;
                        };
                        self.i += 1;
                        cur.push(n);
                        if n == '\'' {
                            break;
                        }
                    }
                }
                '"' => self.double(nest, &mut out, &mut cur),
                '$' if self.peek(0) == Some('(') => {
                    self.i += 1;
                    self.sub(')', nest, &mut out, &mut cur);
                }
                // A process substitution expands to a `/dev/fd/N` path; the
                // word keeps that spelling so a shell or `source` given one
                // is known to read its script from the line.
                '<' | '>' if self.peek(0) == Some('(') => {
                    self.i += 1;
                    cur.push_str("/dev/fd/");
                    self.sub(')', nest, &mut out, &mut cur);
                }
                '`' => self.sub('`', nest, &mut out, &mut cur),
                '(' => {
                    if cur.split_whitespace().all(|w| RESERVED.contains(&w)) {
                        push_command(&mut out, &mut cur);
                        out.push(Piece::Open);
                        let inner = self.seq(Some(')'), nest + 1);
                        out.extend(inner);
                        out.push(Piece::Close);
                    } else {
                        self.opaque = true;
                        cur.push(c);
                    }
                }
                ')' => {
                    self.opaque = true;
                    push_command(&mut out, &mut cur);
                }
                '\n' => {
                    push_command(&mut out, &mut cur);
                    self.bodies();
                }
                ';' => push_command(&mut out, &mut cur),
                '|' => {
                    if matches!(self.peek(0), Some('|' | '&')) {
                        self.i += 1;
                    }
                    push_command(&mut out, &mut cur);
                }
                '&' => {
                    if self.peek(0) == Some('&') {
                        self.i += 1;
                        push_command(&mut out, &mut cur);
                    } else if cur.ends_with(['>', '<']) || self.peek(0) == Some('>') {
                        // `2>&1`, `&>file`: a redirection, not a separator.
                        cur.push(c);
                    } else {
                        push_command(&mut out, &mut cur);
                    }
                }
                '<' if self.peek(0) == Some('<') && self.peek(1) == Some('<') => {
                    self.i += 2;
                    cur.push_str("<<<");
                }
                '<' if self.peek(0) == Some('<') => {
                    self.i += 1;
                    cur.push_str("<<");
                    self.heredoc();
                }
                '#' if cur.is_empty() || cur.ends_with(char::is_whitespace) => {
                    while self.peek(0).is_some_and(|n| n != '\n') {
                        self.i += 1;
                    }
                }
                _ => cur.push(c),
            }
        }
        if end.is_some() {
            self.opaque = true;
        }
        push_command(&mut out, &mut cur);
        out
    }

    /// A double-quoted string: substitutions inside it still run.
    fn double(&mut self, nest: usize, out: &mut Vec<Piece>, cur: &mut String) {
        cur.push('"');
        while let Some(c) = self.peek(0) {
            self.i += 1;
            match c {
                '"' => {
                    cur.push(c);
                    return;
                }
                '\\' if self.peek(0) == Some('\n') => self.i += 1,
                '\\' => {
                    cur.push(c);
                    if let Some(n) = self.peek(0) {
                        cur.push(n);
                        self.i += 1;
                    }
                }
                '$' if self.peek(0) == Some('(') => {
                    self.i += 1;
                    self.sub(')', nest, out, cur);
                }
                '`' => self.sub('`', nest, out, cur),
                _ => cur.push(c),
            }
        }
        self.opaque = true;
    }

    /// Read a heredoc's delimiter; its body starts after the next newline.
    fn heredoc(&mut self) {
        let strip = self.peek(0) == Some('-');
        if strip {
            self.i += 1;
        }
        while matches!(self.peek(0), Some(' ' | '\t')) {
            self.i += 1;
        }
        let mut delim = String::new();
        let mut quoted = false;
        while let Some(c) = self.peek(0) {
            if c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')' | '(' | '<' | '>') {
                break;
            }
            self.i += 1;
            if matches!(c, '\'' | '"' | '\\') {
                quoted = true;
            } else {
                delim.push(c);
            }
        }
        if delim.is_empty() {
            self.opaque = true;
            return;
        }
        self.heredocs.push((delim, strip, quoted));
    }

    /// Skip the bodies of the heredocs opened on the line just ended. An
    /// unquoted body expands `$(…)` and backticks, which this does not
    /// follow: such a body is opaque.
    fn bodies(&mut self) {
        for (delim, strip, quoted) in std::mem::take(&mut self.heredocs) {
            while self.i < self.s.len() {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i] != '\n' {
                    self.i += 1;
                }
                let line: String = self.s[start..self.i].iter().collect();
                if self.i < self.s.len() {
                    self.i += 1;
                }
                let line = if strip {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if line == delim {
                    break;
                }
                if !quoted && (line.contains("$(") || line.contains('`')) {
                    self.opaque = true;
                }
            }
        }
    }
}

/// The simple commands in `pieces`, substitutions included.
fn flatten(pieces: &[Piece]) -> Vec<String> {
    let mut out = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Cmd(c) => out.push(c.clone()),
            Piece::Sub(inner) => out.extend(flatten(inner)),
            Piece::Open | Piece::Close => {}
        }
    }
    out
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
/// command or `rm` that would throw away a path that had uncommitted changes
/// when the session started. Only paths in the session's own repo count; a
/// git call whose repo cannot be resolved is taken to be the session's own,
/// and a part of the line this check cannot read that names git or rm
/// reaches every baseline path.
fn discard(policy: &SessionPolicy, calls: &Calls, repos: &mut Repos) -> Option<String> {
    let total = policy.discard_baseline.len();
    let refuse = |what: &str, hits: usize| {
        Some(format!(
            "refusing: discard guard: {what} would discard {hits} path(s) that had \
             uncommitted changes before this session started; name only files this \
             session changed"
        ))
    };
    if calls.blind_discard() {
        return refuse("a command this check cannot read", total);
    }
    let own = policy
        .workspace_path
        .as_deref()
        .map(|p| canonical(Path::new(p)));
    for call in &calls.calls {
        if call.rm {
            let hits = rm_hits(policy, call, own.as_deref(), repos);
            if hits > 0 {
                return refuse("rm", hits);
            }
            continue;
        }
        let args = git_args(&call.words);
        let Some(sub) = args.first() else {
            continue;
        };
        // A subcommand this reading cannot spell (`git restore${IFS}a.txt`,
        // `git $SUB a.txt`, `git {restore,x} a.txt`) may be any of them.
        let spelled = spelled(sub) && !call.aliased;
        let reach = if spelled {
            match discards(sub, &args[1..]) {
                Some(reach) => reach,
                None => continue,
            }
        } else {
            Reach {
                all: true,
                paths: Vec::new(),
            }
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
            total
        } else {
            let specs: Vec<String> = reach
                .paths
                .iter()
                .filter_map(|p| repo_relative(p, &dir, &root))
                .collect();
            baseline_hits(policy, &specs)
        };
        if hits > 0 {
            return refuse(&format!("git {}", label(sub)), hits);
        }
    }
    None
}

/// How many baseline entries the repo-relative pathspecs reach.
fn baseline_hits(policy: &SessionPolicy, specs: &[String]) -> usize {
    policy
        .discard_baseline
        .iter()
        .filter(|entry| specs.iter().any(|spec| reaches(spec, entry)))
        .count()
}

/// How many baseline paths a plain `rm` reaches. Each operand is resolved on
/// its own — `~/` expanded, made absolute against the directory it was typed
/// in, its parent directory canonicalised (so a symlinked spelling of the
/// session's tree is the tree) — and only an operand that lands in the
/// session's own tree, or names a directory holding it, counts. An operand
/// that cannot be read counts as every path: a glob, an expansion, `~user`,
/// brace expansion, or a relative path after a `cd`. So `rm -rf build`, `rm
/// /tmp/scratch/x` and `rm -rf <own>/new_dir` pass, while `rm *.txt` (even
/// in a scratch directory) and `cd X && rm f` are refused whenever the
/// baseline is not empty.
fn rm_hits(policy: &SessionPolicy, call: &GitCall, own: Option<&Path>, repos: &mut Repos) -> usize {
    let total = policy.discard_baseline.len();
    if call.unreadable {
        return total;
    }
    // A session record always names its tree; without one, the repo the
    // command was typed in stands for it.
    let root = match own {
        Some(own) => own.to_path_buf(),
        None => {
            let dir = canonical(&call.cwd);
            repos.top(&dir).unwrap_or(dir)
        }
    };
    let mut specs = Vec::new();
    for operand in &call.words {
        let Some(word) = expand_home(operand) else {
            return total;
        };
        if word.contains(['*', '?', '[', '$', '`', '{']) {
            return total;
        }
        let path = Path::new(&word);
        if !path.is_absolute() && call.redirected {
            return total;
        }
        for target in rm_targets(&resolve_dir(&word, &call.cwd)) {
            if root.starts_with(&target) {
                // `rm -rf ..`, `rm -rf /`: the whole tree goes.
                return total;
            }
            if let Ok(rel) = target.strip_prefix(&root) {
                specs.push(rel.to_string_lossy().trim_end_matches('/').to_string());
            }
        }
    }
    baseline_hits(policy, &specs)
}

/// The paths an `rm` of `path` deletes, resolved: the entry itself in its
/// canonical parent (`rm link` removes the link, not what it points at),
/// and, when the operand ends in `/`, what it resolves to as well, since an
/// `rm -r link/` may follow it.
fn rm_targets(path: &Path) -> Vec<PathBuf> {
    let lexical = normalize(path);
    let mut out = Vec::new();
    match (lexical.parent(), lexical.file_name()) {
        (Some(parent), Some(name)) => out.push(canonical(parent).join(name)),
        _ => out.push(canonical(&lexical)),
    }
    if path.to_string_lossy().ends_with('/') {
        out.push(canonical(&lexical));
    }
    out
}

/// `~` and `~/…` with `$HOME` put in; any other word as it is. `None` for a
/// `~user` form, or `~` with no `$HOME`: a path this check cannot read.
fn expand_home(word: &str) -> Option<String> {
    let Some(rest) = word.strip_prefix('~') else {
        return Some(word.to_string());
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(format!("{home}{rest}"))
}

/// Which discarding form `git <sub> <args>` is, if any. Mirrors gitguard's
/// `discard`, minus its staging rules. Options are read the way git's
/// parse-options reads them: a long option by any unique prefix (`--har` is
/// `--hard`), a short one anywhere in a cluster (`-qf`), up to a letter that
/// takes a value (`-bfix` is `-b fix`).
fn discards(sub: &str, args: &[String]) -> Option<Reach> {
    let opts = Opts::parse(sub, args);
    let paths = opts.paths.clone();
    let dashdash = opts.dashdash;
    let all = || {
        Some(Reach {
            all: true,
            paths: Vec::new(),
        })
    };
    let these = |paths: Vec<String>| Some(Reach { all: false, paths });
    let force = || opts.short('f') || opts.long("--force");
    // `--pathspec-from-file` (and `checkout-index --stdin`) read the paths
    // from a file or stdin: what they name is not on the line.
    if opts.long("--pathspec-from-file") || (sub == "checkout-index" && opts.long("--stdin")) {
        let staged_only = opts.safe(Some('S'), "--staged")
            && !(opts.short('W') || opts.long("--worktree"));
        match sub {
            "restore" if staged_only => return None,
            "rm" if opts.allow("--cached") => return None,
            "checkout" | "restore" | "rm" | "stash" | "checkout-index" => return all(),
            _ => {}
        }
    }
    match sub {
        // `git checkout-index -f` overwrites the working-tree file from the
        // index; `-a` every one. Without `-f` it leaves existing files.
        "checkout-index" => {
            if !force() {
                return None;
            }
            if opts.short('a') || opts.long("--all") {
                return all();
            }
            these(paths)
        }
        // `git mv -f <src> <dst>` overwrites the destination.
        "mv" => force().then_some(Reach { all: false, paths }),
        "checkout" => {
            if force() {
                return all();
            }
            if (opts.short('b') || opts.short('B') || opts.allow("--orphan")) && !dashdash {
                return None;
            }
            if dashdash {
                return these(after_dashdash(args));
            }
            // `git checkout <x>` is a branch switch, which git refuses over
            // local changes, or a path restore (`-m`/`--merge` re-creates
            // the file from the index): every word may be a path.
            (!paths.is_empty()).then_some(Reach { all: false, paths })
        }
        "restore" => {
            let staged = opts.safe(Some('S'), "--staged");
            let worktree = opts.short('W') || opts.long("--worktree");
            if staged && !worktree {
                return None; // unstages only; the working tree is left alone
            }
            these(paths)
        }
        "reset" => {
            (opts.long("--hard") || opts.long("--merge") || opts.long("--keep")).then_some(Reach {
                all: true,
                paths: Vec::new(),
            })
        }
        "clean" => {
            if opts.safe(Some('n'), "--dry-run") {
                return None;
            }
            force().then_some(Reach { all: false, paths })
        }
        "stash" => match paths.first().map(String::as_str) {
            None | Some("push") => these(paths.into_iter().skip(1).collect()),
            Some("save") => all(),
            // A bare `git stash -- <paths>` is a push.
            Some(_) if dashdash => these(after_dashdash(args)),
            _ => None,
        },
        // `git rm` deletes the working-tree file too unless `--cached`.
        "rm" => (!opts.allow("--cached")).then_some(Reach { all: false, paths }),
        "switch" => (force() || opts.long("--discard-changes")).then_some(Reach {
            all: true,
            paths: Vec::new(),
        }),
        // `git read-tree -u` with `-m`, `--reset` or `--prefix` updates the
        // working tree from the tree it reads (`--reset` over local
        // changes); `-n` only checks.
        "read-tree" => {
            let update = opts.short('u');
            let merges = opts.short('m') || opts.long("--reset") || opts.long("--prefix");
            let dry = opts.safe(Some('n'), "--dry-run");
            (update && merges && !dry).then_some(Reach {
                all: true,
                paths: Vec::new(),
            })
        }
        _ => None,
    }
}

/// A git subcommand's arguments as its parse-options reads them.
struct Opts {
    /// Long options, their `=value` cut off (`--source=x` is `--source`).
    longs: Vec<String>,
    /// Short option letters, every cluster taken apart up to the letter
    /// that takes a value.
    shorts: Vec<char>,
    /// Every option in the order given: `-x` for a short letter, the long
    /// name for a long one. A later option overrides an earlier one, so a
    /// safe flag is read from the last of its spellings and negations.
    order: Vec<String>,
    /// The positional words; everything after `--` among them.
    paths: Vec<String>,
    dashdash: bool,
}

impl Opts {
    fn parse(sub: &str, args: &[String]) -> Self {
        // The options of `sub` that take a value as the next word (or, for a
        // short one, the rest of its cluster).
        let (short_valued, long_valued): (&[char], &[&str]) = match sub {
            "checkout" => (
                &['b', 'B'],
                &["--orphan", "--conflict", "--pathspec-from-file"],
            ),
            "switch" => (
                &['c', 'C'],
                &["--create", "--force-create", "--orphan", "--conflict"],
            ),
            "restore" => (&['s'], &["--source", "--conflict", "--pathspec-from-file"]),
            "stash" => (&['m'], &["--message", "--pathspec-from-file"]),
            "clean" => (&['e'], &["--exclude"]),
            "reset" | "rm" => (&[], &["--pathspec-from-file"]),
            _ => (&[], &[]),
        };
        let mut out = Opts {
            longs: Vec::new(),
            shorts: Vec::new(),
            order: Vec::new(),
            paths: Vec::new(),
            dashdash: false,
        };
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if arg == "--" {
                out.paths.extend(rest.cloned());
                out.dashdash = true;
                break;
            }
            if let Some(long) = arg.strip_prefix("--") {
                let (name, value) = match long.split_once('=') {
                    Some((name, _)) => (format!("--{name}"), true),
                    None => (arg.clone(), false),
                };
                if !value && name.len() > 2 && long_valued.iter().any(|v| v.starts_with(&name)) {
                    rest.next();
                }
                out.order.push(name.clone());
                out.longs.push(name);
            } else if arg.len() > 1 && arg.starts_with('-') {
                for (i, letter) in arg[1..].char_indices() {
                    out.shorts.push(letter);
                    out.order.push(format!("-{letter}"));
                    if short_valued.contains(&letter) {
                        if i + letter.len_utf8() == arg.len() - 1 {
                            rest.next();
                        }
                        break;
                    }
                }
            } else {
                out.paths.push(arg.clone());
            }
        }
        out
    }

    fn short(&self, letter: char) -> bool {
        self.shorts.contains(&letter)
    }

    /// Whether a long option on the line is `name` or a prefix of it, as
    /// git takes it. An ambiguous prefix is an error in git, so counting it
    /// as `name` only ever refuses a command git would refuse too.
    fn long(&self, name: &str) -> bool {
        self.longs
            .iter()
            .any(|l| l.len() > 2 && name.starts_with(l.as_str()))
    }

    /// [`Opts::long`] for an option that makes the call safe: a later
    /// `--no-<name>` (by prefix) takes it back.
    fn allow(&self, name: &str) -> bool {
        self.safe(None, name)
    }

    /// Whether the safe option `name`, or its short spelling `letter`, is
    /// in force: given, and not taken back by a later `--no-<name>` (by
    /// prefix). The last spelling wins, so `-n --no-dry-run` is not a dry
    /// run and `--no-dry-run -n` is (WI-4001 round 5, blocker 2).
    fn safe(&self, letter: Option<char>, name: &str) -> bool {
        let on = |o: &String| {
            letter.is_some_and(|c| *o == format!("-{c}"))
                || (o.len() > 2 && !o.starts_with("--no-") && name.starts_with(o.as_str()))
        };
        let off = |o: &String| {
            o.strip_prefix("--no-")
                .is_some_and(|n| !n.is_empty() && name[2..].starts_with(n))
        };
        self.order
            .iter()
            .rev()
            .find(|o| on(o) || off(o))
            .is_some_and(|o| on(o))
    }
}

fn after_dashdash(args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|a| *a != "--")
        .skip(1)
        .cloned()
        .collect()
}

/// A git subcommand spelled as a plain word, not an expansion or pattern.
fn spelled(sub: &str) -> bool {
    sub.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `sub` as a reason may name it: an unspelled word may hold a path, and a
/// reason never names one.
fn label(sub: &str) -> &str {
    if spelled(sub) {
        sub
    } else {
        "<unreadable subcommand>"
    }
}

/// A pathspec this reading can resolve: no glob, expansion, brace pattern
/// or pathspec magic.
fn readable_spec(spec: &str) -> bool {
    !spec.starts_with([':', '~']) && !spec.contains(['*', '?', '[', '$', '`', '{'])
}

/// `spec`, relative to `dir`, as a path relative to `root`; `""` is the
/// whole tree, and so is a directory above `root`. Its parent is
/// canonicalised, so a symlinked spelling of the tree is the tree. `None`
/// when it lies outside `root`.
fn repo_relative(spec: &str, dir: &Path, root: &Path) -> Option<String> {
    let path = normalize(&resolve_dir(spec, dir));
    let path = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical(parent).join(name),
        _ => canonical(&path),
    };
    if root.starts_with(&path) {
        return Some(String::new());
    }
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
/// the session's own worktree. A repo lookup that fails is not protected, so
/// a broken git never refuses an ordinary command; but a write whose repo
/// the line hides (`GIT_DIR=`, `--git-dir`, `-C "$X"`, a `cd` this check
/// cannot follow) or that sits in a part of the line it cannot read is
/// refused, since it could be any checkout.
fn protected(policy: &SessionPolicy, calls: &Calls, repos: &mut Repos) -> Option<String> {
    let own = policy
        .workspace_path
        .as_deref()
        .map(|p| canonical(Path::new(p)));
    let checkouts: Vec<PathBuf> = policy
        .protected_checkouts
        .iter()
        .map(|p| canonical(Path::new(p)))
        .collect();
    let run_in = own
        .as_deref()
        .map_or_else(|| "the worktree".into(), |p| p.display().to_string());
    if calls.blind_write() {
        return Some(format!(
            "refusing: protected checkout: a git write this check cannot read may reach a \
             project checkout; run it as a plain command in {run_in}"
        ));
    }
    for call in &calls.calls {
        if call.rm {
            continue;
        }
        let args = git_args(&call.words);
        let Some(sub) = args.first() else {
            continue;
        };
        if spelled(sub) && !call.aliased && !git_write(sub, &args[1..]) {
            continue;
        }
        let sub = label(sub);
        if call.redirected {
            return Some(format!(
                "refusing: protected checkout: git {sub} names its repo in a way this check \
                 cannot read (GIT_DIR, --git-dir, --work-tree or an unfollowed cd); run it \
                 with a literal -C in {run_in}"
            ));
        }
        let Some(top) = repos.top(&canonical(&git_dir(&call.words, &call.cwd))) else {
            continue;
        };
        if own.as_ref() == Some(&top) || !checkouts.contains(&top) {
            continue;
        }
        return Some(format!(
            "refusing: protected checkout: git {sub} writes to a project checkout, not this \
             session's worktree; run it in {run_in}"
        ));
    }
    None
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
        if arg == "-C" || arg == "-c" || arg == "--config-env" {
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
    command_words_ext(simple).0
}

/// [`command_words`], and whether a wrapper hid what it runs (`env -S`,
/// `env -C`, a `sudo -s`/`-i` shell): the bool is true when the program left
/// is not the one that runs. Leading reserved words (`!`, `{`, `then`, `do`,
/// `time -p`, …) are stepped over like wrappers, so `if true; then git …`
/// and `! git …` are judged as the git call they hold.
fn command_words_ext(simple: &str) -> (Vec<String>, bool) {
    let mut words = shell_words(simple);
    let mut opaque = false;
    // Drop `n` words from the front, as long as one would be left.
    let drop = |words: &mut Vec<String>, n: usize| {
        let n = n.min(words.len());
        words.drain(..n);
    };
    loop {
        while words.len() > 1 && is_assignment(&words[0]) {
            words.remove(0);
        }
        match words.first().map(|w| base(w)) {
            Some("!" | "{" | "}" | "then" | "else" | "elif" | "do" | "if" | "while" | "until") => {
                drop(&mut words, 1);
            }
            Some("time") => {
                drop(&mut words, 1);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    words.remove(0);
                }
            }
            Some("sudo" | "doas") => {
                words.remove(0);
                while let Some(flag) = words.first().cloned() {
                    if !flag.starts_with('-') {
                        break;
                    }
                    words.remove(0);
                    if flag == "--" {
                        break;
                    }
                    // `sudo -u user git …`: the value is not the program.
                    if matches!(
                        flag.as_str(),
                        "-u" | "-g"
                            | "-h"
                            | "-p"
                            | "-C"
                            | "-D"
                            | "-r"
                            | "-t"
                            | "-U"
                            | "-T"
                            | "--user"
                            | "--group"
                            | "--host"
                            | "--prompt"
                            | "--chdir"
                            | "--close-from"
                            | "--role"
                            | "--type"
                            | "--other-user"
                            | "--command-timeout"
                    ) {
                        if flag == "-D" || flag == "--chdir" {
                            opaque = true;
                        }
                        drop(&mut words, 1);
                    } else if flag == "-s"
                        || flag == "-i"
                        || flag == "--shell"
                        || flag == "--login"
                        || flag.starts_with("--chdir=")
                    {
                        opaque = true;
                    }
                }
            }
            Some("command" | "exec" | "nohup" | "builtin" | "setsid" | "chronic" | "busybox") => {
                words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    let flag = words.remove(0);
                    // `exec -a name git …`
                    if flag == "-a" {
                        drop(&mut words, 1);
                    }
                }
            }
            Some("nice" | "ionice" | "stdbuf") => {
                words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    let flag = words.remove(0);
                    // `nice -n 5`, `ionice -c 3`: the value is the next word.
                    if matches!(
                        flag.as_str(),
                        "-n" | "--adjustment" | "-c" | "--class" | "-p" | "-t" | "-i" | "-o" | "-e"
                    ) && words.first().is_some_and(|w| !w.starts_with('-'))
                    {
                        words.remove(0);
                    }
                }
            }
            Some("timeout") => {
                words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    let flag = words.remove(0);
                    if matches!(flag.as_str(), "-s" | "--signal" | "-k" | "--kill-after") {
                        drop(&mut words, 1);
                    }
                }
                // The duration (`timeout 10 git …`).
                if words.first().is_some_and(|w| !w.starts_with('-')) {
                    words.remove(0);
                }
            }
            Some("env") => {
                words.remove(0);
                while let Some(w) = words.first().cloned() {
                    if is_assignment(&w) {
                        words.remove(0);
                        continue;
                    }
                    if !w.starts_with('-') || w == "-" {
                        break;
                    }
                    words.remove(0);
                    if w == "--" {
                        break;
                    }
                    match env_option(&w) {
                        EnvOption::Plain => {}
                        // `env -u NAME git …`: the name is not the program.
                        EnvOption::Unset(attached) => {
                            if !attached {
                                drop(&mut words, 1);
                            }
                        }
                        // A changed directory: where it runs is not on the
                        // line.
                        EnvOption::Chdir(attached) => {
                            opaque = true;
                            if !attached {
                                drop(&mut words, 1);
                            }
                        }
                        // `env -S 'rm a.txt'`, `env -S'rm a.txt'`, `env
                        // --split-string=…`: the string is split into the
                        // program and its arguments, ahead of the words
                        // after it. Read it as the command it spells.
                        EnvOption::Split(value) => {
                            let value = match value {
                                Some(v) => v,
                                None if words.is_empty() => String::new(),
                                None => words.remove(0),
                            };
                            let mut split = shell_words(&value);
                            split.append(&mut words);
                            words = split;
                            break;
                        }
                    }
                }
            }
            _ => break,
        }
    }
    (words, opaque)
}

/// One `env` option word, read as GNU env's getopt reads it: a cluster
/// up to the first letter that takes a value (`-iS'rm a.txt'` is `-i -S
/// 'rm a.txt'`), and a long option by any unique prefix (`--split=…`).
enum EnvOption {
    Plain,
    /// `-u`/`--unset`; true when its value is attached.
    Unset(bool),
    /// `-C`/`--chdir`; true when its value is attached.
    Chdir(bool),
    /// `-S`/`--split-string`, with its value when attached.
    Split(Option<String>),
}

fn env_option(word: &str) -> EnvOption {
    if let Some(long) = word.strip_prefix("--") {
        let (name, value) = match long.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (long, None),
        };
        let is = |full: &str| !name.is_empty() && full.starts_with(name);
        return if is("split-string") {
            EnvOption::Split(value)
        } else if is("unset") {
            EnvOption::Unset(value.is_some())
        } else if is("chdir") {
            EnvOption::Chdir(value.is_some())
        } else {
            EnvOption::Plain
        };
    }
    for (i, letter) in word.char_indices().skip(1) {
        let rest = &word[i + letter.len_utf8()..];
        match letter {
            'S' => return EnvOption::Split((!rest.is_empty()).then(|| rest.to_string())),
            'u' => return EnvOption::Unset(!rest.is_empty()),
            'C' => return EnvOption::Chdir(!rest.is_empty()),
            _ => {}
        }
    }
    EnvOption::Plain
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Split a command line into the simple commands a shell would run,
/// including those in subshells, groups, compound-command bodies and
/// substitutions ([`lex`]). Quotes suppress splitting, so `echo "a && b"`
/// stays one command.
fn split_commands(command: &str) -> Vec<String> {
    flatten(&lex(command).0)
}

fn push_command(out: &mut Vec<Piece>, cur: &mut String) {
    let trimmed = cur.trim();
    if !trimmed.is_empty() {
        out.push(Piece::Cmd(trimmed.to_string()));
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
            // `$'\x72m'` is `rm`: an ANSI-C string is decoded, not kept.
            None if c == '$' && chars.peek() == Some(&'\'') => {
                chars.next();
                cur.push_str(&unescape(&mut chars, true));
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

/// Up to `max` digits in `radix` from `chars`, as a number, or `None` when
/// there is none.
fn escape_digits(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    radix: u32,
    max: usize,
) -> Option<u32> {
    let mut n: u32 = 0;
    let mut read = 0;
    while read < max {
        let Some(d) = chars.peek().and_then(|c| c.to_digit(radix)) else {
            break;
        };
        n = n.saturating_mul(radix).saturating_add(d);
        chars.next();
        read += 1;
    }
    (read > 0).then_some(n)
}

/// Backslash escapes decoded the way bash decodes an ANSI-C string (and
/// `printf`/`echo -e` their argument), up to and including an unescaped `'`
/// when `quoted` (the body of `$'…'`), else to the end. An escape this does
/// not know is kept as written.
fn unescape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, quoted: bool) -> String {
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\'' && quoted {
            break;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            out.push('\\');
            break;
        };
        let decoded = match e {
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            'e' | 'E' => Some('\u{1b}'),
            'f' => Some('\u{c}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\u{b}'),
            '\\' | '\'' | '"' | '?' => Some(e),
            'x' => escape_digits(chars, 16, 2).and_then(char::from_u32),
            'u' => escape_digits(chars, 16, 4).and_then(char::from_u32),
            'U' => escape_digits(chars, 16, 8).and_then(char::from_u32),
            'c' => chars
                .next()
                .and_then(|x| char::from_u32(u32::from(x) & 0x1f)),
            '0'..='7' => {
                // Up to three octal digits, this one included (`\162`).
                let mut n = e.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            n = n * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                char::from_u32(n & 0xff)
            }
            _ => None,
        };
        match decoded {
            Some(d) => out.push(d),
            None => {
                out.push('\\');
                out.push(e);
            }
        }
    }
    out
}

/// `text` with every ANSI-C string (`$'…'`) decoded in place.
fn ansi_c_decoded(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'\'') {
            chars.next();
            out.push_str(&unescape(&mut chars, true));
        } else {
            out.push(c);
        }
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

    /// WI-3985: a worker's own edits in a fresh clone are not in the
    /// baseline, so staging and committing them is never a discard.
    #[test]
    fn a_session_may_stage_and_commit_its_own_edits() {
        let root = Path::new("/r");
        let p = workspace(&["README.md"], Some("worktree"), root);
        for command in [
            "git add src/new.rs",
            "git add -A",
            "git commit -m 'feat: own change'",
            "git rm --cached README.md",
        ] {
            assert_eq!(check(&p, command, root), None, "{command}");
        }
    }

    /// `rm` of a tracked file that had uncommitted changes is a discard.
    #[test]
    fn plain_rm_of_a_baseline_file_is_refused() {
        let root = Path::new("/r");
        let p = workspace(&["README.md", "src/a.py"], Some("checkout"), root);
        for command in [
            "rm README.md",
            "rm -- src/a.py",
            "git rm README.md",
            "git rm -f src/a.py",
        ] {
            let reason = check(&p, command, root).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("discard guard"), "{command}: {reason}");
        }
        for command in [
            "rm new_file.txt",
            "git rm --cached src/a.py",
            "rm -rf /tmp/vogt-scratch-x",
        ] {
            assert_eq!(check(&p, command, root), None, "{command}");
        }
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

    // ---- WI-1326 review blockers -------------------------------------------

    /// Blocker 2: shell forms that hide a git or rm call from a split on
    /// `;`, `&&`, `|` and newlines. Each is refused with README.md in the
    /// baseline, from inside the session's own (real) repo.
    #[test]
    fn shell_forms_do_not_hide_a_discard() {
        let root = repo("forms");
        let p = workspace(&["README.md"], Some("worktree"), &root);
        for command in [
            "(git checkout -- README.md)",
            "( git checkout -- README.md )",
            "{ git checkout -- README.md; }",
            "true & git checkout -- README.md",
            "sleep 1 &\ngit checkout -- README.md",
            "if true; then git checkout -- README.md; fi",
            "if git checkout -- README.md; then :; fi",
            "for f in a; do git checkout -- README.md; done",
            "while false; do :; done; until true; do git checkout -- README.md; done",
            "! git checkout -- README.md",
            "time git checkout -- README.md",
            "time -p git checkout -- README.md",
            "echo $(git checkout -- README.md)",
            "echo \"$(git checkout -- README.md)\"",
            "echo `git checkout -- README.md`",
            "cat <(git checkout -- README.md)",
            "env -u HOME git checkout -- README.md",
            "env -i -u X PATH=/usr/bin git checkout -- README.md",
            "bash -c -- 'git checkout -- README.md'",
            "bash -e -c 'git checkout -- README.md'",
            "bash -o pipefail -c 'git checkout -- README.md'",
            "alias g=git; g checkout -- README.md",
            "g() { git checkout -- README.md; }; g",
            "function g { git checkout -- README.md; }; g",
            "case x in x) git checkout -- README.md;; esac",
            "G=git; $G checkout -- README.md",
            "echo 'git checkout -- README.md' | bash",
            "printf 'git checkout -- README.md' | sh",
            "cat cmds.txt | bash # rm",
            "sudo -u root git checkout -- README.md",
            "nice -n 5 git checkout -- README.md",
            "ls\ngit checkout -- README.md",
            "ls\n\trm README.md",
            "(rm README.md)",
            "echo $(rm README.md)",
            "x=1 rm README.md",
            "git status |& git checkout -- README.md",
        ] {
            let reason = check(&p, command, &root).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("discard guard"), "{command}: {reason}");
            assert!(!reason.contains("README"), "{reason}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Re-review blockers on c09846cc515ef: a line continuation, a variable
    /// or ANSI-C string as the program, an expansion in a git subcommand or
    /// pathspec, and a shell reading its script from a process substitution
    /// or stdin. Each was allowed with `README.md` in the baseline.
    #[test]
    fn rereview_bypasses_are_refused() {
        let root = repo("rereview");
        let p = workspace(&["README.md"], Some("worktree"), &root);
        for command in [
            // 1. backslash-newline continuations
            "git restore \\\nREADME.md",
            "git \\\nrestore README.md",
            "gi\\\nt restore README.md",
            "rm \\\nREADME.md",
            "r\\\nm README.md",
            "git restore \"READ\\\nME.md\"",
            // 2. a variable or ANSI-C string as the program, or as the
            // payload of eval / sh -c
            "R=rm; $R README.md",
            "CMD='rm README.md'; $CMD",
            "CMD='git restore README.md'; eval \"$CMD\"",
            "CMD='git restore README.md'; eval $CMD",
            "CMD='git restore README.md'; bash -c \"$CMD\"",
            "G=git; S=restore; $G $S README.md",
            "$'\\x72m' README.md",
            "$'\\162m' README.md",
            "$'g\\x69t' restore README.md",
            "R=r; ${R}m README.md",
            // 3. an expansion in the subcommand, a brace pattern
            "git restore${IFS}README.md",
            "git restore {README,x}.md",
            "git $SUB README.md",
            "git checkout -- {README,x}.md",
            "git restore src/{a,b}.rs",
            "R=$(printf 'r''m'); $R README.md",
            "eval \"$(echo rm README.md)\"",
            // 4. a shell reading its script from a process substitution or stdin
            "bash <(echo git restore README.md)",
            "source <(echo git restore README.md)",
            ". <(echo git restore README.md)",
            "bash /dev/stdin <<< 'git restore README.md'",
            "bash < <(echo git restore README.md)",
            "sh /dev/fd/3 3<<<'git restore README.md'",
            "bash <(printf '\\x72m README.md')",
        ] {
            let reason =
                check(&p, command, &root).unwrap_or_else(|| panic!("allowed: {command:?}"));
            assert!(reason.contains("discard guard"), "{command}: {reason}");
            assert!(!reason.contains("README"), "{reason}");
        }
        // What can still be read passes.
        for command in [
            "git restore \\\nsrc/own.rs",
            "cargo test \\\n  -p vogt-engine-server",
            "$'\\x72m' new.txt",
            "$'g\\x69t' status",
            "echo $HOME && git status",
            "source ./env.sh && git status",
            "eval \"$(ssh-agent -s)\"; git push",
        ] {
            assert_eq!(check(&p, command, &root), None, "{command:?}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The structural reading must not refuse what it can read: these name
    /// git or rm inside the same forms but reach no baseline path.
    #[test]
    fn shell_forms_still_allow_what_they_can_read() {
        let root = repo("forms-ok");
        let p = workspace(&["README.md"], Some("worktree"), &root);
        for command in [
            "(git status)",
            "{ git diff; }",
            "git log & git status",
            "if git diff --quiet; then echo clean; fi",
            "for f in a b; do git log -1 -- $f; done",
            "echo $(git rev-parse HEAD)",
            "echo \"$(git rev-parse --short HEAD)\"",
            "(git checkout -- src/own.rs)",
            "time git status",
            "! git diff --quiet",
            "env -u HOME git status",
            "bash -c -- 'git log'",
            "cargo test 2>&1 | tail -5",
            "cargo build &> /tmp/build.log",
            "echo done # git checkout -- README.md",
            "git commit -F - <<'EOF'\nfix: x\n\ngit checkout -- README.md\nEOF",
            "eval \"$(ssh-agent -s)\"",
            "rm -rf target",
        ] {
            assert_eq!(check(&p, command, &root), None, "{command}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 2, the protected-checkout half: a write whose repo the line
    /// hides is refused in a worktree session.
    #[test]
    fn hidden_repos_do_not_escape_the_protected_checkout_rule() {
        let (main, wt) = checkout_and_worktree("hidden");
        let mut p = workspace(&[], Some("worktree"), &wt);
        p.protected_checkouts = vec![main.display().to_string()];
        let c = main.display();
        for command in [
            format!("(cd {c} && git commit -m x)"),
            format!("{{ cd {c}; git commit -m x; }}"),
            format!("true & cd {c} && git commit -m x"),
            format!("echo $(cd {c} && git commit -m x)"),
            format!("GIT_DIR={c}/.git git commit -m x"),
            format!("export GIT_DIR={c}/.git; git commit -m x"),
            format!("git --git-dir={c}/.git commit -m x"),
            format!("git --work-tree={c} add ."),
            "cd \"$REPO\" && git commit -m x".to_string(),
            "git -C \"$REPO\" commit -m x".to_string(),
            format!("alias g='git -C {c}'; g commit -m x"),
            "R=git; $R commit -m x".to_string(),
            format!("if true; then git -C {c} commit -m x; fi"),
            format!("env -C {c} git commit -m x"),
            format!("bash -c -- 'git -C {c} commit -m x'"),
        ] {
            let reason = check(&p, &command, &wt).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("protected checkout"), "{command}: {reason}");
        }
        // Reads, and writes in its own worktree, still pass.
        for command in [
            format!("(cd {c} && git log -1)"),
            format!("GIT_DIR={c}/.git git log -1"),
            "(git commit --allow-empty -m x)".to_string(),
            format!("(cd {} && git commit --allow-empty -m x)", wt.display()),
            "git add src/new.rs && git commit -m 'feat: own'".to_string(),
        ] {
            assert_eq!(check(&p, &command, &wt), None, "{command}");
        }
        std::fs::remove_dir_all(&wt).ok();
        std::fs::remove_dir_all(&main).ok();
    }

    /// Blocker 3: an `rm` is judged by the path it deletes, not by the
    /// directory it was typed in, and a symlinked spelling of the session's
    /// tree is the tree.
    #[test]
    fn rm_is_judged_by_the_path_it_deletes() {
        let own = repo("rm-own");
        std::fs::create_dir_all(own.join("src")).unwrap();
        std::fs::write(own.join("README.md"), "x").unwrap();
        std::fs::write(own.join("src/a.py"), "x").unwrap();
        let other = repo("rm-other");
        let link = own.with_file_name(format!(
            "{}-link",
            own.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&own, &link).unwrap();
        let p = workspace(&["README.md", "src/a.py"], Some("worktree"), &own);
        let o = own.display();
        let l = link.display();
        for (command, cwd) in [
            (format!("rm {o}/README.md"), &other),
            (format!("rm -rf {o}/src"), &other),
            (format!("rm -rf {o}"), &other),
            (format!("rm -f -- {o}/README.md"), &other),
            (format!("unlink {o}/README.md"), &other),
            (format!("rm {l}/README.md"), &own),
            (format!("rm {l}/src/a.py"), &other),
            (format!("rm -r {l}/src/"), &other),
            (format!("rm -rf {o}/src/../README.md"), &other),
            ("rm -rf ..".to_string(), &own.join("src")),
            ("rm ../README.md".to_string(), &own.join("src")),
            ("rm README.md".to_string(), &own),
            (format!("cd {o} && rm README.md"), &other),
            ("cd elsewhere && rm notes.txt".to_string(), &own),
            ("rm *.md".to_string(), &own),
            ("rm -rf /tmp/scratch/*".to_string(), &own),
            ("rm $FILE".to_string(), &own),
            ("rm ~other/README.md".to_string(), &own),
            ("rm {README.md,x}".to_string(), &own),
        ] {
            let reason = check(&p, &command, cwd).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("discard guard"), "{command}: {reason}");
        }
        // The session's own new files, other repos and scratch dirs pass,
        // from wherever the rm is typed.
        let scratch = std::env::temp_dir().join(format!("vogt-rm-scratch-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let t = scratch.display();
        for (command, cwd) in [
            (format!("rm {o}/new_file.rs"), &other),
            (format!("rm -rf {o}/target"), &other),
            (format!("rm -rf {l}/target"), &own),
            (format!("rm {}/README.md", other.display()), &own),
            (format!("rm -rf {t}/out"), &own),
            (format!("rm -rf {t}"), &own),
            ("rm -rf build".to_string(), &own),
            ("rm src/new.rs".to_string(), &own),
            ("rm -rf out".to_string(), &scratch),
            (format!("rm {l}"), &other),
        ] {
            assert_eq!(check(&p, &command, cwd), None, "{command}");
        }
        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&scratch).ok();
        std::fs::remove_dir_all(&own).ok();
        std::fs::remove_dir_all(&other).ok();
    }

    /// WI-3985 stays fixed: a worker in its own fresh clone (empty baseline)
    /// stages, commits and deletes its own files with any shell form.
    #[test]
    fn a_fresh_clone_runs_its_own_work_in_any_form() {
        let root = repo("fresh-forms");
        let p = workspace(&[], Some("worktree"), &root);
        for command in [
            "git add -A && git commit -m 'feat: x'",
            "(git add src/new.rs; git commit -m x)",
            "rm -rf ./*",
            "cd sub && rm -rf *",
            "echo $(git checkout -- .)",
            "alias g=git; g checkout -- .",
        ] {
            assert_eq!(check(&p, command, &root), None, "{command}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The lexer never panics, and fails closed on what it cannot balance.
    #[test]
    fn the_lexer_is_bounded() {
        let deep = "(".repeat(10_000) + "git checkout -- README.md";
        assert!(lex(&deep).1);
        let subs = "$(".repeat(10_000);
        assert!(lex(&subs).1);
        for text in [
            "'",
            "\"",
            "`",
            "$(",
            ")",
            "<<",
            "<<''",
            "a <<EOF\n",
            "\0",
            "",
        ] {
            let _ = lex(text);
        }
        let root = Path::new("/r");
        let p = workspace(&["README.md"], Some("checkout"), root);
        assert!(check(&p, &deep, root).is_some());
        assert!(check(&p, "echo 'unterminated; git checkout -- README.md", root).is_some());
    }

    /// WI-4001 (round 3 re-review): the guard's must-refuse and must-allow
    /// tables, against a worktree with `a.txt` dirty in its baseline.
    fn round3(name: &str) -> (PathBuf, SessionPolicy) {
        let root = repo(&format!("round3-{name}"));
        std::fs::write(root.join("a.txt"), "x").unwrap();
        let p = workspace(&["a.txt"], Some("worktree"), &root);
        (root, p)
    }

    fn assert_refused(p: &SessionPolicy, root: &Path, commands: &[&str]) {
        let mut allowed = Vec::new();
        for command in commands {
            match check(p, command, root) {
                Some(reason) => {
                    assert!(reason.contains("discard guard"), "{command}: {reason}");
                    assert!(!reason.contains("a.txt"), "{reason}");
                }
                None => allowed.push(*command),
            }
        }
        assert!(allowed.is_empty(), "allowed: {allowed:#?}");
    }

    fn assert_allowed(p: &SessionPolicy, root: &Path, commands: &[&str]) {
        for command in commands {
            assert_eq!(check(p, command, root), None, "{command:?}");
        }
    }

    /// The ordinary worker commands the guard must keep allowing with a
    /// non-empty baseline.
    #[test]
    fn round3_ordinary_commands_still_pass() {
        let (root, p) = round3("ok");
        assert_allowed(
            &p,
            &root,
            &[
                "cd engine && cargo test",
                "git commit -m 'fix: x'",
                "git commit -F - <<'EOF'\nfix: x\n\nbody\nEOF",
                "for f in a b; do echo $f; done",
                "eval \"$(ssh-agent -s)\"; git push",
                "eval \"$(ssh-agent -s)\"",
                "git restore --staged a.txt",
                "git status && git diff",
                "timeout 60 cargo test",
                "ls src/{a,b}.rs",
                "echo {a,b}",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Bypass 3: a brace expansion as the program word.
    #[test]
    fn round3_braced_programs_are_refused() {
        let (root, p) = round3("braces");
        assert_refused(
            &p,
            &root,
            &[
                "{rm,a.txt}",
                "{git,restore,a.txt}",
                "{r,}m a.txt",
                "(echo; {rm,a.txt})",
                "bash -c '{rm,a.txt}'",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Bypass 6: a `trap` payload runs later, in this shell.
    #[test]
    fn round3_trap_payloads_are_read() {
        let (root, p) = round3("trap");
        assert_refused(
            &p,
            &root,
            &[
                "trap 'rm a.txt' EXIT",
                "trap -- 'git restore a.txt' EXIT INT",
                "trap \"git checkout -- a.txt\" ERR",
                "trap \"$CMD\" EXIT",
                "trap 'cd /; rm -rf x' EXIT; true",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "trap 'rm -f /tmp/wi-scratch.log' EXIT",
                "trap - EXIT",
                "trap -p",
                "trap 'echo done' EXIT; cargo test",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Bypass 4: a git alias, set on the line or by `git config`, names a
    /// subcommand this reading cannot see.
    #[test]
    fn round3_git_aliases_are_refused() {
        let (root, p) = round3("alias");
        assert_refused(
            &p,
            &root,
            &[
                "git -c alias.r=restore r a.txt",
                "git -c 'alias.r=!rm a.txt' r",
                "git -c ALIAS.r=restore r a.txt",
                "git -c al\"\"ias.r=restore r a.txt",
                "K=x git --config-env=alias.r=K r a.txt",
                "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.r GIT_CONFIG_VALUE_0=restore git r a.txt",
                "GIT_CONFIG_PARAMETERS=\"'alias.r'='restore'\" git r a.txt",
                "git config alias.r restore; git r a.txt",
                "git config alias.r restore",
                "git config --global alias.r '!rm a.txt'",
                "git -c include.path=/tmp/x r a.txt",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "git config --get alias.st",
                "git config --list",
                "git config user.name x",
                "git -c color.ui=never log -1",
                "git -c alias.st=status status",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Bypass 4, protected-checkout half: an alias hides a write's verb.
    #[test]
    fn round3_git_aliases_do_not_escape_the_protected_checkout_rule() {
        let (main, wt) = checkout_and_worktree("round3-alias-pc");
        let mut p = workspace(&[], Some("worktree"), &wt);
        p.protected_checkouts = vec![main.display().to_string()];
        let c = main.display();
        for command in [
            format!("git -C {c} -c alias.c=commit c -m x"),
            format!("git -c alias.c=commit -C {c} c -m x"),
            format!("git -c color.ui=never -C {c} commit -m x"),
            format!("GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.c GIT_CONFIG_VALUE_0=commit git -C {c} c -m x"),
        ] {
            let reason = check(&p, &command, &wt).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("protected checkout"), "{command}: {reason}");
        }
        assert_eq!(check(&p, &format!("git -C {c} log -1"), &wt), None);
        assert_eq!(
            check(&p, &format!("git -C {c} -c alias.l=log log -1"), &wt),
            None
        );
        std::fs::remove_dir_all(&wt).ok();
        std::fs::remove_dir_all(&main).ok();
    }

    /// Bypass 1: a command substitution as the program runs whatever it
    /// prints. Only a plain `ssh-agent` substitution under `eval` is kept.
    #[test]
    fn round3_substituted_programs_are_refused() {
        let (root, p) = round3("subst");
        assert_refused(
            &p,
            &root,
            &[
                "$(printf %s%s r m) a.txt",
                "`printf %s%s r m` a.txt",
                "\"$(printf %s%s r m)\" a.txt",
                "eval \"$(echo cm0gYS50eHQ= | base64 -d)\"",
                "eval `echo cm0gYS50eHQ= | base64 -d`",
                "bash -c \"$(cat /tmp/s)\"",
                "sh -c \"`cat /tmp/s`\"",
                "eval \"$(ssh-agent -s; cat /tmp/s)\"",
                "eval \"$(ssh-agent -s)$(cat /tmp/s)\"",
                "eval \"$(ssh-agent $X)\"",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "eval \"$(ssh-agent -s)\"; git push",
                "eval \"$(ssh-agent -s)\" && ssh-add ~/.ssh/k",
                "eval `ssh-agent -s`",
                "eval $(ssh-agent)",
                "echo $(git rev-parse HEAD)",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Bypass 2: a shell reading its script from stdin or `<(…)` is
    /// unreadable whatever else the line names.
    #[test]
    fn round3_encoded_scripts_into_a_shell_are_refused() {
        let (root, p) = round3("stdin");
        assert_refused(
            &p,
            &root,
            &[
                "echo cm0gYS50eHQ= | base64 -d | bash",
                "echo cm0gYS50eHQ= | base64 -d | sh -s",
                "source <(base64 -d <<< cm0gYS50eHQ=)",
                ". <(base64 -d <<< cm0gYS50eHQ=)",
                "bash <(base64 -d <<< cm0gYS50eHQ=)",
                "base64 -d <<< cm0gYS50eHQ= > /tmp/s; bash < /dev/stdin < /tmp/s",
                "bash <<'EOF'\nprintf x\nEOF",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &["bash scripts/check.sh", "source ./env.sh && git status"],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Cheap extras from the round-3 verdict.
    #[test]
    fn round3_extras_are_refused() {
        let (root, p) = round3("extras");
        assert_refused(
            &p,
            &root,
            &[
                "git checkout-index -f a.txt",
                "git checkout-index -f -a",
                "git checkout-index --force --all",
                "git mv -f b.txt a.txt",
                "git restore --pathspec-from-file=/tmp/list",
                "git checkout --pathspec-from-file /tmp/list",
                "git rm --pathspec-from-file=/tmp/list",
                "git stash push --pathspec-from-file=/tmp/list",
                "su -c 'rm a.txt'",
                "su root -c 'git restore a.txt'",
                "script -q -c 'rm a.txt' /dev/null",
                "script -c 'git checkout -- a.txt'",
                "busybox rm a.txt",
                "busybox sh -c 'rm a.txt'",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "git checkout-index -a",
                "git mv b.txt c.txt",
                "git restore --staged --pathspec-from-file=/tmp/list",
                "busybox ls",
                "script -q -c 'cargo test' /dev/null",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The false positives docs/ENGINE.md names: refused with a non-empty
    /// baseline, allowed in a fresh clone.
    #[test]
    fn round3_documented_false_positives() {
        let (root, p) = round3("fp");
        let commands = [
            "$EDITOR f",
            "${CARGO:-cargo} test",
            "\"$(command -v cargo)\" test",
            "git restore src/{a,b}.rs",
            "GIT_EDITOR=vim git commit",
            "git -c core.pager=delta log",
        ];
        assert_refused(&p, &root, &commands);
        let fresh = workspace(&[], Some("worktree"), &root);
        assert_allowed(&fresh, &root, &commands);
        std::fs::remove_dir_all(&root).ok();
    }

    /// WI-4001 round 4, blocker 1: git takes any unique prefix of a long
    /// option, so `--har` is `--hard`.
    #[test]
    fn round4_long_option_prefixes_are_refused() {
        let (root, p) = round3("r4-prefix");
        assert_refused(
            &p,
            &root,
            &[
                "git reset --har",
                "git reset --ha HEAD",
                "git reset --kee",
                "git checkout --forc HEAD",
                "git checkout --f HEAD",
                "git switch --discard master",
                "git switch --forc master",
                "git checkout-index --for --all",
                "git checkout-index -f --al",
                "git mv --forc b.txt a.txt",
                "git clean --forc a.txt",
                "git stash push --mess x",
                "git restore --pathspec-from=/tmp/l",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "git restore --sta a.txt",
                "git rm --cach a.txt",
                "git reset --mixed",
                "git reset --soft HEAD~1",
                "git clean --dry -f a.txt",
                "git checkout-index --al",
                "git checkout --orph x",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 2: a `-f` inside a short-option cluster.
    #[test]
    fn round4_clustered_force_is_refused() {
        let (root, p) = round3("r4-cluster");
        assert_refused(
            &p,
            &root,
            &[
                "git checkout -qf HEAD",
                "git checkout -fb x",
                "git checkout -fB x",
                "git switch -qf master",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "git checkout -qb x",
                "git checkout -bfix",
                "git switch -cfix",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 3: an option value attached to its letter (`-S'…'`, `-c'…'`).
    #[test]
    fn round4_attached_payloads_are_read() {
        let (root, p) = round3("r4-attached");
        assert_refused(
            &p,
            &root,
            &[
                "env -S'rm a.txt'",
                "env -S'git restore a.txt'",
                "env -iS'rm a.txt'",
                "su -c'rm a.txt'",
                "su -lc'rm a.txt'",
                "runuser -c'git checkout -- a.txt'",
                "script -qc'rm a.txt' /dev/null",
                "su --comm='rm a.txt'",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "env -S'cargo test'",
                "su -c'cargo test'",
                "script -qc'ls' /dev/null",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 4: git configuration and environment that run a command.
    #[test]
    fn round4_command_config_is_refused() {
        let (root, p) = round3("r4-config");
        assert_refused(
            &p,
            &root,
            &[
                "git -c diff.external=./x.sh diff",
                "git -c diff.external='cp b.txt a.txt' diff",
                "GIT_EXTERNAL_DIFF=./x.sh git diff",
                "env GIT_EXTERNAL_DIFF=./x.sh git diff",
                "export GIT_EXTERNAL_DIFF=./x.sh; git diff",
                "git -c core.fsmonitor='cp b.txt a.txt' status",
                "git -c CORE.FSMONITOR=./x status",
                "git grep -O'cp b.txt a.txt' x",
                "git grep -O ./x x",
                "git grep --open-files-in-pager=./x x",
                "git grep --open x",
                "git difftool -y -x ./x.sh",
                "git difftool -yx ./x.sh",
                "git difftool --extcmd=./x.sh",
                "git -c core.pager='sh -c x' log",
                "GIT_PAGER='cp b.txt a.txt' git log",
                "PAGER=./x git log",
                "git -c pager.log=./x log",
                "git -c core.editor=./x commit",
                "GIT_EDITOR=./x git commit",
                "git -c sequence.editor=./x rebase -i HEAD~1",
                "GIT_SEQUENCE_EDITOR=./x git rebase -i HEAD~1",
                "GIT_SSH_COMMAND=./x git fetch",
                "git -c core.sshCommand=./x fetch",
                "git -c credential.helper='!./x' fetch",
                "GIT_ASKPASS=./x git fetch",
                "git -c core.askpass=./x fetch",
                "git -c core.hooksPath=/tmp/h commit -m x",
                "git -c filter.lfs.smudge=./x checkout HEAD -- f",
                "git -c diff.x.textconv=./x diff",
                "git -c merge.x.driver=./x merge y",
                "git -c difftool.x.cmd=./x difftool",
                "git rebase -x ./x HEAD~1",
                "git rebase --exec=./x HEAD~1",
                "git rebase -ix ./x HEAD~1",
                "git bisect run ./x",
                "git submodule foreach ./x",
                "git --exec-path=/tmp/x status",
                "git config core.pager ./x",
                "git config --global diff.external ./x",
                "LESSOPEN='|./x %s' git log",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "GIT_PAGER=cat git log",
                "GIT_PAGER= git log",
                "git -c core.pager=less log",
                "git -c core.pager='less -FRX' log",
                "GIT_EDITOR=true git commit --amend",
                "GIT_SEQUENCE_EDITOR=: git rebase -i HEAD~2",
                "git -c pager.log=false log",
                "GIT_SSH_COMMAND='ssh -i ~/.ssh/k -o IdentitiesOnly=yes' git fetch",
                "git -c core.hooksPath=/dev/null commit -m x",
                "git grep -n TODO",
                "git difftool -y",
                "git config core.pager cat",
                "git config --get core.pager",
                "git --exec-path",
                "git -c color.ui=never log -1",
                "git commit -m 'fix GIT_PAGER handling'",
                "echo $GIT_EDITOR; git status",
                "unset GIT_PAGER; git log",
            ],
        );
        assert_refused(
            &p,
            &root,
            &[
                "export GIT_EXTERNAL_DIFF; git diff",
                "read GIT_PAGER < /tmp/p; git log",
                "printf -v GIT_PAGER %s ./x; git log",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 4, protected-checkout half: a config that runs a command
    /// makes even a read a possible write.
    #[test]
    fn round4_command_config_does_not_escape_the_protected_checkout_rule() {
        let (main, wt) = checkout_and_worktree("round4-config-pc");
        let mut p = workspace(&[], Some("worktree"), &wt);
        p.protected_checkouts = vec![main.display().to_string()];
        let c = main.display();
        for command in [
            format!("git -C {c} -c core.fsmonitor=./x status"),
            format!("GIT_EXTERNAL_DIFF=./x git -C {c} diff"),
            format!("git -C {c} grep -O./x y"),
        ] {
            let reason = check(&p, &command, &wt).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("protected checkout"), "{command}: {reason}");
        }
        assert_eq!(
            check(&p, &format!("GIT_PAGER=cat git -C {c} log -1"), &wt),
            None
        );
        std::fs::remove_dir_all(&wt).ok();
        std::fs::remove_dir_all(&main).ok();
    }

    /// Blocker 5: `read-tree -u` with `--reset`/`-m`, and `checkout -m <path>`.
    #[test]
    fn round4_read_tree_and_checkout_merge_are_refused() {
        let (root, p) = round3("r4-readtree");
        assert_refused(
            &p,
            &root,
            &[
                "git read-tree --reset -u HEAD",
                "git read-tree --res -u HEAD",
                "git read-tree -mu HEAD",
                "git read-tree -m -u HEAD",
                "git read-tree -u --reset HEAD",
                "git checkout -m a.txt",
                "git checkout -m -- a.txt",
                "git checkout -qm a.txt",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "git read-tree HEAD",
                "git read-tree -m HEAD",
                "git read-tree -n -mu HEAD",
                "git checkout -m mine.txt",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// WI-4001 round 5, blocker 1: git takes any unique prefix of a long
    /// option, so `--ex` is `--exec`; an ambiguous one (`--e`) is an error.
    #[test]
    fn round5_exec_prefixes_are_refused() {
        let (root, p) = round3("r5-exec");
        assert_refused(
            &p,
            &root,
            &[
                "git rebase --ex='echo X>a.txt' HEAD~1",
                "git rebase --autostash --ex='echo X>a.txt' HEAD~1",
                "git rebase --ex 'echo X>a.txt' HEAD~1",
                "git difftool --ext=./x.sh",
                "git grep --ope x",
            ],
        );
        assert_allowed(&p, &root, &["git rebase --onto main HEAD~1", "git rebase -i HEAD~2"]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 2: a later `--no-<long>` takes back an earlier short safe
    /// flag; the last one wins.
    #[test]
    fn round5_negated_safe_flags_are_refused() {
        let (root, p) = round3("r5-neg");
        assert_refused(
            &p,
            &root,
            &[
                "git clean -fn --no-dry-run",
                "git clean -n -f --no-dry-run a.txt",
                "git clean -fn --no-dry a.txt",
                "git read-tree --reset -u -n --no-dry-run HEAD",
                "git restore -S --no-staged a.txt",
                "git rm --cached --no-cached a.txt",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "git clean -fn",
                "git clean --no-dry-run -fn",
                "git read-tree --reset -u --no-dry-run -n HEAD",
                "git restore --no-staged -S a.txt",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 3: the environment a command runs with is read from its
    /// words after quote removal, not from the text.
    #[test]
    fn round5_quoted_command_env_is_refused() {
        let (root, p) = round3("r5-env");
        assert_refused(
            &p,
            &root,
            &[
                "export GIT_PAG\"\"ER='sh -c \"echo P>a.txt\"'; git -p log",
                "export GIT_PAG\\ER='sh -c \"echo P>a.txt\"'; git -p log",
                "X=GIT_PAGER; export $X='sh -c x'; git -p log",
                "export 'GIT_EXTERNAL_DIFF'='sh -c x'; git diff",
                "GIT_EXTERNAL_DIFF+='sh -c x' git diff",
                "export GIT_EXTERNAL_DIFF+=x; git diff",
                "declare -x \"GIT_PAGER=sh -c x\"; git -p log",
                "declare -n R=GIT_PAGER; R=./x; git -p log",
                "env -u X GIT_PAGER=./x git log",
                "eval 'export GIT_PAGER=./x'; git log",
                "bash -c 'GIT_PAGER=./x git log'",
                "export GIT_CONF\"\"IG_COUNT=1 GIT_CONFIG_KEY_0=alias.r GIT_CONFIG_VALUE_0=restore; git r a.txt",
                "read -r $N; git log",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 4: a `-c` key that is not a literal word.
    #[test]
    fn round5_non_literal_config_keys_are_refused() {
        let (root, p) = round3("r5-key");
        assert_refused(
            &p,
            &root,
            &[
                "K=diff.external; git -c $K='sh -c x' diff",
                "K=alias.r; git -c $K=restore r a.txt",
                "K=alias.r; git -c \"$K\"=restore r a.txt",
                "git -c `echo alias.r`=restore r a.txt",
                "K=alias.r; git --config-env=$K=V r a.txt",
                "K=alias.r; git config $K restore; git r a.txt",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Blocker 5: `-c`, `--config-env` and `git config` take only keys on an
    /// allowlist; a pager is harmless only without an output file; `LESS`
    /// and `LESSKEY*` are command environment.
    #[test]
    fn round5_config_allowlist() {
        let (root, p) = round3("r5-allow");
        assert_refused(
            &p,
            &root,
            &[
                "GIT_PAGER='less -Oa.txt' git -p log",
                "PAGER='less -oa.txt' git -p log",
                "git -c core.pager='less -Oa.txt' -p log",
                "git -c core.pager='less --log-file=a.txt' -p log",
                "git -c core.pager='less -RO a.txt' -p log",
                "git -c core.pager='less --lesskey-file=k' -p log",
                "LESS=-Oa.txt git -p log",
                "LESSKEYIN=k git -p log",
                "git -c trailer.x.cmd='sh -c x' interpret-trailers --trailer x=1",
                "git -c trailer.x.command=./x commit --trailer x=1 -m y",
                "git -c man.x.cmd=./x help -m git",
                "git -c gpg.ssh.defaultKeyCommand=./x commit -S -m y",
                "git -c protocol.ext.allow=always fetch 'ext::sh -c rm% a.txt'",
                "git -c help.autocorrect=immediate restor a.txt",
                "git -c remote.origin.vcs=x fetch",
                "git -c core.alternateRefsCommand=./x fetch",
                "git --config-env=core.pager=P log",
                "git config protocol.ext.allow always",
                "git config --global trailer.x.cmd ./x",
            ],
        );
        assert_allowed(
            &p,
            &root,
            &[
                "git -c core.quotepath=off status",
                "git -c color.ui=never log -1",
                "git -c color.diff.meta=blue diff",
                "git -c user.name=x -c user.email=y@z commit -m m",
                "git -c commit.gpgsign=false commit -m m",
                "git -c advice.detachedHead=false log",
                "git -c safe.directory=/x status",
                "git -c pull.rebase=true pull",
                "git -c push.default=current push",
                "git -c diff.noprefix=true diff",
                "git -c log.showSignature=false log",
                "git -c core.pager=cat log",
                "git -c core.pager='less -FRX' log",
                "git -c pager.log=false log",
                "git -c alias.st=status st",
                "LESS=-FRX git log",
                "LESS='-R -S' git log",
                "git config user.email x@y",
                "git config core.pager cat",
                "git config --get protocol.ext.allow",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Non-blocking from the round 5 brief: a commit message or format
    /// string that mentions a command variable sets nothing; a redirect into
    /// `.git/config` or `.git/hooks/` is refused.
    #[test]
    fn round5_mentions_pass_and_git_dir_writes_are_refused() {
        let (root, p) = round3("r5-misc");
        assert_allowed(
            &p,
            &root,
            &[
                "git commit -m \"mention EDITOR=vim\"",
                "git commit -F - <<'X'\nfix: refuse GIT_PAGER=evil\nX",
                "git commit -F - <<X\nfix: refuse GIT_PAGER=evil\nX",
                "git log --format='%H EDITOR=x'",
                "echo GIT_PAGER=x",
                "cat .git/config",
                "cd engine && cargo test",
                "git -C /tmp log",
                "eval \"$(ssh-agent -s)\"",
                "git restore --staged a.txt",
                "git push",
            ],
        );
        assert_refused(
            &p,
            &root,
            &[
                "printf '[core]\\n\\tpager=./x\\n' >> .git/config; git log",
                "echo x > .git/hooks/pre-commit",
                "echo x >.git/hooks/pre-commit; chmod +x .git/hooks/pre-commit",
                "tee .git/hooks/post-checkout < /tmp/h",
                "cp /tmp/h .git/hooks/pre-commit",
                "echo x 2>&1 >> ./.git/config",
            ],
        );
        let fresh = workspace(&[], Some("worktree"), &root);
        assert_allowed(
            &fresh,
            &root,
            &[
                "git -c core.pager=delta log",
                "LESS=-Oa.txt git -p log",
                "git commit -m \"mention EDITOR=vim\"",
            ],
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
