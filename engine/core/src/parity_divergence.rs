//! Intentional Rust-only divergences from the Python parity goldens (WI-4317).
//!
//! Operator decision, 2026-11-16: the Rust core may add these schema-visible
//! changes that Python does not have. The goldens are Python recordings and
//! workers never re-record them (WI-2661), so each golden-comparing test
//! adopts the Rust value for exactly the entries below and compares the rest
//! as before. Add an entry here, with a work item, only on an operator ruling.
//!
//! - `project.list`: `mode` parameter (`summary` default, `full`), and result
//!   rows that are `ProjectSummary` or `ProjectListing` (`ProjectRow`).
//! - `SessionResult.warning`: set by `session.start` when no registered LLM
//!   provider serves the requested model. Every operation returning
//!   `SessionResult` carries the field in its result schema.
//! - The `work.create` refusal text gained a `local_only=true` hint; it is not
//!   in any golden (messages are runtime strings).

use serde_json::Value;

/// Registry operations whose params/result schema is a recorded divergence.
pub const OPERATIONS: &[&str] = &[
    "project.list",
    "session.start",
    "session.stop",
    "session.hibernate",
    "session.wake",
    "session.keep_awake",
    "session.rename",
    "session.remove",
    "session.set_role",
];

/// OpenAPI components that are a recorded divergence.
pub const COMPONENTS: &[&str] = &[
    "ListMode",
    "ProjectListResult",
    "ProjectRow",
    "ProjectSummary",
    "SessionResult",
];

/// OpenAPI paths that are a recorded divergence.
pub const PATHS: &[&str] = &["/api/projects"];

/// MCP tools whose `inputSchema` is a recorded divergence.
pub const TOOLS: &[&str] = &["project_list"];

fn adopt(golden: &mut Value, ours: &Value) {
    *golden = ours.clone();
}

/// Replace the golden registry dump's divergent operations with ours.
pub fn patch_registry(golden: &mut Value, ours: &Value) {
    let (Some(g_ops), Some(o_ops)) = (
        golden["operations"].as_array_mut(),
        ours["operations"].as_array(),
    ) else {
        return;
    };
    for g in g_ops {
        let name = g["name"].as_str().unwrap_or_default().to_string();
        if OPERATIONS.contains(&name.as_str()) {
            if let Some(o) = o_ops.iter().find(|o| o["name"] == name.as_str()) {
                adopt(g, o);
            }
        }
    }
}

/// Replace the golden OpenAPI document's divergent paths and components.
pub fn patch_openapi(golden: &mut Value, ours: &Value) {
    for path in PATHS {
        if !ours["paths"][*path].is_null() {
            golden["paths"][*path] = ours["paths"][*path].clone();
        }
    }
    for name in COMPONENTS {
        let theirs = &ours["components"]["schemas"][*name];
        if !theirs.is_null() {
            golden["components"]["schemas"][*name] = theirs.clone();
        }
    }
}

/// Replace the golden `tools/list` entries for divergent tools.
pub fn patch_tools(golden: &mut Value, ours: &Value) {
    let (Some(g_tools), Some(o_tools)) = (golden["tools"].as_array_mut(), ours["tools"].as_array())
    else {
        return;
    };
    for g in g_tools {
        let name = g["name"].as_str().unwrap_or_default().to_string();
        if TOOLS.contains(&name.as_str()) {
            if let Some(o) = o_tools.iter().find(|o| o["name"] == name.as_str()) {
                adopt(g, o);
            }
        }
    }
}
