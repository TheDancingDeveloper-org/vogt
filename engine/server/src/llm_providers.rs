//! The LLM provider registry (WI-3491): which model providers a Codex session
//! may use, and the one secret each needs, kept in a file the engine re-reads
//! at every launch so a provider is added or removed without a restart.
//!
//! `ENGINE_LLM_PROVIDERS_FILE` is TOML, one `[[provider]]` table per entry:
//!
//! ```toml
//! [[provider]]
//! name = "deepseek"                    # the Codex model_providers key
//! base_url = "http://litellm:4000/v1"
//! wire_api = "chat"                    # or "responses"
//! env_key = "HOMELAB_LITELLM_DEEPSEEK_KEY"
//! secret_project = "<secrets-manager project id>"
//! secret_name = "HOMELAB_LITELLM_DEEPSEEK_KEY"
//! models = ["deepseek-chat"]           # informational, shown in status
//! ```
//!
//! At each launch the registry does two things, and nothing else:
//!
//! - every provider's `env_key` joins the agent-auth manifest as an
//!   `optional` line (`env_key secret_project secret_name`), so the helper
//!   resolves it into the session and the broker's policy covers it. A name in
//!   no manifest and no registry is still refused;
//! - a Codex session's config gets one `model_providers.<name>` table per
//!   provider: as `-c` overrides on a host launch's argv, or as TOML in the pod
//!   bootstrap's `codex_config`. The shared `~/.codex/config.toml` is never
//!   rewritten, so a running session keeps the providers it launched with.
//!
//! The file holds names and addresses, never a key.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::secret_broker::{ManifestPolicy, ManifestSecret};

/// One registered provider.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub wire_api: String,
    pub env_key: String,
    pub secret_project: String,
    pub secret_name: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    provider: Vec<Provider>,
}

/// The registry in `path`, read now. A missing file registers nothing.
pub fn load(path: &Path) -> Result<Vec<Provider>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!(
                "reading ENGINE_LLM_PROVIDERS_FILE ({}): {e}",
                path.display()
            ))
        }
    };
    parse(&text).map_err(|e| format!("in ENGINE_LLM_PROVIDERS_FILE ({}): {e}", path.display()))
}

/// Parse and validate a registry. Every field that reaches Codex's config or
/// the manifest is held to a narrow vocabulary, so a registry entry can never
/// smuggle in another TOML key, a flag or a manifest line.
pub fn parse(text: &str) -> Result<Vec<Provider>, String> {
    let file: RegistryFile = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let mut seen_names: Vec<&str> = Vec::new();
    let mut seen_keys: Vec<&str> = Vec::new();
    for p in &file.provider {
        validate_provider_env_name(&p.name, &p.env_key)?;
        if !is_provider_name(&p.name) {
            return Err(format!(
                "provider name {:?} must be 1-64 of a-z, 0-9, '-' or '_', starting with a letter or digit",
                p.name
            ));
        }
        if seen_names.contains(&p.name.as_str()) {
            return Err(format!("provider {} is registered twice", p.name));
        }
        seen_names.push(&p.name);
        if !(p.base_url.starts_with("http://") || p.base_url.starts_with("https://"))
            || p.base_url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!(
                "provider {}: base_url must be an http(s) URL",
                p.name
            ));
        }
        if !matches!(p.wire_api.as_str(), "chat" | "responses") {
            return Err(format!(
                "provider {}: wire_api must be \"chat\" or \"responses\"",
                p.name
            ));
        }
        if !is_env_name(&p.env_key) {
            return Err(format!(
                "provider {}: env_key {:?} is not an environment variable name",
                p.name, p.env_key
            ));
        }
        if seen_keys.contains(&p.env_key.as_str()) {
            return Err(format!(
                "provider {}: env_key {} is already another provider's",
                p.name, p.env_key
            ));
        }
        seen_keys.push(&p.env_key);
        for (field, value) in [
            ("secret_project", &p.secret_project),
            ("secret_name", &p.secret_name),
        ] {
            if !is_manifest_word(value) {
                return Err(format!(
                    "provider {}: {field} must be one word with no '#'",
                    p.name
                ));
            }
        }
        for model in &p.models {
            if model.is_empty() || model.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(format!(
                    "provider {}: model {model:?} is not one word",
                    p.name
                ));
            }
        }
    }
    Ok(file.provider)
}

/// Validate a single provider using the same rules as a registry file.
pub fn validate(provider: &Provider) -> Result<(), String> {
    let text = toml::to_string(&RegistryFile {
        provider: vec![provider.clone()],
    })
    .map_err(|e| e.to_string())?;
    parse(&text).map(|_| ())
}

