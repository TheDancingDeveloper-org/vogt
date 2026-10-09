//! The operation registry. Ports `src/vogt/registry/`.
//!
//! One definition per product operation: name, scope, mutation flag, route,
//! CLI path and MCP tool name. Handlers are wired to services as those land
//! (S1–S9); until then an operation carries a `not_ported` handler, so the
//! manifest is complete before the behaviour is. `registry.dump` is the one
//! operation this module implements itself.

pub mod operations;
mod schemas;

use std::collections::{HashMap, HashSet};

use crate::errors::VogtError;

/// Authorization scopes. Every operation declares what it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    WorkWrite,
    ProjectWrite,
    Admin,
    Writeback,
}

impl Scope {
    /// The wire spelling, as the registry manifest and tokens use it.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::WorkWrite => "work.write",
            Scope::ProjectWrite => "project.write",
            Scope::Admin => "admin",
            Scope::Writeback => "writeback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    #[allow(dead_code)] // Python's set uses GET and POST only; the type matches it.
    Patch,
    #[allow(dead_code)]
    Delete,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRoute {
    pub method: HttpMethod,
    pub path: &'static str,
}

impl HttpRoute {
    pub fn new(method: HttpMethod, path: &'static str) -> Self {
        assert!(
            path.starts_with('/'),
            "route path must start with '/': {path}"
        );
        Self { method, path }
    }
}

/// The CLI command path, e.g. `["project", "register"]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliBinding {
    pub path: &'static [&'static str],
}

impl CliBinding {
    pub fn new(path: &'static [&'static str]) -> Self {
        assert!(
            !path.is_empty(),
            "CLI binding needs at least one path segment"
        );
        Self { path }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Cli,
    Http,
    Mcp,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Cli => "cli",
            Transport::Http => "http",
            Transport::Mcp => "mcp",
        }
    }
}

/// The service behind an operation, once it is ported.
///
/// One signature for every operation: the context the hooks built, and the
/// parameters the transport already parsed. The result is the JSON the
/// transport sends back. An operation that is not in the table has not been
/// ported, which is a failure and never an empty success.
pub type ServiceFn = fn(
    &crate::application::context::Built,
    serde_json::Value,
) -> Result<serde_json::Value, VogtError>;

