//! Checkpoint a session pod's unlanded work as a graft draft (WI-3370,
//! WI-4267).
//!
//! A checkpoint brings a scratch working copy under `/save` to "the line base
//! plus everything the session has not landed": the paths its unlanded stack
//! changed and its uncommitted edits, flattened. Then it runs `graft commit`
//! (`--amend` after the first) in that copy's own save workspace,
//! `vogt-save-<session>-<gen>`, and renews the lease to 24 h. The save
//! workspace never mixes with the agent's own stack, so a later `g land` in
//! `/work` never lands half-done work.
//!
//! A draft is readable by every holder of the repository's `read` scope, so
//! the copy into `/save` is the security boundary: graft only ever sees what
//! the filter in [`Saver::classify`] let through. Secret-named files,
//! hardlinked files, symlinks that leave the checkout, build directories,
//! files over the caps, and files whose bytes look like a credential stay
//! out. A secret-named or refused tracked file keeps its base content in
//! `/save`, so a skip never turns into a delete.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CString, OsStr},
    fs,
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use regex::bytes::RegexSet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vogt_engine_contract::{
    salvage::{is_secret_path, SalvageSkip, SkipReason},
    saved_work::{
        excluded_dir_prefix, save_workspace_name, SaveTrigger, SavedWorkReport, DEFAULT_SAVE_DIR,
        DEFAULT_SAVE_INTERVAL_SECS, DEFAULT_SAVE_MAX_BYTES, DEFAULT_SAVE_MAX_FILE_BYTES,
        ENV_POD_GEN, ENV_SAVE, ENV_SAVE_DIR, ENV_SAVE_INTERVAL, ENV_SAVE_MAX_BYTES,
        ENV_SAVE_MAX_FILE_BYTES, SAVE_LEASE_SECS, TOKEN_SHAPE_PATTERNS,
    },
};

use crate::salvage::safe_relative;

/// How often an unchanged save still renews its lease, so a long-lived pod
/// whose tree stops changing never lets the draft lapse.
const RENEW_EVERY_SECS: u64 = 6 * 3600;
/// Changed paths asked of `graft diff` per page.
const DIFF_PAGE: &str = "1000";
/// The most `graft diff` pages read for one stack.
const MAX_DIFF_PAGES: usize = 200;
/// Shortest known secret (a token file's content) worth matching literally.
const MIN_KNOWN_SECRET: usize = 16;

/// The pod's save settings, from the environment the engine gives the pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSettings {
    /// `ENGINE_POD_SAVE`; on unless set to `0`, `false`, `no` or `off`.
    pub enabled: bool,
    /// `ENGINE_POD_SAVE_INTERVAL`; `None` turns the timer off.
    pub interval: Option<Duration>,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub save_dir: PathBuf,
    pub gen: u64,
}

impl Default for SaveSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Some(Duration::from_secs(DEFAULT_SAVE_INTERVAL_SECS)),
            max_file_bytes: DEFAULT_SAVE_MAX_FILE_BYTES,
            max_total_bytes: DEFAULT_SAVE_MAX_BYTES,
            save_dir: PathBuf::from(DEFAULT_SAVE_DIR),
            gen: 0,
        }
    }
}

impl SaveSettings {
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let number = |name: &str| get(name).and_then(|v| v.parse::<u64>().ok());
        let defaults = Self::default();
        Self {
            enabled: !get(ENV_SAVE)
                .is_some_and(|v| matches!(v.as_str(), "0" | "false" | "no" | "off")),
            interval: match number(ENV_SAVE_INTERVAL) {
                Some(0) => None,
                Some(secs) => Some(Duration::from_secs(secs)),
                None => defaults.interval,
            },
            max_file_bytes: number(ENV_SAVE_MAX_FILE_BYTES)
                .filter(|n| *n > 0)
                .unwrap_or(defaults.max_file_bytes),
            max_total_bytes: number(ENV_SAVE_MAX_BYTES)
                .filter(|n| *n > 0)
                .unwrap_or(defaults.max_total_bytes),
            save_dir: get(ENV_SAVE_DIR)
                .map(PathBuf::from)
                .unwrap_or(defaults.save_dir),
            gen: number(ENV_POD_GEN).unwrap_or(0),
        }
    }
}

