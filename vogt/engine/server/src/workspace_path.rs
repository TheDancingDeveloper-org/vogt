//! Shared workspace path resolution used by every API that takes a client-supplied path.
//!
//! All endpoints that touch the filesystem must funnel through one policy so a
//! future change can't accidentally leave one of them lax. The two operations
//! we need are:
//!
//! * [`resolve_existing`] — for reads (file, dir, git repo lookups). Canonicalises
//!   the requested path and asserts the result still lives inside the workspace
//!   root after symlinks are resolved.
//! * [`resolve_existing_allow_absolute`] — the same policy, but also accepts an
//!   absolute path that already points somewhere under the workspace root.
//! * [`resolve_for_write`] — for creating or overwriting a file. Canonicalises
//!   the parent directory (which must already exist after any optional
//!   `create_parents` step) and joins the final filename component without
//!   following it as a symlink.
//!
//! Both reject `..`, root, and prefix components up front so callers don't
//! depend on canonicalisation alone.
//!
//! Confinement answers *where* a path may point; [`may_show`] answers *whether
//! its bytes may leave the engine*. Every endpoint that returns file content —
//! the viewer, the download, the git diff's working-tree side, the ripgrep
//! search — calls it on the **resolved** path, so a symlink or a rename cannot
//! walk a credential past it. It refuses any hidden component (what the file
//! browser already hides: `.git/`, `.ssh/`, `.claude/`, `.env`, `.mcp.json`)
//! and any name that looks like a credential.
//!
//! The canonical workspace root is assumed to already be canonical; the config
//! loader canonicalises it once at startup.
//!
//! CodeQL note: `rust/path-injection` flags the call sites that pass a
//! client-supplied string into these functions, because its default model does
//! not recognise this module as a barrier. Every filesystem-touching handler
//! (`files.rs`, `history*.rs`, and the rest) routes through `resolve_*` here, so
//! those alerts are false positives — the containment check below is the
//! barrier the query cannot see. Path builders elsewhere are safe for a
//! different reason: they interpolate a typed `Uuid` (which cannot hold a
//! separator or `..`) or a server-config path, never a raw request string.

use std::path::{Component, Path, PathBuf};

use crate::error::{ApiError, Result};

fn strip_lexically(root: &Path, requested: &str) -> Result<PathBuf> {
    let rel = requested.trim_start_matches('/');
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(ApiError::BadRequest(
                    "path contains '..' (parent component)".into(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ApiError::BadRequest("path must be relative".into()));
            }
        }
    }
    Ok(out)
}

