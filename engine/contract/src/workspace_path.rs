//! Workspace path confinement and content-exposure rules, shared by the engine's
//! file API (`vogt-server`'s `workspace_path`) and the session agent's FileOp
//! handler (WI-4866), so both sides refuse the same paths.
//!
//! * [`resolve_existing`] canonicalises a path that must exist and asserts it is
//!   still under the root after symlinks are followed.
//! * [`resolve_for_write`] canonicalises the parent and refuses to write through
//!   a symlink at the final component.
//! * [`may_show`] refuses hidden components and credential-looking names on an
//!   already-resolved path.
//!
//! All of them reject `..`, root and prefix components up front. The root is
//! assumed canonical.

use std::path::{Component, Path, PathBuf};

/// A refused or unresolvable path. The message names the rule, never file bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError(pub String);

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PathError {}

pub type Result<T> = std::result::Result<T, PathError>;

fn strip_lexically(root: &Path, requested: &str) -> Result<PathBuf> {
    let rel = requested.trim_start_matches('/');
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(PathError("path contains '..' (parent component)".into()));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathError("path must be relative".into()));
            }
        }
    }
    Ok(out)
}

fn canonicalize_under_root(root: &Path, candidate: &Path, requested: &str) -> Result<PathBuf> {
    let canon = candidate
        .canonicalize()
        .map_err(|e| PathError(format!("path {requested:?}: {e}")))?;
    if !canon.starts_with(root) {
        return Err(PathError("path escapes workspace_root via symlink".into()));
    }
    Ok(canon)
}

/// Resolve a path the client expects to already exist, e.g. a file to read or
/// a directory to list. Follows symlinks and verifies the final path is still
/// under `root`.
pub fn resolve_existing(root: &Path, requested: &str) -> Result<PathBuf> {
    let joined = strip_lexically(root, requested)?;
    canonicalize_under_root(root, &joined, requested)
}

/// Resolve an existing path under the workspace root, preserving compatibility
/// for callers that may supply either a relative workspace path or an absolute
/// path that already points inside the workspace.
pub fn resolve_existing_allow_absolute(root: &Path, requested: &str) -> Result<PathBuf> {
    let requested = requested.trim();
    let path = Path::new(requested);
    if path.is_absolute() {
        canonicalize_under_root(root, path, requested)
    } else {
        resolve_existing(root, requested)
    }
}

/// Resolve a path the client wants to write. Canonicalises the parent — which
/// must exist — then re-joins the final filename component without following
/// it. Rejects writing through a symlink that points outside the workspace.
pub fn resolve_for_write(root: &Path, requested: &str) -> Result<PathBuf> {
    let joined = strip_lexically(root, requested)?;
    let parent = joined
        .parent()
        .ok_or_else(|| PathError("path has no parent".into()))?;
    let file_name = joined
        .file_name()
        .ok_or_else(|| PathError("path has no final component".into()))?;
    let canon_parent = parent
        .canonicalize()
        .map_err(|e| PathError(format!("parent of {requested:?} does not exist: {e}")))?;
    if !canon_parent.starts_with(root) {
        return Err(PathError("path escapes workspace_root via symlink".into()));
    }
    let target = canon_parent.join(file_name);
    // The final component is re-joined without canonicalisation so a
    // not-yet-existing file still resolves — but if it already exists as a
    // symlink, an O_CREAT|O_TRUNC write follows it and lands wherever it
    // points, outside the workspace. A hostile repo or an agent can
    // plant `workspace_root/foo -> ~/.ssh/authorized_keys`. Refuse to write
    // through it; the caller may remove the link explicitly if that is the
    // real intent.
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(PathError(
                "refusing to write through a symlink at the target path".into(),
            ));
        }
        _ => {}
    }
    Ok(target)
}

/// Variant of [`resolve_existing`] that tolerates the path not yet existing.
/// Used by callers that just want the lexically-joined path without verifying
/// it's inside the workspace via canonicalisation (e.g. the search root may
/// be a virtual subdirectory). Always canonicalises if the path exists.
pub fn resolve_existing_or_lexical(root: &Path, requested: &str) -> Result<PathBuf> {
    let joined = strip_lexically(root, requested)?;
    match joined.canonicalize() {
        Ok(canon) => {
            if !canon.starts_with(root) {
                return Err(PathError("path escapes workspace_root via symlink".into()));
            }
            Ok(canon)
        }
        Err(_) => Ok(joined),
    }
}

/// Basename prefixes, suffixes and exact names that hold credentials. Matched
/// lowercase. A hidden name is refused before this list is consulted, so the
/// dotfiles here only matter where a caller deliberately allows hidden paths
/// (a tracked dotfile in a git diff).
const SECRET_EXACT: &[&str] = &[
    ".netrc",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".envrc",
    ".htpasswd",
    ".mcp.json",
    "settings.local.json",
    "credentials",
    "kubeconfig",
    "key.properties",
    "authorized_keys",
    "known_hosts",
    "pip.conf",
];