/// How the agent runs graft: the program, the token file and the extra
/// environment (the session ids graft attributes a draft to).
#[derive(Debug, Clone)]
pub struct GraftCli {
    pub program: PathBuf,
    pub token_file: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl GraftCli {
    fn run(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(&self.program);
        if let Some(token) = &self.token_file {
            command.arg("--token-file").arg(token);
        }
        command.arg("-C").arg(dir).args(args).current_dir(dir);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        let out = command
            .output()
            .map_err(|e| format!("could not run graft: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let detail = if stderr.trim().is_empty() {
                stdout.trim().to_owned()
            } else {
                stderr.trim().to_owned()
            };
            return Err(format!(
                "graft {} failed: {}",
                args.first().copied().unwrap_or(""),
                detail.chars().take(400).collect::<String>()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// What one checkpoint did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkpoint {
    /// A new report: the draft was written, or the tree is clean.
    Saved(SavedWorkReport),
    /// Nothing changed since the last report, which is returned again.
    Unchanged(SavedWorkReport),
    /// The clone root is not a graft checkout; nothing is saved.
    Unsupported(String),
}

/// What a path in the changed set becomes in `/save`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    File { bytes: Vec<u8>, exec: bool },
    Link(PathBuf),
    Absent,
}

/// What the scratch copy holds for one path the save once changed: the base
/// content was copied aside to `orig/` before the first write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Original {
    present: bool,
}

/// What survives an agent restart inside the same pod, in `/save/state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SaveState {
    gen: u64,
    seq: u64,
    fingerprint: Option<String>,
    repo: Option<String>,
    line: Option<String>,
    base: Option<String>,
    workspace_open: bool,
    originals: BTreeMap<String, Original>,
    last: Option<SavedWorkReport>,
    renewed_at: u64,
}

/// `graft status --json`, the fields a save reads.
#[derive(Debug, Deserialize)]
struct Status {
    #[serde(default)]
    line: Option<String>,
    base: String,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    changes: Vec<StatusChange>,
}

#[derive(Debug, Deserialize)]
struct StatusChange {
    path: String,
}

/// One page of `graft diff --json FROM TO`.
#[derive(Debug, Deserialize)]
struct DiffPage {
    #[serde(default)]
    files: Vec<StatusChange>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Committed {
    id: String,
}

/// Saves one checkout's unlanded work. One per pod.
pub struct Saver {
    root: PathBuf,
    save_dir: PathBuf,
    session: String,
    gen: u64,
    max_file_bytes: u64,
    max_total_bytes: u64,
    graft: GraftCli,
    shapes: RegexSet,
    /// Paths below the clone root that are never read (a token directory
    /// that happens to sit inside it), relative to the root.
    never_walk: Vec<String>,
    state: SaveState,
}

impl Saver {
    /// A saver for the checkout at `root`. `known_secrets` are the pod's own
    /// credentials (its token files' contents), refused wherever they appear
    /// in a file; `never_walk` are absolute directories never read.
    pub fn new(
        root: PathBuf,
        settings: &SaveSettings,
        session: &str,
        graft: GraftCli,
        known_secrets: &[Vec<u8>],
        never_walk: &[PathBuf],
    ) -> Result<Self, String> {
        let mut patterns: Vec<String> = TOKEN_SHAPE_PATTERNS
            .iter()
            .map(|p| (*p).to_owned())
            .collect();
        for secret in known_secrets {
            let trimmed = secret.trim_ascii();
            if trimmed.len() >= MIN_KNOWN_SECRET {
                patterns.push(regex::escape(&String::from_utf8_lossy(trimmed)));
            }
        }
        let shapes =
            RegexSet::new(&patterns).map_err(|e| format!("token shapes do not compile: {e}"))?;
        let never_walk = never_walk
            .iter()
            .filter_map(|dir| dir.strip_prefix(&root).ok())
            .map(|rel| rel.to_string_lossy().trim_matches('/').to_owned())
            .collect();
        let state = load_state(&settings.save_dir)
            .filter(|s| s.gen == settings.gen)
            .unwrap_or(SaveState {
                gen: settings.gen,
                ..SaveState::default()
            });
        Ok(Self {
            root,
            save_dir: settings.save_dir.clone(),
            session: session.to_owned(),
            gen: settings.gen,
            max_file_bytes: settings.max_file_bytes,
            max_total_bytes: settings.max_total_bytes,
            graft,
            shapes,
            never_walk,
            state,
        })
    }

    /// The save workspace's name.
    pub fn workspace(&self) -> String {
        save_workspace_name(&self.session, self.gen)
    }

    fn tree(&self) -> PathBuf {
        self.save_dir.join("tree")
    }

    fn originals_dir(&self) -> PathBuf {
        self.save_dir.join("orig")
    }

    /// Save the checkout's unlanded work. An unchanged tree (same changed
    /// paths, sizes, times, modes and stack head) makes no graft write.
    pub fn checkpoint(&mut self, trigger: SaveTrigger) -> Result<Checkpoint, String> {
        if !self.root.join(".graft").is_dir() {
            return Ok(Checkpoint::Unsupported(
                "the clone root is not a graft checkout".into(),
            ));
        }
        let repo = read_repo(&self.root)?;
        let status: Status =
            serde_json::from_str(&self.graft.run(&self.root, &["status", "--json"])?)
                .map_err(|e| format!("graft status returned invalid JSON: {e}"))?;
        let line = status.line.clone().ok_or("graft status named no line")?;
        let mut changed: BTreeSet<String> = status.changes.into_iter().map(|c| c.path).collect();
        if let Some(head) = status.head.as_deref().filter(|h| *h != status.base) {
            changed.extend(self.stack_paths(&status.base, head)?);
        }
        let fingerprint = self.fingerprint(&status.base, status.head.as_deref(), &changed);
        if let Some(last) = self.state.last.clone() {
            if self.state.fingerprint.as_deref() == Some(fingerprint.as_str()) {
                if last.draft.is_some()
                    && now_secs().saturating_sub(self.state.renewed_at) >= RENEW_EVERY_SECS
                {
                    self.renew()?;
                    self.persist();
                }
                return Ok(Checkpoint::Unchanged(last));
            }
        }
        let (draft, files, bytes, skipped) = if changed.is_empty() {
            (None, 0, 0, Vec::new())
        } else {
            self.save(&repo, &line, &status.base, &changed, trigger)?
        };
        self.state.seq += 1;
        let report = SavedWorkReport {
            provider: "graft".into(),
            repo,
            line,
            workspace: self.workspace(),
            draft,
            base: status.base,
            seq: self.state.seq,
            gen: self.gen,
            at: rfc3339(SystemTime::now()),
            files,
            bytes,
            complete: SavedWorkReport::complete_with(&skipped),
            skipped,
            trigger,
            previous: None,
        };
        self.state.fingerprint = Some(fingerprint);
        self.state.last = Some(report.clone());
        self.persist();
        Ok(Checkpoint::Saved(report))
    }

    /// The paths the unlanded stack (`base..head`) changed.
    fn stack_paths(&self, base: &str, head: &str) -> Result<Vec<String>, String> {
        let mut paths = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_DIFF_PAGES {
            let mut args = vec!["diff", "--json", "--limit", DIFF_PAGE];
            if let Some(c) = cursor.as_deref() {
                args.extend(["--cursor", c]);
            }
            args.extend([base, head]);
            let page: DiffPage = serde_json::from_str(&self.graft.run(&self.root, &args)?)
                .map_err(|e| format!("graft diff returned invalid JSON: {e}"))?;
            paths.extend(page.files.into_iter().map(|f| f.path));
            match page.next {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => return Ok(paths),
            }
        }
        Err("the unlanded stack changes too many paths to list".into())
    }

    /// A digest of what a checkpoint would read: the stack, and each changed
    /// path's size, mtime, mode, inode and link count, never its bytes.
    fn fingerprint(&self, base: &str, head: Option<&str>, changed: &BTreeSet<String>) -> String {
        let mut hash = Sha256::new();
        hash.update(format!(
            "gen {} base {base} head {} caps {} {}\n",
            self.gen,
            head.unwrap_or("-"),
            self.max_file_bytes,
            self.max_total_bytes
        ));
        for path in changed {
            hash.update(path.as_bytes());
            match fs::symlink_metadata(self.root.join(path)) {
                Ok(m) => hash.update(format!(
                    " {} {} {} {} {}\n",
                    m.len(),
                    m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
                    m.mode(),
                    m.ino(),
                    m.nlink()
                )),
                Err(_) => hash.update(" absent\n"),
            }
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Bring `/save` to base plus the allowed changes and commit it there.
    fn save(
        &mut self,
        repo: &str,
        line: &str,
        base: &str,
        changed: &BTreeSet<String>,
        trigger: SaveTrigger,
    ) -> Result<(Option<String>, u64, u64, Vec<SalvageSkip>), String> {
        self.prepare_tree(repo, line, base)?;
        let mut budget = self.max_total_bytes;
        let mut skipped = Vec::new();
        let mut excluded = BTreeSet::new();
        let mut restore = Vec::new();
        let mut deletes = Vec::new();
        let mut writes = Vec::new();
        // A path an earlier save changed that is no longer changed (or is now
        // refused) goes back to its base content.
        let candidates: BTreeSet<String> = changed
            .iter()
            .chain(self.state.originals.keys())
            .cloned()
            .collect();
        for path in &candidates {
            if !changed.contains(path) {
                restore.push(path.clone());
                continue;
            }
            match self.classify(path, &mut budget) {
                Ok(Entry::Absent) => deletes.push(path.clone()),
                Ok(entry) => writes.push((path.clone(), entry)),
                Err(skip) => {
                    if skip.reason == SkipReason::Ignored {
                        excluded.insert(skip.path);
                    } else {
                        skipped.push(skip);
                    }
                    restore.push(path.clone());
                }
            }
        }
        let tree = self.tree();
        for path in &restore {
            self.restore_base(path)
                .map_err(|e| format!("could not restore {path} in the save copy: {e}"))?;
        }
        // Deepest first, so a directory emptied by its deletes can then be
        // replaced by a file of the same name.
        deletes.sort_by(|a, b| b.cmp(a));
        let mut files = 0u64;
        let mut bytes = 0u64;
        for path in &deletes {
            if let Err(e) = self.snapshot(path).and_then(|()| remove_entry(&tree, path)) {
                skipped.push(SalvageSkip {
                    path: path.clone(),
                    reason: SkipReason::Unreadable,
                });
                tracing::warn!(%path, error = %e, "session-agent: a deleted path could not be saved");
                continue;
            }
            files += 1;
        }
        for (path, entry) in &writes {
            if let Err(e) = self
                .snapshot(path)
                .and_then(|()| write_entry(&tree, path, entry))
            {
                // A path the save copy cannot hold (its parent is a symlink in
                // the base) is listed, never written through the link.
                skipped.push(SalvageSkip {
                    path: path.clone(),
                    reason: SkipReason::Unreadable,
                });
                tracing::warn!(%path, error = %e, "session-agent: a changed path could not be saved");
                let _ = self.restore_base(path);
                continue;
            }
            files += 1;
            if let Entry::File { bytes: b, .. } = entry {
                bytes += b.len() as u64;
            }
        }
        for prefix in excluded {
            skipped.push(SalvageSkip {
                path: format!("{prefix}/"),
                reason: SkipReason::Ignored,
            });
        }
        skipped.sort_by(|a, b| a.path.cmp(&b.path));
        let draft = self.commit(trigger)?;
        if draft.is_some() {
            self.renew()?;
        }
        Ok((draft, files, bytes, skipped))
    }

    /// Clone `/save` at `base` the first time it is needed, or again when the
    /// checkout's base moved (a `sync`). A save workspace at an old base is
    /// closed first so its name can be reopened; its last draft stays
    /// readable by id as the fallback.
    fn prepare_tree(&mut self, repo: &str, line: &str, base: &str) -> Result<(), String> {
        let tree = self.tree();
        let ready = tree.join(".graft").is_dir()
            && self.state.workspace_open
            && self.state.repo.as_deref() == Some(repo)
            && self.state.line.as_deref() == Some(line)
            && self.state.base.as_deref() == Some(base);
        if ready {
            return Ok(());
        }
        if tree.join(".graft").is_dir() && self.state.workspace_open {
            if let Err(e) = self.graft.run(&tree, &["workspace", "close"]) {
                tracing::warn!(error = %e, "session-agent: could not close the old save workspace");
            }
        }
        self.state.workspace_open = false;
        self.state.originals.clear();
        self.state.base = None;
        for dir in [tree.clone(), self.originals_dir()] {
            match fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("could not clear {}: {e}", dir.display())),
            }
        }
        fs::create_dir_all(&self.save_dir)
            .and_then(|()| fs::set_permissions(&self.save_dir, fs::Permissions::from_mode(0o700)))
            .map_err(|e| format!("could not prepare {}: {e}", self.save_dir.display()))?;
        let tree_arg = tree.to_string_lossy().into_owned();
        self.graft.run(
            &self.save_dir,
            &["clone", repo, &tree_arg, "--line", line, "--at", base],
        )?;
        let workspace = self.workspace();
        if let Err(first) = self.graft.run(&tree, &["workspace", "open", &workspace]) {
            // The name is still leased from an earlier copy this pod lost
            // track of: close it and open it again at this base.
            let _ = self.graft.run(&tree, &["workspace", "close", &workspace]);
            self.graft
                .run(&tree, &["workspace", "open", &workspace])
                .map_err(|e| format!("{e} (after: {first})"))?;
        }
        self.state.repo = Some(repo.to_owned());
        self.state.line = Some(line.to_owned());
        self.state.base = Some(base.to_owned());
        self.state.workspace_open = true;
        self.persist();
        Ok(())
    }

    /// Decide what `path` becomes in `/save`. An `Err` is the skip to report;
    /// [`SkipReason::Ignored`] carries the excluded directory, not the path.
    fn classify(&self, path: &str, budget: &mut u64) -> Result<Entry, SalvageSkip> {
        let skip = |reason| SalvageSkip {
            path: path.to_owned(),
            reason,
        };
        if !safe_relative(path) {
            return Err(skip(SkipReason::Secret));
        }
        if let Some(prefix) = excluded_dir_prefix(path) {
            return Err(SalvageSkip {
                path: prefix,
                reason: SkipReason::Ignored,
            });
        }
        if self
            .never_walk
            .iter()
            .any(|dir| dir.is_empty() || path == dir || path.starts_with(&format!("{dir}/")))
        {
            return Err(skip(SkipReason::Secret));
        }
        if is_secret_path(path) {
            return Err(skip(SkipReason::Secret));
        }
        let limit = self.max_file_bytes.min(*budget);
        match inspect(&self.root, Path::new(path), limit) {
            Ok(Inspected::Absent) => Ok(Entry::Absent),
            Ok(Inspected::Link(target)) => {
                if link_escapes(path, &target)
                    || self.shapes.is_match(target.as_os_str().as_bytes())
                {
                    Err(skip(SkipReason::Secret))
                } else {
                    Ok(Entry::Link(target))
                }
            }
            Ok(Inspected::Hardlinked) => Err(skip(SkipReason::Secret)),
            Ok(Inspected::TooLarge) => Err(skip(SkipReason::TooLarge)),
            Ok(Inspected::File { bytes, exec }) => {
                if self.shapes.is_match(&bytes) {
                    return Err(skip(SkipReason::Secret));
                }
                *budget -= bytes.len() as u64;
                Ok(Entry::File { bytes, exec })
            }
            Ok(Inspected::Other) | Err(_) => Err(skip(SkipReason::Unreadable)),
        }
    }

    /// Before `/save` first changes `path`, copy its base content aside.
    fn snapshot(&mut self, path: &str) -> io::Result<()> {
        if self.state.originals.contains_key(path) {
            return Ok(());
        }
        let tree = self.tree();
        check_parents(&tree, path)?;
        let from = tree.join(path);
        let present = match fs::symlink_metadata(&from) {
            Ok(meta) if meta.is_dir() => false,
            Ok(meta) => {
                let to = self.originals_dir().join(path);
                fs::create_dir_all(to.parent().unwrap_or(&self.originals_dir()))?;
                remove_any(&to)?;
                if meta.file_type().is_symlink() {
                    std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
                } else {
                    fs::copy(&from, &to)?;
                    fs::set_permissions(&to, meta.permissions())?;
                }
                true
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        };
        self.state
            .originals
            .insert(path.to_owned(), Original { present });
        Ok(())
    }

    /// Put `path` back to its base content in `/save`, if a save changed it.
    fn restore_base(&mut self, path: &str) -> io::Result<()> {
        let Some(original) = self.state.originals.get(path).copied() else {
            return Ok(());
        };
        let tree = self.tree();
        remove_entry(&tree, path)?;
        if original.present {
            let from = self.originals_dir().join(path);
            check_parents(&tree, path)?;
            let to = tree.join(path);
            let meta = fs::symlink_metadata(&from)?;
            if meta.file_type().is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
            } else {
                fs::copy(&from, &to)?;
                fs::set_permissions(&to, meta.permissions())?;
            }
            remove_any(&from)?;
        }
        self.state.originals.remove(path);
        Ok(())
    }

    /// Commit `/save` into the save workspace: amend its draft when it has
    /// one. The answer is the draft now holding the work, or `None` when the
    /// copy is back at its base.
    fn commit(&mut self, trigger: SaveTrigger) -> Result<Option<String>, String> {
        let tree = self.tree();
        let status: Status =
            serde_json::from_str(&self.graft.run(&tree, &["status", "--json"])?)
                .map_err(|e| format!("graft status in the save copy returned invalid JSON: {e}"))?;
        if status.changes.is_empty() {
            return Ok(status.head);
        }
        let seq = format!("vogt.save_seq={}", self.state.seq + 1);
        let trigger_meta = format!("vogt.trigger={}", trigger.as_str());
        let message = format!("vogt: unlanded work of session {}", self.session);
        let mut args = vec!["commit", "--json"];
        if status.head.is_some() {
            args.push("--amend");
        }
        args.extend([
            "-m",
            &message,
            "--meta",
            "vogt.saved=unlanded",
            "--meta",
            &seq,
            "--meta",
            &trigger_meta,
        ]);
        match self.graft.run(&tree, &args) {
            Ok(out) => serde_json::from_str::<Committed>(&out)
                .map(|c| Some(c.id))
                .map_err(|e| format!("graft commit returned invalid JSON: {e}")),
            // An amend back to the base leaves nothing to keep.
            Err(e) if status.head.is_some() && e.contains("nothing to commit") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn renew(&mut self) -> Result<(), String> {
        let lease = SAVE_LEASE_SECS.to_string();
        self.graft
            .run(&self.tree(), &["workspace", "renew", "--lease", &lease])?;
        self.state.renewed_at = now_secs();
        Ok(())
    }

    fn persist(&self) {
        let path = self.save_dir.join("state.json");
        let tmp = self.save_dir.join(".state.json.tmp");
        let Ok(text) = serde_json::to_vec(&self.state) else {
            return;
        };
        if fs::create_dir_all(&self.save_dir).is_err() {
            return;
        }
        let written = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| io::Write::write_all(&mut f, &text))
            .and_then(|()| fs::rename(&tmp, &path));
        if let Err(e) = written {
            tracing::warn!(error = %e, "session-agent: could not keep the save state");
        }
    }
}

fn load_state(save_dir: &Path) -> Option<SaveState> {
    serde_json::from_slice(&fs::read(save_dir.join("state.json")).ok()?).ok()
}

/// The repository name in the checkout's `.graft/config`.
fn read_repo(root: &Path) -> Result<String, String> {
    #[derive(Deserialize)]
    struct WorkingCopyConfig {
        repo: String,
    }
    let text = fs::read_to_string(root.join(".graft/config"))
        .map_err(|e| format!("could not read the checkout's graft config: {e}"))?;
    let config: WorkingCopyConfig = toml::from_str(&text)
        .map_err(|e| format!("the checkout's graft config is invalid: {e}"))?;
    Ok(config.repo)
}

/// What a path in `/work` is, read without following any symlink.
#[derive(Debug, PartialEq, Eq)]
enum Inspected {
    Absent,
    File {
        bytes: Vec<u8>,
        exec: bool,
    },
    Link(PathBuf),
    /// A regular file with more than one link: refused, as it may be a
    /// credential hardlinked under an innocent name.
    Hardlinked,
    TooLarge,
    /// A FIFO, socket, device or directory.
    Other,
}

/// Look at `relative` below `root`: every parent is opened with
/// `O_NOFOLLOW`, the entry itself is examined with `fstatat` without
/// following it, and a regular file is opened `O_NOFOLLOW` and checked again
/// on the descriptor before a bounded read. A parent that is missing, not a
/// directory or a symlink means the path does not exist as a file in the
/// checkout.
fn inspect(root: &Path, relative: &Path, max_bytes: u64) -> io::Result<Inspected> {
    let components: Vec<&OsStr> = relative
        .components()
        .map(|c| match c {
            Component::Normal(name) => Ok(name),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "non-relative save path",
            )),
        })
        .collect::<io::Result<_>>()?;
    let Some((name, parents)) = components.split_last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty save path",
        ));
    };
    let mut dir = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    for parent in parents {
        match openat(&dir, parent, libc::O_DIRECTORY) {
            Ok(next) => dir = next,
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP)
                ) =>
            {
                return Ok(Inspected::Absent)
            }
            Err(e) => return Err(e),
        }
    }
    let cname = c_name(name)?;
    let stat = match fstatat(&dir, &cname) {
        Ok(stat) => stat,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(Inspected::Absent),
        Err(e) => return Err(e),
    };
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFLNK => {
            let target = readlinkat(&dir, &cname)?;
            Ok(Inspected::Link(target))
        }
        libc::S_IFREG => {
            if stat.st_nlink > 1 {
                return Ok(Inspected::Hardlinked);
            }
            if stat.st_size as u64 > max_bytes {
                return Ok(Inspected::TooLarge);
            }
            let file = openat(&dir, name, libc::O_NONBLOCK)?;
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Ok(Inspected::Other);
            }
            if meta.nlink() > 1 {
                return Ok(Inspected::Hardlinked);
            }
            let mut bytes = Vec::new();
            file.take(max_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > max_bytes {
                return Ok(Inspected::TooLarge);
            }
            Ok(Inspected::File {
                bytes,
                exec: meta.mode() & 0o100 != 0,
            })
        }
        _ => Ok(Inspected::Other),
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in save path"))
}