/// Validate a provider against the deploy-time manifest. A registry provider
/// may not shadow a manifest variable (including a secret declared by the
/// deployment), because that would change which secret a session receives.
pub fn validate_against_manifest(
    provider: &Provider,
    manifest: &[ManifestSecret],
) -> Result<(), String> {
    validate(provider)?;
    if manifest.iter().any(|entry| entry.var == provider.env_key) {
        return Err(format!(
            "provider {}: env_key {} clashes with manifest VAR",
            provider.name, provider.env_key
        ));
    }
    Ok(())
}

/// Return whether a provider names one exact secret approved by the live
/// manifest or by an operator-approved grant project.  The provider's
/// `env_key` is intentionally not part of this lookup: it is the name Codex
/// reads, and may be distinct from the manifest VAR that authorises the
/// underlying secret.  Shadowing that VAR is checked separately above.
pub fn secret_reference_is_approved(
    provider: &Provider,
    manifest: &[ManifestSecret],
    grant_projects: &[String],
) -> bool {
    manifest.iter().any(|entry| {
        entry.project_id == provider.secret_project && entry.secret_name == provider.secret_name
    }) || grant_projects
        .iter()
        .any(|project| project == &provider.secret_project)
}

/// Names which are owned by the engine/runtime and may not be supplied by a
/// provider registry entry.  In particular, allowing ENGINE_* here would let
/// an operator-controlled registry replace the engine's own credentials.
pub fn validate_provider_env_key(provider: &Provider) -> Result<(), String> {
    validate(provider)?;
    validate_provider_env_name(&provider.name, &provider.env_key)
}

fn validate_provider_env_name(name: &str, key: &str) -> Result<(), String> {
    if key == "PATH"
        || key == "LD_PRELOAD"
        || key == "LD_LIBRARY_PATH"
        || key == "HOME"
        || key == "GH_TOKEN"
        || key == "NODE_OPTIONS"
        || key.starts_with("ENGINE_")
        || key.starts_with("VOGT_")
        || key.starts_with("INFISICAL_")
    {
        return Err(format!(
            "provider {}: env_key {} is reserved by the engine",
            name, key
        ));
    }
    Ok(())
}

/// Serialise registry read/modify/write operations.  The rename in `write`
/// is atomic, but without this lock concurrent updates can still lose one
/// another's changes.
pub fn registry_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Validate the complete registry before publishing it.
pub fn validate_all(providers: &[Provider]) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    let mut env_keys = std::collections::HashSet::new();
    let mut secret_refs = std::collections::HashSet::new();
    for provider in providers {
        validate(provider)?;
        if !names.insert(&provider.name) {
            return Err(format!("duplicate provider name {:?}", provider.name));
        }
        if !env_keys.insert(&provider.env_key) {
            return Err(format!(
                "provider {}: env_key {} is already another provider's",
                provider.name, provider.env_key
            ));
        }
        let secret_ref = (&provider.secret_project, &provider.secret_name);
        if !secret_refs.insert(secret_ref) {
            return Err(format!(
                "provider {}: secret reference {}/{} is already another provider's",
                provider.name, provider.secret_project, provider.secret_name
            ));
        }
    }
    Ok(())
}

/// Replace the registry with a durable, secret-free TOML document.
/// `rename` is the atomic publication point; a failed write leaves the old
/// registry untouched.
pub fn write(path: &Path, providers: &[Provider]) -> Result<(), String> {
    let text = toml::to_string(&RegistryFile {
        provider: providers.to_vec(),
    })
    .map_err(|e| format!("serialising provider registry: {e}"))?;
    parse(&text).map_err(|e| format!("invalid provider registry: {e}"))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("creating provider registry directory: {e}"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("creating provider registry temporary file: {e}"))?;
    std::io::Write::write_all(&mut temp, text.as_bytes())
        .map_err(|e| format!("writing provider registry temporary file: {e}"))?;
    temp.as_file()
        .sync_all()
        .map_err(|e| format!("syncing provider registry temporary file: {e}"))?;
    temp.persist(path)
        .map_err(|e| format!("publishing provider registry: {}", e.error))?;
    Ok(())
}

/// A provider's public status. Secret values are deliberately absent.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProviderStatus {
    #[serde(flatten)]
    pub provider: Provider,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn statuses(
    path: Option<&Path>,
    manifest: &[ManifestSecret],
) -> Result<Vec<ProviderStatus>, String> {
    let providers = path.map(load).transpose()?.unwrap_or_default();
    Ok(providers
        .into_iter()
        .map(|provider| {
            let clash = manifest.iter().any(|entry| entry.var == provider.env_key);
            ProviderStatus {
                provider,
                state: if clash { "dropped" } else { "active" },
                detail: clash.then(|| "env_key clashes with manifest VAR".to_string()),
            }
        })
        .collect())
}