/// The service for an operation, or `None` while it is still `not_ported`.
///
/// The table lives here, not in the generated operation list, so a service
/// landing adds one arm and touches none of the four transports that call it.
pub fn service_for(name: &str) -> Option<ServiceFn> {
    match name {
        "migrate" => Some(crate::application::instance::migrate_op),
        "status" => Some(crate::application::instance::status_op),
        "contract.evaluate" => Some(crate::application::contracts::contract_evaluate_op),
        "token.issue" => Some(crate::application::services::auth::issue_token_op),
        "contract.check" => Some(crate::application::contracts::contract_check_op),
        "contract.adopt" => Some(crate::application::contracts::contract_adopt_op),
        "contract.decline" => Some(crate::application::contracts::contract_decline_op),
        "why" => Some(crate::application::views::why_op),
        _ => None,
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handler {
    /// The service for this operation is not ported. Running it returns the
    /// honest-unavailable error rather than an empty success.
    NotPorted,
    /// Implemented by this module: the registry describes itself.
    RegistryDump,
}

/// One capability of the product, in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub name: &'static str,
    pub summary: &'static str,
    pub scope: Scope,
    pub mutating: bool,
    /// The parameter model carries a required `reason`. True for every
    /// mutating operation (enforced at construction) and for the few reads
    /// that also audit a reason.
    pub reason_required: bool,
    pub route: HttpRoute,
    pub cli: CliBinding,
    pub handler: Handler,
}

impl Operation {
    /// Build an operation. `reason_required` follows `mutating`: the registry
    /// refuses a write without one, and no read in the Python set is built
    /// that way here — the reads that audit a reason set it afterwards.
    fn new(
        name: &'static str,
        summary: &'static str,
        scope: Scope,
        mutating: bool,
        route: HttpRoute,
        cli: CliBinding,
    ) -> Self {
        Self {
            name,
            summary,
            scope,
            mutating,
            reason_required: mutating,
            route,
            cli,
            handler: Handler::NotPorted,
        }
    }

    /// MCP tool names use underscores; everything else is identical.
    pub fn mcp_tool_name(&self) -> String {
        self.name.replace('.', "_")
    }

    /// Run the operation. An operation with no service yet is a failure, never a
    /// success, and the message names the operation so the caller can see which
    /// one is missing.
    pub fn run(
        &self,
        ctx: Option<&crate::application::context::Built>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VogtError> {
        match self.handler {
            Handler::RegistryDump => Ok(dump()),
            Handler::NotPorted => match (service_for(self.name), ctx) {
                (Some(service), Some(ctx)) => service(ctx, params),
                (Some(_), None) => Err(VogtError::InvalidRequest(format!(
                    "{} needs an instance context and none was given",
                    self.name
                ))),
                (None, _) => Err(VogtError::InvalidRequest(format!(
                    "{} is not available in this build: its service has not been ported yet",
                    self.name
                ))),
            },
        }
    }
}

/// What a missing `reason` is told. Every write is audited and the registry
/// refuses to build a write whose reason is optional, so the reason cannot be
/// defaulted.
#[allow(dead_code)] // The reason hint is part of the validation message P2.2 emits.
pub const REASON_HINT: &str = "every write is audited and must say why it is being made — pass \
     `reason` as a short sentence, e.g. reason=\"tests pass, ready for review\"";

/// Operations that exist only where the data directory is. Each entry carries
/// its justification, so "why is this excluded" is answerable without
/// archaeology.
pub const LOCAL_ONLY: &[(&str, &str)] = &[
    (
        "init",
        "Creates the instance in a local data directory. A running server \
         already has one, so there is no meaningful remote semantics.",
    ),
    (
        "migrate",
        "Brings the schema of the local data directory forward. Over HTTP it \
         would be a request asking the server to change the ground it is \
         standing on mid-flight; the server already migrates at startup \
         , so the remote case is served by restarting it.",
    ),
    (
        "serve",
        "Takes over this process to listen on a port. A running server being \
         asked over its own API to start another one is not a meaningful \
         request; restarting a service is the supervisor's job.",
    ),
    (
        "backup",
        "Writes to a path on the machine holding the data directory. Over \
         HTTP the path would name a filesystem the caller cannot see.",
    ),
    (
        "restore",
        "Replaces the live stores from a path on the server's filesystem, \
         and every client's view of the instance changes underneath them. \
         That is an operator action taken at the machine, not a request.",
    ),
    (
        "clone",
        "Replaces the live stores from a path on the server's filesystem, \
         exactly as restore does, so it is an operator action at the machine \
         for the same reason.",
    ),
    (
        "import",
        "Reads a file from the machine holding the data directory, for the \
         same reason as backup.",
    ),
    (
        "mcp.stdio",
        "Takes over this process's stdin and stdout to speak MCP. That is \
         meaningful only where the data directory is; a remote client uses \
         the streamable-HTTP transport at /mcp. \
         Offering it as a REST route would mean a server hijacking its own \
         stdout.",
    ),
];

/// Operations that exist only over HTTP.
pub const HTTP_ONLY: &[(&str, &str)] = &[(
    "session.token",
    "The session engine's own call, made over HTTP with its credential, \
     to mint or revoke the agent credential of a session it started \
     (WI-926). No person or agent has a reason to call it, and an MCP \
     tool or CLI command for it would only put a refusal on every \
     agent's tool list.",
)];

/// The registry is internally inconsistent — always a programming error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError(pub String);

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An ordered collection of operations, validated on build.
#[derive(Debug, Clone)]
pub struct OperationRegistry {
    operations: Vec<Operation>,
}

impl OperationRegistry {
    pub fn new(mut operations: Vec<Operation>) -> Result<Self, RegistryError> {
        // The reads that audit a reason. Every mutating operation already has
        // `reason_required`; these three do not mutate but still require one.
        for operation in &mut operations {
            if matches!(operation.name, "backup" | "restore" | "export") {
                operation.reason_required = true;
            }
            if operation.name == "registry.dump" {
                operation.handler = Handler::RegistryDump;
            }
        }
        let mut seen = HashSet::new();
        for operation in &operations {
            if !seen.insert(operation.name) {
                return Err(RegistryError(format!(
                    "duplicate operation name: {}",
                    operation.name
                )));
            }
        }
        let registry = Self { operations };
        registry.validate()?;
        registry.validate_schemas()?;
        Ok(registry)
    }

    pub fn len(&self) -> usize {
        self.operations.len()
    }

    #[allow(dead_code)] // Used by the transports that land in P2.3–P2.5.
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Operation> {
        self.operations.iter()
    }

    #[allow(dead_code)]
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.operations.iter().map(|operation| operation.name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.operations
            .iter()
            .any(|operation| operation.name == name)
    }

    pub fn get(&self, name: &str) -> Result<&Operation, RegistryError> {
        self.operations
            .iter()
            .find(|operation| operation.name == name)
            .ok_or_else(|| RegistryError(format!("unknown operation: {name}")))
    }

    pub fn by_mcp_tool(&self, tool_name: &str) -> Result<&Operation, RegistryError> {
        self.operations
            .iter()
            .find(|operation| operation.mcp_tool_name() == tool_name)
            .ok_or_else(|| RegistryError(format!("unknown MCP tool: {tool_name}")))
    }

    /// Which surfaces this operation is expected to appear on.
    pub fn transports_for(&self, name: &str) -> Vec<Transport> {
        if LOCAL_ONLY.iter().any(|(excluded, _)| *excluded == name) {
            return vec![Transport::Cli];
        }
        if HTTP_ONLY.iter().any(|(excluded, _)| *excluded == name) {
            return vec![Transport::Http];
        }
        vec![Transport::Cli, Transport::Http, Transport::Mcp]
    }

    #[allow(dead_code)]
    pub fn for_transport(&self, transport: Transport) -> Vec<&Operation> {
        self.operations
            .iter()
            .filter(|operation| self.transports_for(operation.name).contains(&transport))
            .collect()
    }

    fn validate(&self) -> Result<(), RegistryError> {
        self.validate_unique_bindings()?;
        self.validate_reasons()?;
        self.validate_exclusions()
    }

    fn validate_unique_bindings(&self) -> Result<(), RegistryError> {
        let mut routes: HashMap<(&str, &str), &str> = HashMap::new();
        let mut cli_paths: HashMap<&[&str], &str> = HashMap::new();
        let mut tools: HashMap<String, &str> = HashMap::new();
        for operation in &self.operations {
            let route_key = (operation.route.method.as_str(), operation.route.path);
            if let Some(owner) = routes.insert(route_key, operation.name) {
                return Err(RegistryError(format!(
                    "route {} {} is claimed by both {owner} and {}",
                    route_key.0, route_key.1, operation.name
                )));
            }
            if let Some(owner) = cli_paths.insert(operation.cli.path, operation.name) {
                return Err(RegistryError(format!(
                    "CLI path {} is claimed by both {owner} and {}",
                    operation.cli.path.join(" "),
                    operation.name
                )));
            }
            if let Some(owner) = tools.insert(operation.mcp_tool_name(), operation.name) {
                return Err(RegistryError(format!(
                    "MCP tool {} is claimed by both {owner} and {}",
                    operation.mcp_tool_name(),
                    operation.name
                )));
            }
        }
        self.validate_cli_tree(&cli_paths)
    }

    /// A CLI command cannot be both a leaf and a group.
    fn validate_cli_tree(&self, cli_paths: &HashMap<&[&str], &str>) -> Result<(), RegistryError> {
        let mut paths: Vec<(&[&str], &str)> = cli_paths
            .iter()
            .map(|(path, owner)| (*path, *owner))
            .collect();
        paths.sort();
        for (path, owner) in &paths {
            for (other, other_owner) in &paths {
                if other != path && other.starts_with(path) {
                    return Err(RegistryError(format!(
                        "CLI path '{}' ({owner}) is a prefix of '{}' ({other_owner}); a command \
                         cannot be both a leaf and a group",
                        path.join(" "),
                        other.join(" ")
                    )));
                }
            }
        }
        Ok(())
    }

    /// Every mutating operation must take a required reason.
    fn validate_reasons(&self) -> Result<(), RegistryError> {
        for operation in &self.operations {
            if operation.mutating && !operation.reason_required {
                return Err(RegistryError(format!(
                    "{} is mutating but its parameters have no reason field",
                    operation.name
                )));
            }
        }
        Ok(())
    }

    /// Every operation has a recorded schema, and no schema names an operation
    /// that does not exist. Checked against the parsed map, which is also what
    /// the dump and the MCP tool list read, so a mismatch fails here rather
    /// than at the first call.
    fn validate_schemas(&self) -> Result<(), RegistryError> {
        let schemas = parsed_schemas();
        for operation in &self.operations {
            if !schemas.contains_key(operation.name) {
                return Err(RegistryError(format!(
                    "{} has no recorded schema; regenerate with scripts/gen_registry.py",
                    operation.name
                )));
            }
        }
        for name in schemas.keys() {
            if !self.contains(name) {
                return Err(RegistryError(format!(
                    "schema recorded for '{name}', which is not a registered operation"
                )));
            }
        }
        Ok(())
    }

    fn validate_exclusions(&self) -> Result<(), RegistryError> {
        for (name, _) in LOCAL_ONLY.iter().chain(HTTP_ONLY) {
            if !self.contains(name) {
                return Err(RegistryError(format!(
                    "parity exclusion names '{name}', which is not a registered operation — the \
                     list is stale"
                )));
            }
        }
        for (local, _) in LOCAL_ONLY {
            if HTTP_ONLY.iter().any(|(name, _)| name == local) {
                return Err(RegistryError(format!(
                    "operations excluded twice: [{local}]"
                )));
            }
        }
        Ok(())
    }
}

/// Build the registry every adapter uses.
pub fn default_registry() -> OperationRegistry {
    OperationRegistry::new(operations::build_operations()).expect("the default registry is valid")
}

/// One operation as every transport sees it. The field order is the manifest's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OperationManifest {
    pub name: &'static str,
    pub summary: &'static str,
    pub scope: &'static str,
    pub mutating: bool,
    pub reason_required: bool,
    pub http_method: &'static str,
    pub http_path: &'static str,
    pub mcp_tool: String,
    pub cli_path: Vec<&'static str>,
    /// Recorded from pydantic until the models are ported to Rust. See
    /// `schemas.rs` for why this is a generated table and not a derivation.
    pub params_schema: serde_json::Value,
    pub result_schema: serde_json::Value,
    pub transports: Vec<&'static str>,
    /// `None` serialises as `null`, matching pydantic's `model_dump`, which
    /// emits the field for every operation rather than omitting it.
    pub exclusion: Option<&'static str>,
    pub exclusion_reason: Option<&'static str>,
}

fn exclusion_of(name: &str) -> Option<(&'static str, &'static str)> {
    if let Some((_, reason)) = LOCAL_ONLY.iter().find(|(excluded, _)| *excluded == name) {
        return Some(("local_only", *reason));
    }
    HTTP_ONLY
        .iter()
        .find(|(excluded, _)| *excluded == name)
        .map(|(_, reason)| ("http_only", *reason))
}

/// Parameter and result schemas, parsed once. `validate_schemas` refuses a
/// registry whose table does not match the operation set, so a lookup that
/// misses here is a bug in that check, not a state to paper over.
fn parsed_schemas() -> &'static HashMap<&'static str, (serde_json::Value, serde_json::Value)> {
    use std::sync::LazyLock;
    static PARSED: LazyLock<HashMap<&str, (serde_json::Value, serde_json::Value)>> =
        LazyLock::new(|| {
            schemas::SCHEMAS
                .iter()
                .map(|(name, params, result)| {
                    (
                        *name,
                        (
                            serde_json::from_str(params).expect("generated params schema is JSON"),
                            serde_json::from_str(result).expect("generated result schema is JSON"),
                        ),
                    )
                })
                .collect()
        });
    &PARSED
}

/// The parameter schema an operation's MCP tool advertises as `inputSchema`.
pub fn params_schema_for(name: &str) -> Option<&'static serde_json::Value> {
    parsed_schemas().get(name).map(|(params, _)| params)
}