fn openat(dir: &fs::File, name: &OsStr, extra: libc::c_int) -> io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    // SAFETY: `dir` is a live directory descriptor and `name` one
    // NUL-terminated component; a returned descriptor is owned here.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_RDONLY | extra,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor owned by this call.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

fn fstatat(dir: &fs::File, name: &CString) -> io::Result<libc::stat> {
    use std::os::fd::AsRawFd;
    // SAFETY: an all-zero `stat` is a valid value for the kernel to fill.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: valid descriptor, NUL-terminated name and a writable `stat`.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
}

fn readlinkat(dir: &fs::File, name: &CString) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: valid descriptor and name; the buffer is writable for its length.
    let n = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(PathBuf::from(OsStr::from_bytes(&buf)))
}

/// Whether a symlink at `path` with `target` points outside the checkout:
/// an absolute target, or one whose `..` climbs above the root.
fn link_escapes(path: &str, target: &Path) -> bool {
    if target.as_os_str().is_empty() || target.is_absolute() {
        return true;
    }
    let mut depth = path.split('/').count() as i64 - 1;
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    false
}

/// Refuse to write below `tree` through a symlink: every parent of `path`
/// must be a real directory (created when missing).
fn check_parents(tree: &Path, path: &str) -> io::Result<()> {
    let mut at = tree.to_path_buf();
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a parent in the save copy is not a directory",
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&at)?,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Remove a file or link (or an empty directory) at `path`; a missing one is
/// fine.
fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Remove `path` from the save copy without following a link in its parents.
fn remove_entry(tree: &Path, path: &str) -> io::Result<()> {
    let mut at = tree.to_path_buf();
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            // The parent is gone or is not a directory: nothing to remove
            // below it, and nothing is followed.
            _ => return Ok(()),
        }
    }
    remove_any(&tree.join(path))
}

