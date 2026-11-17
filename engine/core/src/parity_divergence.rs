//! Intentional Rust-only divergences from the Python parity goldens (WI-4317).
//!
//! Operator decision, 2026-11-16: the Rust core may add these schema-visible
//! changes that Python does not have. The goldens are Python recordings and
//! workers never re-record them (WI-2661), so each golden-comparing test
//! adopts the Rust value for exactly the fields below and compares the rest
//! as before. Add an entry here, with a work item, only on an operator ruling.
//!
//! - `project.list`: `mode` parameter (`full` default over REST and core, `summary` default only for the MCP tool, `full` to ask for everything), and result
//!   rows that are `ProjectSummary` or `ProjectListing` (`ProjectRow`).
//! - `session.start`: optional `provider` parameter (refused when unregistered
//!   or not serving the model).
//! - `SessionResult.warning`: set by `session.start` when no registered LLM
//!   provider serves the requested model. Every operation returning
//!   `SessionResult` carries the field in its result schema.
//! - `SessionSummary.reported_status` (WI-4322): the program's OSC 7501 report,
//!   in the schema of every operation that returns a `SessionSummary`.
//! - The `work.create` refusal text gained a `local_only=true` hint; it is not
//!   in any golden (messages are runtime strings).

use serde_json::Value;

/// A location inside a JSON document, as object keys.
type Path = &'static [&'static str];

/// Registry operation fields that are a recorded divergence: the whole
/// `params_schema` / `result_schema` of `project.list` (its rows are a union
/// now), `session.start` params_schema (field order is compared, so the new
/// `provider` property cannot be spliced in alone), and only the `warning` property of every `SessionResult` operation.
const OPERATION_FIELDS: &[(&str, Path)] = &[
    ("project.list", &["params_schema"]),
    ("project.list", &["result_schema"]),
    ("session.start", &["params_schema"]),
    ("session.start", &["result_schema", "properties", "warning"]),
    ("session.stop", &["result_schema", "properties", "warning"]),
    (
        "session.hibernate",
        &["result_schema", "properties", "warning"],
    ),
    ("session.wake", &["result_schema", "properties", "warning"]),
    (
        "session.keep_awake",
        &["result_schema", "properties", "warning"],
    ),
    (
        "session.rename",
        &["result_schema", "properties", "warning"],
    ),
    (
        "session.remove",
        &["result_schema", "properties", "warning"],
    ),
    (
        "session.set_role",
        &["result_schema", "properties", "warning"],
    ),
];

/// Operations whose result embeds `SessionSummary`; its `reported_status`
/// property is a recorded divergence (WI-4322).
const SESSION_SUMMARY_OPS: &[&str] = &[
    "work.create",
    "work.get",
    "work.update",
    "work.transition",
    "work.relate",
    "work.unrelate",
    "work.bind_branch",
    "session.start",
    "session.list",
    "session.stop",
    "session.sweep",
    "session.hibernate",
    "session.wake",
    "session.keep_awake",
    "session.rename",
    "session.remove",
    "session.set_role",
    "session.bind_work",
];

/// OpenAPI locations that are a recorded divergence. `/api/projects` keeps
/// every other field of the golden; the schemas are new or changed wholesale
/// except `SessionResult`, which gained one property.
const OPENAPI_FIELDS: &[Path] = &[
    &["paths", "/api/projects", "get", "parameters"],
    &["paths", "/api/projects", "get", "responses", "200"],
    &["components", "schemas", "ListMode"],
    &["components", "schemas", "ProjectListResult"],
    &["components", "schemas", "ProjectRow"],
    &["components", "schemas", "ProjectSummary"],
    &[
        "components",
        "schemas",
        "SessionResult",
        "properties",
        "warning",
    ],
    &[
        "components",
        "schemas",
        "SessionSummary",
        "properties",
        "reported_status",
    ],
    &[
        "components",
        "schemas",
        "StartSessionParams",
        "properties",
        "provider",
    ],
];

/// MCP tools whose `inputSchema` is a recorded divergence.
const TOOLS: &[&str] = &["project_list"];

fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |node, key| node.get(*key))
}

/// Copy `ours` at `path` into `golden`, creating the object keys on the way
/// only for the last key (the parents must already exist in the golden).
fn adopt(golden: &mut Value, ours: &Value, path: &[&str]) {
    let Some(value) = at(ours, path) else {
        return;
    };
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut node = golden;
    for key in parents {
        let Some(next) = node.get_mut(*key) else {
            return;
        };
        node = next;
    }
    if let Some(map) = node.as_object_mut() {
        map.insert((*last).to_string(), value.clone());
    }
}

/// Adopt our recorded-divergence fields into the golden registry dump.
pub fn patch_registry(golden: &mut Value, ours: &Value) {
    let (Some(g_ops), Some(o_ops)) = (
        golden["operations"].as_array_mut(),
        ours["operations"].as_array(),
    ) else {
        return;
    };
    for g in g_ops {
        let name = g["name"].as_str().unwrap_or_default().to_string();
        let Some(o) = o_ops.iter().find(|o| o["name"] == name.as_str()) else {
            continue;
        };
        for (operation, path) in OPERATION_FIELDS {
            if *operation == name {
                adopt(g, o, path);
            }
        }
        if SESSION_SUMMARY_OPS.contains(&name.as_str()) {
            adopt(
                g,
                o,
                &[
                    "result_schema",
                    "$defs",
                    "SessionSummary",
                    "properties",
                    "reported_status",
                ],
            );
        }
    }
}

/// Adopt our recorded-divergence fields into the golden OpenAPI document.
pub fn patch_openapi(golden: &mut Value, ours: &Value) {
    for path in OPENAPI_FIELDS {
        adopt(golden, ours, path);
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
                g["inputSchema"] = o["inputSchema"].clone();
            }
        }
        if name == "session_start" {
            if let Some(o) = o_tools.iter().find(|o| o["name"] == "session_start") {
                adopt(g, o, &["inputSchema", "properties", "provider"]);
            }
        }
    }
}