fn is_provider_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// A manifest field: one whitespace-free word that cannot start a comment.
fn is_manifest_word(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '#')
}

/// The manifest lines the registry contributes: one `optional` entry per
/// provider key, so a key that is missing does not mark the launch degraded.
pub fn manifest_entries(providers: &[Provider]) -> Vec<ManifestSecret> {
    providers
        .iter()
        .map(|p| ManifestSecret {
            var: p.env_key.clone(),
            project_id: p.secret_project.clone(),
            secret_name: p.secret_name.clone(),
            policy: ManifestPolicy::Optional,
            writable: false,
        })
        .collect()
}

/// The same providers as `config.toml` tables, for a pod's `codex_config`.
pub fn codex_config_tables(providers: &[Provider]) -> String {
    let s = crate::codex_guidance::toml_basic_string;
    providers
        .iter()
        .map(|p| {
            format!(
                "[model_providers.{}]\nname = {}\nbase_url = {}\nwire_api = {}\nenv_key = {}\nrequires_openai_auth = false\n\n",
                p.name,
                s(&p.name),
                s(&p.base_url),
                s(&p.wire_api),
                s(&p.env_key)
            )
        })
        .collect()
}

/// The complete Codex config fragment for a pod launch. Root keys must precede
/// the provider tables or Codex will interpret them as table members.
/// Select the provider and model for a Codex launch. Prefer the explicitly
/// named `litellm` provider for compatibility with the shared deployment, but
/// fall back to the first registered provider (whose name is the actual TOML
/// table key). A provider's first model is its fallback model.
pub fn codex_selection<'a>(
    requested_model: Option<&str>,
    providers: &'a [Provider],
) -> Option<(String, &'a Provider)> {
    let provider = providers
        .iter()
        .find(|p| p.name == "litellm")
        .or_else(|| providers.first())?;
    let model = requested_model
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .or_else(|| provider.models.first().cloned())
        .unwrap_or_else(|| "grok".to_owned());
    Some((model, provider))
}