fn canonicalize_under_root(root: &Path, candidate: &Path, requested: &str) -> Result<PathBuf> {
    let canon = candidate
        .canonicalize()
        .map_err(|e| ApiError::BadRequest(format!("path {requested:?}: {e}")))?;
    if !canon.starts_with(root) {
        return Err(ApiError::BadRequest(
            "path escapes workspace_root via symlink".into(),
        ));
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
        .ok_or_else(|| ApiError::BadRequest("path has no parent".into()))?;
    let file_name = joined
        .file_name()
        .ok_or_else(|| ApiError::BadRequest("path has no final component".into()))?;
    let canon_parent = parent.canonicalize().map_err(|e| {
        ApiError::BadRequest(format!("parent of {requested:?} does not exist: {e}"))
    })?;
    if !canon_parent.starts_with(root) {
        return Err(ApiError::BadRequest(
            "path escapes workspace_root via symlink".into(),
        ));
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
            return Err(ApiError::BadRequest(
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
                return Err(ApiError::BadRequest(
                    "path escapes workspace_root via symlink".into(),
                ));
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
        return Err(ApiError::BadRequest(
            "refusing to read a hidden path through the file API".into(),
        ));
    }
    if names_a_secret(root, path) {
        return Err(ApiError::BadRequest(
            "refusing to read a credential file through the file API".into(),
        ));
    }
    Ok(())
}

/// Refuse a `workspace_root` that would put the engine's own state and every
/// credential in the home directory behind the file API: `/`, the home
/// directory itself (or an ancestor of it), or a root that contains
/// `state_dir`. Paths are compared after canonicalising where they exist.
pub fn validate_root(root: &Path, home: Option<&Path>, state_dir: &Path) -> Result<()> {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let root = canon(root);
    if root.parent().is_none() {
        return Err(ApiError::Config(format!(
            "workspace_root {} is the filesystem root; set it to a dedicated tree such as ~/Working",
            root.display()
        )));
    }
    if let Some(home) = home.map(canon) {
        if home.starts_with(&root) {
            return Err(ApiError::Config(format!(
                "workspace_root {} contains the home directory {}; set it to a dedicated tree such as ~/Working",
                root.display(),
                home.display()
            )));
        }
    }
    let state = canon(state_dir);
    if state.starts_with(&root) {
        return Err(ApiError::Config(format!(
            "workspace_root {} contains state_dir {}; the file API would serve the engine's own state",
            root.display(),
            state.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn rejects_parent_component() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        assert!(resolve_existing(&root, "../escape").is_err());
        assert!(resolve_existing(&root, "a/../../escape").is_err());
    }

    #[test]
    fn rejects_root_component() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        // Leading slashes are deliberately accepted as workspace-relative
        // paths for compatibility, but a root component that survives the
        // lexical stripping must never escape to the host root.
        assert_eq!(resolve_existing(&root, "/").unwrap(), root);
        assert!(matches!(
            resolve_existing(&root, "/../escape"),
            Err(ApiError::BadRequest(msg)) if msg.contains("parent component")
        ));
    }

    #[test]
    fn rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().canonicalize().unwrap();
        std::fs::write(outside_path.join("victim.txt"), b"hi").unwrap();
        symlink(&outside_path, root.join("link")).unwrap();

        let res = resolve_existing(&root, "link/victim.txt");
        assert!(
            matches!(&res, Err(ApiError::BadRequest(msg)) if msg.contains("symlink")),
            "expected symlink rejection, got {res:?}"
        );
    }

    #[test]
    fn write_rejects_symlink_parent_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().canonicalize().unwrap();
        symlink(&outside_path, root.join("link")).unwrap();

        let res = resolve_for_write(&root, "link/new.txt");
        assert!(
            matches!(&res, Err(ApiError::BadRequest(msg)) if msg.contains("symlink")),
            "expected symlink rejection, got {res:?}"
        );
    }

    #[test]
    fn write_rejects_final_component_symlink_escape() {
        // The parent is the workspace root (canonical, under root), but the
        // final component is itself a symlink pointing outside. The old code
        // canonicalised only the parent and returned parent.join(name), so a
        // following O_CREAT|O_TRUNC write landed on the link's target.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().canonicalize().unwrap();
        let victim = outside_path.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        symlink(&victim, root.join("foo")).unwrap();

        let res = resolve_for_write(&root, "foo");
        assert!(
            matches!(&res, Err(ApiError::BadRequest(msg)) if msg.contains("symlink")),
            "expected symlink rejection, got {res:?}"
        );
    }

    #[test]
    fn write_accepts_new_file_in_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let p = resolve_for_write(&root, "new.txt").unwrap();
        assert_eq!(p, root.join("new.txt"));
    }

    #[test]
    fn absolute_existing_path_under_root_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let file = root.join("nested.txt");
        std::fs::write(&file, b"hi").unwrap();

        let resolved = resolve_existing_allow_absolute(&root, &file.to_string_lossy()).unwrap();
        assert_eq!(resolved, file);
    }

    #[test]
    fn absolute_existing_path_outside_root_is_rejected() {
        let root_tmp = tempfile::tempdir().unwrap();
        let root = root_tmp.path().canonicalize().unwrap();
        let outside_tmp = tempfile::tempdir().unwrap();
        let outside = outside_tmp.path().canonicalize().unwrap();
        let file = outside.join("escape.txt");
        std::fs::write(&file, b"hi").unwrap();

        let res = resolve_existing_allow_absolute(&root, &file.to_string_lossy());
        assert!(
            matches!(&res, Err(ApiError::BadRequest(msg)) if msg.contains("workspace_root")),
            "expected outside-root rejection, got {res:?}"
        );
    }

    #[test]
    fn secret_names_are_refused() {
        let secret = [
            ".env",
            ".env.local",
            ".env~",
            ".env.bak",
            "prod.env",
            "id_rsa",
            "id_ed25519",
            "server.key",
            "server.key.bak",
            "cert.pem.old",
            "store.p12",
            "vault.kdbx",
            "github_token",
            "api-token.txt",
            "deploy_token_prod",
            "token.txt",
            ".netrc",
            ".git-credentials",
            ".mcp.json",
            "settings.local.json",
            "credentials",
            "credentials.json",
            "gcp-service-account.json",
            "client_secret.json",
            "secrets.tf",
            "secrets.yaml",
            "terraform.tfstate",
            "terraform.tfstate.backup",
            "prod.auto.tfvars",
            "kubeconfig",
            "key.properties",
            "authorized_keys",
            "wg0.conf",
            "client.ovpn",
            "AuthKey_ABC.p8",
            "app.sqlite",
            "data.db",
            "secrets.py",
            "token.ts",
        ];
        for name in secret {
            assert!(is_secret_name(name), "{name} must be refused");
        }
        let allowed = [
            "README.md",
            "main.rs",
            "notes.txt",
            "identity.ts",
            "tokenise.py",
            "id_generator.py",
            "id_utils.rs",
            "Cargo.toml",
            "package.json",
            "docker-compose.yml",
            "env.d.ts",
            "secret_broker.rs",
            "0005_tokens.sql",
            "0017_password_credentials.sql",
            "test_bootstrap_agent_token.py",
        ];
        for name in allowed {
            assert!(!is_secret_name(name), "{name} must be readable");
        }
    }

    #[test]
    fn hidden_components_are_refused_below_root_only() {
        // The root itself may sit under a hidden directory (tempdirs are
        // `.tmpXXXX`); only components below it count.
        let root = Path::new("/srv/.hidden/work");
        assert!(may_show(root, &root.join("repo/src/main.rs")).is_ok());
        for bad in [
            "repo/.git/config",
            "repo/.claude/settings.local.json",
            ".ssh/id_ed25519",
            "infra/.terraform/terraform.tfstate",
            "repo/.mcp.json",
            "repo/.envrc",
            "repo/.github/workflows/ci.yml",
        ] {
            assert!(
                matches!(may_show(root, &root.join(bad)), Err(ApiError::BadRequest(m)) if m.contains("hidden")),
                "{bad} must be refused as hidden"
            );
        }
        // Secret-named components refuse what sits inside them.
        assert!(may_show(root, &root.join("deploy/secrets/app.yaml")).is_err());
        assert!(may_show(root, &root.join("repo/prod.env")).is_err());
        // Outside the root is never shown.
        assert!(may_show(root, Path::new("/etc/hostname")).is_err());
        // Tracked-dotfile callers still refuse `.git/` internals and secret names.
        assert!(names_a_secret(root, &root.join("repo/.git/config")));
        assert!(names_a_secret(root, &root.join("repo/.mcp.json")));
        assert!(!names_a_secret(
            root,
            &root.join("repo/.github/workflows/ci.yml")
        ));
    }

    #[test]
    fn workspace_root_guard() {
        let home = tempfile::tempdir().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        let working = home_path.join("Working");
        std::fs::create_dir(&working).unwrap();
        let state = home_path.join(".local/share/vogt-engine");

        assert!(validate_root(&working, Some(&home_path), &state).is_ok());
        assert!(validate_root(Path::new("/"), Some(&home_path), &state).is_err());
        assert!(validate_root(&home_path, Some(&home_path), &state).is_err());
        // An ancestor of home is as bad as home itself.
        assert!(validate_root(home_path.parent().unwrap(), Some(&home_path), &state).is_err());
        // A state_dir inside the root would serve the engine's own state.
        assert!(validate_root(&working, Some(&home_path), &working.join("state")).is_err());
        // No HOME at all still applies the other two rules.
        assert!(validate_root(&working, None, &state).is_ok());
    }
}