/// The `registry.dump` result: every operation, in registration order.
pub fn dump() -> serde_json::Value {
    let registry = default_registry();
    let operations: Vec<OperationManifest> = registry
        .iter()
        .map(|operation| {
            let (exclusion, exclusion_reason) = match exclusion_of(operation.name) {
                Some((kind, reason)) => (Some(kind), Some(reason)),
                None => (None, None),
            };
            let (params_schema, result_schema) = parsed_schemas()
                .get(operation.name)
                .map(|(params, result)| (params.clone(), result.clone()))
                .expect("validate_schemas guarantees a schema for every operation");
            OperationManifest {
                name: operation.name,
                summary: operation.summary,
                scope: operation.scope.as_str(),
                mutating: operation.mutating,
                reason_required: operation.reason_required,
                http_method: operation.route.method.as_str(),
                http_path: operation.route.path,
                mcp_tool: operation.mcp_tool_name(),
                cli_path: operation.cli.path.to_vec(),
                params_schema,
                result_schema,
                transports: registry
                    .transports_for(operation.name)
                    .iter()
                    .map(|transport| transport.as_str())
                    .collect(),
                exclusion,
                exclusion_reason,
            }
        })
        .collect();
    serde_json::json!({ "operations": operations })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> OperationRegistry {
        default_registry()
    }

    #[test]
    fn the_default_registry_builds_and_covers_every_exclusion() {
        let registry = registry();
        assert!(registry.len() > 100, "got {}", registry.len());
        for (name, _) in LOCAL_ONLY.iter().chain(HTTP_ONLY) {
            assert!(
                registry.contains(name),
                "{name} is excluded but not registered"
            );
        }
    }

    #[test]
    fn transports_follow_the_exclusion_lists() {
        let registry = registry();
        for (name, _) in LOCAL_ONLY {
            assert_eq!(registry.transports_for(name), vec![Transport::Cli]);
        }
        for (name, _) in HTTP_ONLY {
            assert_eq!(registry.transports_for(name), vec![Transport::Http]);
        }
        assert_eq!(
            registry.transports_for("work.get"),
            vec![Transport::Cli, Transport::Http, Transport::Mcp]
        );
    }

    #[test]
    fn a_mutating_operation_requires_a_reason() {
        let mut operations = operations::build_operations();
        let work_create = operations
            .iter_mut()
            .find(|operation| operation.name == "work.create")
            .unwrap();
        work_create.reason_required = false;
        let error = OperationRegistry::new(operations).unwrap_err();
        assert!(error.0.contains("work.create"), "{}", error.0);
    }

    #[test]
    fn a_stale_exclusion_is_rejected() {
        let error = OperationRegistry::new(vec![]).unwrap_err();
        assert!(error.0.contains("parity exclusion"), "{}", error.0);
    }

    #[test]
    fn a_leaf_and_group_cli_path_is_rejected() {
        let mut operations = operations::build_operations();
        operations.push(Operation::new(
            "status.child",
            "a child of a leaf",
            Scope::Read,
            false,
            HttpRoute::new(HttpMethod::Get, "/status/child"),
            CliBinding::new(&["status", "x"]),
        ));
        let error = OperationRegistry::new(operations).unwrap_err();
        assert!(
            error.0.contains("cannot be both a leaf and a group"),
            "{}",
            error.0
        );
    }

    #[test]
    fn bindings_are_unique() {
        let mut operations = operations::build_operations();
        let work_get = operations
            .iter()
            .find(|operation| operation.name == "work.get")
            .unwrap()
            .clone();
        operations.push(Operation::new(
            "work.get.again",
            "a duplicate route",
            Scope::Read,
            false,
            HttpRoute::new(work_get.route.method, work_get.route.path),
            work_get.cli.clone(),
        ));
        let error = OperationRegistry::new(operations).unwrap_err();
        assert!(error.0.contains("claimed by both"), "{}", error.0);
    }

    #[test]
    fn not_ported_is_a_failure() {
        let registry = registry();
        let operation = registry.get("work.create").unwrap();
        let error = operation.run(None, serde_json::json!({})).unwrap_err();
        assert!(
            error.message().contains("not been ported"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn dump_names_every_operation_and_its_exclusion() {
        let manifest = dump();
        let operations = manifest["operations"].as_array().unwrap();
        let names: Vec<&str> = operations
            .iter()
            .map(|operation| operation["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), registry().len());
        assert!(names.contains(&"registry.dump"));
        let init = operations
            .iter()
            .find(|operation| operation["name"] == "init")
            .unwrap();
        assert_eq!(init["exclusion"], "local_only");
        assert_eq!(init["transports"], serde_json::json!(["cli"]));
        assert!(init["exclusion_reason"]
            .as_str()
            .unwrap()
            .contains("local data directory"));
        let shared = operations
            .iter()
            .find(|operation| operation["name"] == "work.get")
            .unwrap();
        assert!(shared.get("exclusion").unwrap().is_null());
        assert_eq!(shared["mcp_tool"], "work_get");
        assert_eq!(shared["cli_path"], serde_json::json!(["work", "get"]));
    }

    #[test]
    fn the_dump_matches_the_python_golden() {
        // Recorded by `scripts/gen_registry.py` from Python's own registry
        // dump. Compared with no normaliser, so a drift in any field fails.
        // The golden is unkeyed (tests/parity/golden/registry.json, not under
        // golden/<sha>/) on purpose: it describes the registry, which changes
        // with the operation set rather than with the parity script, and
        // `gen_registry.py --check` re-records it from the live Python.
        let golden_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/parity/golden/registry.json");
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&golden_path).unwrap()).unwrap();
        let dump = dump();
        assert_eq!(dump, golden, "registry dump drifted from the Python golden");
        // Value equality ignores key order, and the manifest's field order is
        // part of the contract, so compare the text too.
        assert_eq!(
            serde_json::to_string_pretty(&dump).unwrap(),
            serde_json::to_string_pretty(&golden).unwrap(),
            "registry field order drifted"
        );
    }

    #[test]
    fn lookup_misses_are_errors() {
        let registry = registry();
        assert!(registry.get("nope").is_err());
        assert!(registry.by_mcp_tool("nope").is_err());
        assert_eq!(registry.by_mcp_tool("work_get").unwrap().name, "work.get");
    }
}