pub fn codex_config(requested_model: Option<&str>, providers: &[Provider]) -> String {
    let (model, provider) = codex_selection(requested_model, providers)
        .expect("codex config requires at least one provider");
    format!(
        "model_provider = {}\nmodel = {}\n\n{}",
        crate::codex_guidance::toml_basic_string(&provider.name),
        crate::codex_guidance::toml_basic_string(&model),
        codex_config_tables(providers)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEEPSEEK: &str = r#"
[[provider]]
name = "deepseek"
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "DEEPSEEK_KEY"
secret_project = "proj-a"
secret_name = "HOMELAB_LITELLM_DEEPSEEK_KEY"
models = ["deepseek-chat"]
"#;

    #[test]
    fn a_registered_provider_is_one_manifest_line_and_one_codex_table() {
        let providers = parse(DEEPSEEK).unwrap();
        assert_eq!(
            crate::secret_broker::render_manifest(&manifest_entries(&providers)),
            "DEEPSEEK_KEY proj-a HOMELAB_LITELLM_DEEPSEEK_KEY optional\n"
        );

        // The shared builder is what Codex parses as `key=value` TOML.
        let provider = &providers[0];
        let shared = vogt_session_agent::CodexProvider {
            name: provider.name.clone(),
            base_url: provider.base_url.clone(),
            wire_api: provider.wire_api.clone(),
            env_key: provider.env_key.clone(),
        };
        let args = vogt_session_agent::codex_args(&vogt_session_agent::CodexArgs {
            command_args: &[],
            resume: None,
            session_id: None,
            model: None,
            config_model: None,
            effort: None,
            permission_mode: None,
            approval_policy: "never",
            exec: false,
            compact_limit: None,
            provider: None,
            providers: &[shared],
            trust_cwd: None,
            guidance: None,
            charter: None,
            mcp_servers: &[],
            mcp_args: &[],
            prompt: None,
        })
        .unwrap();
        let (key, value) = args[1].split_once('=').unwrap();
        assert_eq!(key, "model_providers.deepseek");
        let table: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        let v = table["v"].as_table().unwrap();
        assert_eq!(v["base_url"].as_str(), Some("http://litellm:4000/v1"));
        assert_eq!(v["wire_api"].as_str(), Some("chat"));
        assert_eq!(v["env_key"].as_str(), Some("DEEPSEEK_KEY"));
        assert_eq!(v["requires_openai_auth"].as_bool(), Some(false));

        // The pod form is the same table in config.toml.
        let config: toml::Table = toml::from_str(&codex_config_tables(&providers)).unwrap();
        assert_eq!(
            config["model_providers"]["deepseek"]["env_key"].as_str(),
            Some("DEEPSEEK_KEY")
        );

        let config: toml::Table = toml::from_str(&codex_config(Some("grok"), &providers)).unwrap();
        assert_eq!(config["model_provider"].as_str(), Some("deepseek"));
        assert!(config["model_providers"]
            .as_table()
            .unwrap()
            .contains_key("deepseek"));
        assert_eq!(config["model"].as_str(), Some("grok"));
    }

    #[test]
    fn nothing_registered_is_nothing_added() {
        assert!(parse("").unwrap().is_empty());
        assert!(load(Path::new("/nonexistent/providers.toml"))
            .unwrap()
            .is_empty());
        let args = vogt_session_agent::codex_args(&vogt_session_agent::CodexArgs {
            command_args: &[],
            resume: None,
            session_id: None,
            model: None,
            config_model: None,
            effort: None,
            permission_mode: None,
            approval_policy: "never",
            exec: false,
            compact_limit: None,
            provider: None,
            providers: &[],
            trust_cwd: None,
            guidance: None,
            charter: None,
            mcp_servers: &[],
            mcp_args: &[],
            prompt: None,
        })
        .unwrap();
        assert!(!args.iter().any(|arg| arg.starts_with("model_providers.")));
    }

    #[test]
    fn an_entry_cannot_smuggle_a_key_a_line_or_a_flag() {
        let with = |field: &str, value: &str| DEEPSEEK.replace(field, value);
        let refused = [
            // A name that would open another TOML key path.
            with("name = \"deepseek\"", "name = \"a.b\""),
            with("name = \"deepseek\"", "name = \"-c\""),
            with(
                "base_url = \"http://litellm:4000/v1\"",
                "base_url = \"file:///etc\"",
            ),
            with("wire_api = \"chat\"", "wire_api = \"grpc\""),
            with("env_key = \"DEEPSEEK_KEY\"", "env_key = \"BAD-KEY\""),
            // A second manifest line hidden in a field.
            with(
                "secret_name = \"HOMELAB_LITELLM_DEEPSEEK_KEY\"",
                "secret_name = \"a\\nX p s\"",
            ),
            with("secret_project = \"proj-a\"", "secret_project = \"p # c\""),
            // A field Codex would read that the registry does not own.
            with(
                "models = [",
                "experimental_bearer_token = \"x\"\nmodels = [",
            ),
            // The same provider, or the same key, twice.
            format!("{DEEPSEEK}{DEEPSEEK}"),
            format!(
                "{DEEPSEEK}{}",
                DEEPSEEK.replace("name = \"deepseek\"", "name = \"other\"")
            ),
        ];
        for text in refused {
            assert!(parse(&text).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn registry_writes_are_atomic_and_status_never_contains_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/providers.toml");
        let providers = parse(DEEPSEEK).unwrap();
        write(&path, &providers).unwrap();
        assert_eq!(load(&path).unwrap(), providers);
        let statuses = statuses(Some(&path), &[]).unwrap();
        let json = serde_json::to_string(&statuses).unwrap();
        assert!(json.contains("DEEPSEEK_KEY"));
        assert!(!json.contains("token"));
    }

    #[test]
    fn manifest_collision_is_rejected_and_reported_as_dropped() {
        let provider = parse(DEEPSEEK).unwrap().remove(0);
        let manifest = vec![ManifestSecret {
            var: provider.env_key.clone(),
            project_id: "deploy".into(),
            secret_name: "deploy-secret".into(),
            policy: ManifestPolicy::Required,
            writable: false,
        }];
        assert!(validate_against_manifest(&provider, &manifest)
            .unwrap_err()
            .contains("clashes with manifest VAR"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        write(&path, &[provider]).unwrap();
        let status = statuses(Some(&path), &manifest).unwrap().remove(0);
        assert_eq!(status.state, "dropped");
        assert_eq!(
            status.detail.as_deref(),
            Some("env_key clashes with manifest VAR")
        );
    }

    #[test]
    fn reserved_provider_environment_names_are_rejected_when_loaded() {
        for key in [
            "PATH",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "HOME",
            "GH_TOKEN",
            "NODE_OPTIONS",
            "ENGINE_TOKEN",
            "VOGT_TOKEN",
            "INFISICAL_CLIENT_ID",
        ] {
            let text = DEEPSEEK.replace("DEEPSEEK_KEY", key);
            assert!(
                parse(&text).unwrap_err().contains("reserved"),
                "accepted {key}"
            );
        }
    }
}