const SECRET_SUFFIXES: &[&str] = &[
    ".env",
    ".key",
    ".pem",
    ".p8",
    ".p12",
    ".pfx",
    ".kdbx",
    ".keystore",
    ".jks",
    ".asc",
    ".gpg",
    ".ppk",
    ".ovpn",
    ".tfstate",
    ".tfvars",
    ".sqlite",
    ".sqlite3",
    ".db",
];

/// Backup suffixes stripped before the checks above, so `server.key.bak`
/// and `.env~` are judged by the name they shadow.
const BACKUP_SUFFIXES: &[&str] = &[".bak", ".backup", ".old", ".orig", ".swp", "~"];

/// Source and prose extensions. A file like `secret_broker.rs` or
/// `0005_tokens.sql` is code *about* credentials, not one, so the substring
/// heuristics below skip these unless the stem is exactly a credential word.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "swift", "c", "h",
    "cc", "cpp", "hpp", "cs", "rb", "php", "sql", "md", "dart", "vue", "svelte",
];

/// Whether a single basename looks like a credential.
pub fn is_secret_name(name: &str) -> bool {
    let mut name = name.to_lowercase();
    while let Some(stripped) = BACKUP_SUFFIXES
        .iter()
        .find_map(|s| name.strip_suffix(s).filter(|rest| !rest.is_empty()))
    {
        name = stripped.to_string();
    }
    if name.is_empty() {
        return false;
    }
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    if SECRET_EXACT.contains(&name.as_str()) {
        return true;
    }
    if SECRET_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or("");
    if stem.starts_with("wg") && name.ends_with(".conf") {
        return true;
    }
    if name.starts_with("id_rsa")
        || name.starts_with("id_dsa")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_ed25519")
    {
        return true;
    }
    const CREDENTIAL_STEMS: &[&str] = &[
        "secret",
        "secrets",
        "credential",
        "credentials",
        "token",
        "tokens",
    ];
    if CREDENTIAL_STEMS.contains(&stem) {
        return true;
    }
    let is_code = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| CODE_EXTENSIONS.contains(&ext));
    if is_code {
        return false;
    }
    // `secrets.tf`, `secrets.yaml`, `client_secret.json`, `credentials.json`,
    // `gcp-service-account.json`, `deploy_token`, `api-token.txt`.
    name.contains("secret")
        || name.contains("credential")
        || name.contains("service-account")
        || name.contains("_token")
        || name.contains("-token")
        || name.contains("token_")
}

/// The components of `path` below `root`, or `None` when it is not under it.
fn components_below<'a>(root: &Path, path: &'a Path) -> Option<Vec<std::borrow::Cow<'a, str>>> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy()),
                _ => None,
            })
            .collect(),
    )
}

/// Whether `path` has a hidden component below `root`.
pub fn has_hidden_component(root: &Path, path: &Path) -> bool {
    match components_below(root, path) {
        Some(parts) => parts.iter().any(|p| p.starts_with('.')),
        None => true,
    }
}

/// Whether `path` names a credential anywhere below `root`: any component that
/// looks like one (so `secrets/` or `credentials/` hides what sits inside), or
/// a path under `.git/` (remote URLs in `.git/config` can carry tokens).
pub fn names_a_secret(root: &Path, path: &Path) -> bool {
    match components_below(root, path) {
        Some(parts) => parts
            .iter()
            .any(|p| p.eq_ignore_ascii_case(".git") || is_secret_name(p)),
        None => true,
    }
}

/// The one content-exposure policy. `path` must already be resolved
/// (canonical, under `root`). Refuses hidden components and credential names
/// with an error naming the rule, never the bytes.
pub fn may_show(root: &Path, path: &Path) -> Result<()> {
    if has_hidden_component(root, path) {
        return Err(PathError(
            "refusing to read a hidden path through the file API".into(),
        ));
    }
    if names_a_secret(root, path) {
        return Err(PathError(
            "refusing to read a credential file through the file API".into(),
        ));
    }
    Ok(())
}

/// Resolve an existing path without following its final component: the parent
/// is canonicalised under `root` and the name re-joined, so a symlink resolves
/// to the link itself. Used to delete or rename an entry, where acting on the
/// link (not its target) is the intent. Refuses the root itself.
pub fn resolve_no_follow(root: &Path, requested: &str) -> Result<PathBuf> {
    let joined = strip_lexically(root, requested)?;
    let parent = joined
        .parent()
        .ok_or_else(|| PathError("path has no parent".into()))?;
    let file_name = joined
        .file_name()
        .ok_or_else(|| PathError("path has no final component".into()))?;
    let canon_parent = parent
        .canonicalize()
        .map_err(|e| PathError(format!("parent of {requested:?} does not exist: {e}")))?;
    if !canon_parent.starts_with(root) {
        return Err(PathError("path escapes workspace_root via symlink".into()));
    }
    let target = canon_parent.join(file_name);
    std::fs::symlink_metadata(&target)
        .map_err(|e| PathError(format!("path {requested:?}: {e}")))?;
    Ok(target)
}