fn write_entry(tree: &Path, path: &str, entry: &Entry) -> io::Result<()> {
    check_parents(tree, path)?;
    let to = tree.join(path);
    remove_any(&to)?;
    match entry {
        Entry::File { bytes, exec } => {
            let mode = if *exec { 0o755 } else { 0o644 };
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&to)?;
            io::Write::write_all(&mut file, bytes)?;
            // The umask may have dropped bits the mode asked for.
            fs::set_permissions(&to, fs::Permissions::from_mode(mode))
        }
        Entry::Link(target) => std::os::unix::fs::symlink(target, &to),
        Entry::Absent => Ok(()),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// RFC 3339 in UTC, to the second.
fn rfc3339(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Lower the calling thread's CPU priority (`nice 10`), so a checkpoint and
/// the graft processes it starts never compete with the PTY.
pub fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscalls on the calling thread's own id.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS as _, tid, 10);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vogt-save-unit-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn saver(root: &Path) -> Saver {
        saver_with(root, &SaveSettings::default(), &[])
    }

    fn saver_with(root: &Path, settings: &SaveSettings, known: &[Vec<u8>]) -> Saver {
        let mut settings = settings.clone();
        settings.save_dir = root.with_extension("save");
        Saver::new(
            root.to_path_buf(),
            &settings,
            "s1",
            GraftCli {
                program: PathBuf::from("/nonexistent/graft"),
                token_file: None,
                env: Vec::new(),
            },
            known,
            &[root.join("config/graft")],
        )
        .unwrap()
    }

    fn classify(saver: &Saver, path: &str) -> Result<Entry, SalvageSkip> {
        let mut budget = saver.max_total_bytes;
        saver.classify(path, &mut budget)
    }

    fn reason(result: Result<Entry, SalvageSkip>) -> Option<SkipReason> {
        result.err().map(|s| s.reason)
    }

    #[test]
    fn settings_read_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let defaults = SaveSettings::from_lookup(env(&[]));
        assert_eq!(defaults, SaveSettings::default());
        assert_eq!(defaults.interval, Some(Duration::from_secs(300)));
        let custom = SaveSettings::from_lookup(env(&[
            ("ENGINE_POD_SAVE", "0"),
            ("ENGINE_POD_SAVE_INTERVAL", "0"),
            ("ENGINE_POD_SAVE_MAX_FILE_BYTES", "10"),
            ("ENGINE_POD_SAVE_MAX_BYTES", "20"),
            ("VOGT_POD_GEN", "4"),
            ("VOGT_SAVE_DIR", "/tmp/s"),
        ]));
        assert!(!custom.enabled);
        assert_eq!(custom.interval, None);
        assert_eq!((custom.max_file_bytes, custom.max_total_bytes), (10, 20));
        assert_eq!(custom.gen, 4);
        assert_eq!(custom.save_dir, PathBuf::from("/tmp/s"));
    }

    #[test]
    fn token_shapes_are_refused_and_ordinary_text_is_not() {
        let root = scratch("shapes");
        let saver = saver_with(
            &root,
            &SaveSettings::default(),
            &[b"0123456789abcdef0123456789abcdef\n".to_vec()],
        );
        let ghp = format!("token = \"ghp_{}\"\n", "A".repeat(36));
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbg==\n".to_owned();
        let jwt = format!("eyJhbGciOi.{}.{}\n", "x".repeat(20), "y".repeat(20));
        let aws = "aws_access_key_id = AKIAABCDEFGHIJKLMNOP\n".to_owned();
        let anthropic = format!("key: sk-ant-{}\n", "b".repeat(30));
        let graft = format!("v4.public.{}\n", "Q".repeat(80));
        let own = "pasted 0123456789abcdef0123456789abcdef here\n".to_owned();
        for (name, body) in [
            ("ghp.txt", ghp),
            ("pem.txt", pem),
            ("jwt.txt", jwt),
            ("aws.txt", aws),
            ("anthropic.txt", anthropic),
            ("graft.txt", graft),
            ("own.txt", own),
        ] {
            fs::write(root.join(name), body).unwrap();
            assert_eq!(
                reason(classify(&saver, name)),
                Some(SkipReason::Secret),
                "{name} should be refused"
            );
        }
        for (name, body) in [
            ("main.rs", "fn main() { let task = \"sk\"; }\n"),
            (
                "Cargo.lock",
                "checksum = \"9f2a0c3e1b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c\"\n",
            ),
            ("cert.md", "-----BEGIN CERTIFICATE-----\n"),
        ] {
            fs::write(root.join(name), body).unwrap();
            assert!(
                matches!(classify(&saver, name), Ok(Entry::File { .. })),
                "{name} should be saved"
            );
        }
    }

    #[test]
    fn secret_names_excluded_dirs_and_the_token_dir_are_refused() {
        let root = scratch("names");
        let saver = saver(&root);
        for name in [
            ".env",
            "deploy/key.pem",
            ".ssh/id_ed25519",
            "config/graft/x",
        ] {
            assert_eq!(
                reason(classify(&saver, name)),
                Some(SkipReason::Secret),
                "{name}"
            );
        }
        for (name, prefix) in [
            ("target/debug/x.o", "target"),
            ("web/node_modules/a.js", "web/node_modules"),
            (".vogt/restored", ".vogt"),
            (".graft/state", ".graft"),
        ] {
            let skip = classify(&saver, name).unwrap_err();
            assert_eq!(
                (skip.path.as_str(), skip.reason),
                (prefix, SkipReason::Ignored)
            );
        }
        assert_eq!(
            reason(classify(&saver, "../escape")),
            Some(SkipReason::Secret)
        );
    }

    #[test]
    fn a_hardlinked_file_is_refused() {
        let root = scratch("hardlink");
        let outside = scratch("hardlink-outside");
        fs::write(outside.join("sa"), "SERVICE ACCOUNT").unwrap();
        fs::hard_link(outside.join("sa"), root.join("notes.txt")).unwrap();
        assert_eq!(
            reason(classify(&saver(&root), "notes.txt")),
            Some(SkipReason::Secret)
        );
    }

    #[test]
    fn symlinks_are_kept_as_links_unless_they_leave_the_checkout() {
        let root = scratch("links");
        fs::create_dir_all(root.join("docs")).unwrap();
        std::os::unix::fs::symlink("../README.md", root.join("docs/readme")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("abs")).unwrap();
        std::os::unix::fs::symlink("../../outside", root.join("docs/up")).unwrap();
        let saver = saver(&root);
        assert_eq!(
            classify(&saver, "docs/readme").unwrap(),
            Entry::Link(PathBuf::from("../README.md"))
        );
        assert_eq!(reason(classify(&saver, "abs")), Some(SkipReason::Secret));
        assert_eq!(
            reason(classify(&saver, "docs/up")),
            Some(SkipReason::Secret)
        );
    }

    #[test]
    fn a_path_through_a_symlinked_directory_is_not_read() {
        let root = scratch("link-parent");
        let outside = scratch("link-parent-outside");
        fs::write(outside.join("innocent.txt"), "CREDENTIAL").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        assert_eq!(
            classify(&saver(&root), "linked/innocent.txt").unwrap(),
            Entry::Absent
        );
    }

    #[test]
    fn a_fifo_is_unreadable() {
        let root = scratch("fifo");
        let name = CString::new(root.join("pipe").as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(
            reason(classify(&saver(&root), "pipe")),
            Some(SkipReason::Unreadable)
        );
    }

    #[test]
    fn the_caps_refuse_large_files_and_count_the_total() {
        let root = scratch("caps");
        fs::write(root.join("big.bin"), vec![b'x'; 64]).unwrap();
        fs::write(root.join("a.bin"), vec![b'a'; 20]).unwrap();
        fs::write(root.join("b.bin"), vec![b'b'; 20]).unwrap();
        let settings = SaveSettings {
            max_file_bytes: 32,
            max_total_bytes: 30,
            ..SaveSettings::default()
        };
        let saver = saver_with(&root, &settings, &[]);
        let mut budget = saver.max_total_bytes;
        assert_eq!(
            reason(saver.classify("big.bin", &mut budget)),
            Some(SkipReason::TooLarge)
        );
        assert!(matches!(
            saver.classify("a.bin", &mut budget),
            Ok(Entry::File { .. })
        ));
        assert_eq!(budget, 10);
        assert_eq!(
            reason(saver.classify("b.bin", &mut budget)),
            Some(SkipReason::TooLarge)
        );
    }

    #[test]
    fn exec_bits_and_deletes_are_seen() {
        let root = scratch("exec");
        fs::write(root.join("run.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let saver = saver(&root);
        assert!(matches!(
            classify(&saver, "run.sh"),
            Ok(Entry::File { exec: true, .. })
        ));
        assert_eq!(classify(&saver, "gone.txt").unwrap(), Entry::Absent);
    }

    #[test]
    fn link_escape_rules() {
        assert!(!link_escapes("a/b", Path::new("c")));
        assert!(!link_escapes("a/b", Path::new("../c")));
        assert!(link_escapes("a/b", Path::new("../../c")));
        assert!(link_escapes("b", Path::new("../c")));
        assert!(link_escapes("b", Path::new("/c")));
        assert!(link_escapes("b", Path::new("")));
        assert!(!link_escapes("a/b/c", Path::new("./../d/../e")));
    }

    #[test]
    fn timestamps_are_rfc3339() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_794_700_800)),
            "2026-11-15T00:00:00Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400 + 3_661)),
            "2000-02-29T01:01:01Z"
        );
    }
}
