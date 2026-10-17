//! Parameter and result models, with the JSON Schemas derived from them.
//!
//! Ports the models in `src/vogt/application/models.py` one operation at a
//! time. An operation listed in [`DERIVED`] takes its schemas from the types
//! here: `schemars` derives them, and `scripts/gen_registry.py` leaves those
//! operations out of the table it records from pydantic. Every other
//! operation keeps its recorded schema until its models are ported.
//!
//! The derived schemas are checked against Python's live `registry.dump` by
//! `tests/test_rust_schemas.py`, so a Rust model that validated differently
//! from the Python one fails there rather than at the swap.

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// The operations whose schemas are derived from the types in this module.
///
/// `scripts/gen_registry.py` reads this list to decide which schemas to
/// record, so a name added here stops being recorded on the next regeneration.
pub const DERIVED: &[&str] = &["init", "migrate", "observations.prune", "user.remove"];

/// `(operation name, params schema, result schema)` for every name in
/// [`DERIVED`], as `registry::dump()` emits them.
pub fn derived_schemas() -> Vec<(&'static str, serde_json::Value, serde_json::Value)> {
    vec![
        ("init", schema_of::<InitParams>(), schema_of::<InitResult>()),
        (
            "migrate",
            schema_of::<MigrateParams>(),
            schema_of::<MigrateResult>(),
        ),
        (
            "observations.prune",
            schema_of::<PruneParams>(),
            schema_of::<PruneResult>(),
        ),
        (
            "user.remove",
            schema_of::<RemoveUserParams>(),
            schema_of::<RemoveUserResult>(),
        ),
    ]
}

/// The schema `schemars` derives for `T`, shaped as pydantic's is: no draft
/// marker at the root, and keys in the order the dump records them.
fn schema_of<T: JsonSchema>() -> serde_json::Value {
    let mut value =
        serde_json::to_value(schemars::schema_for!(T)).expect("a derived schema is JSON");
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
    }
    canonical(value)
}

/// Sorts the keys of every schema node, as pydantic's dump does, and keeps
/// the `properties` map in declaration order, which pydantic does too. The
/// dump is compared text-for-text, so the order is part of what is checked.
fn canonical(value: serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(key, child)| {
                    let child = if key == "properties" {
                        keep_order(child)
                    } else {
                        canonical(child)
                    };
                    (key, child)
                })
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(entries.into_iter().collect::<Map<String, Value>>())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// A `properties` map: each property's schema is canonical, the names stay
/// in the order the fields were declared.
fn keep_order(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(name, schema)| (name, canonical(schema)))
                .collect(),
        ),
        other => other,
    }
}

/// `int` in Python is `"type": "integer"` with no format. `schemars` would
/// add `"format": "int64"` or `"uint32"` to a Rust integer, which pydantic
/// does not emit, so every integer field names this instead.
fn integer(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "integer" })
}

fn not_configured() -> String {
    "not_configured".to_string()
}

// `init` and `migrate` take nothing. The data directory comes from configuration.

/// `init` takes nothing: the data directory comes from configuration.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct InitParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitResult {
    #[schemars(title = "Instance Id")]
    pub instance_id: String,
    #[schemars(title = "Data Dir")]
    pub data_dir: String,
    #[schemars(title = "Created")]
    pub created: bool,
    #[schemars(title = "Declared Schema Version", schema_with = "integer")]
    pub declared_schema_version: i64,
    #[schemars(title = "Observed Schema Version", schema_with = "integer")]
    pub observed_schema_version: i64,
    #[schemars(title = "Migrations Applied")]
    pub migrations_applied: Vec<String>,
    // What became of `bootstrap_core_token_file`. Plain comments, not doc
    // comments: pydantic's `#:` comments do not reach the schema.
    #[serde(default = "not_configured")]
    #[schemars(title = "Bootstrap Core Token", default = "not_configured")]
    pub bootstrap_core_token: String,
    // The same, for `bootstrap_agent_token_file`.
    #[serde(default = "not_configured")]
    #[schemars(title = "Bootstrap Agent Token", default = "not_configured")]
    pub bootstrap_agent_token: String,
}

/// `migrate` takes nothing: the data directory comes from configuration.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct MigrateParams {}

/// What `migrate` moved, and whether anything is still behind.
///
/// Reports the expected versions beside the applied ones for the same reason
/// `/health/ready` does: the applied number alone cannot answer
/// "is this instance current?", which is the only question the operator
/// running this has.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrateResult {
    #[schemars(title = "Data Dir")]
    pub data_dir: String,
    #[schemars(title = "Declared Schema Version", schema_with = "integer")]
    pub declared_schema_version: i64,
    #[schemars(title = "Observed Schema Version", schema_with = "integer")]
    pub observed_schema_version: i64,
    #[schemars(title = "Declared Schema Expected", schema_with = "integer")]
    pub declared_schema_expected: i64,
    #[schemars(title = "Observed Schema Expected", schema_with = "integer")]
    pub observed_schema_expected: i64,
    #[schemars(title = "Migrations Applied")]
    pub migrations_applied: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PruneParams {
    #[schemars(title = "Reason", length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PruneResult {
    #[schemars(title = "Removed", schema_with = "integer")]
    pub removed: i64,
    #[schemars(title = "Kept Latest", schema_with = "integer")]
    pub kept_latest: i64,
    #[schemars(title = "Kept Referenced", schema_with = "integer")]
    pub kept_referenced: i64,
    #[schemars(title = "Horizon Days", schema_with = "integer")]
    pub horizon_days: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveUserParams {
    #[schemars(title = "Username", length(min = 1))]
    pub username: String,
    #[schemars(title = "Reason", length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveUserResult {
    #[schemars(title = "Username")]
    pub username: String,
    #[schemars(title = "Sessions Revoked", schema_with = "integer")]
    pub sessions_revoked: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_names_are_exactly_the_listed_set() {
        let names: Vec<&str> = derived_schemas().iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names, DERIVED);
    }

    #[test]
    fn init_params_takes_nothing() {
        let (_, params, _) = derived_schemas().remove(0);
        assert_eq!(params["title"], "InitParams");
        assert_eq!(params["additionalProperties"], false);
        assert_eq!(
            params["description"],
            "`init` takes nothing: the data directory comes from configuration."
        );
        assert!(params.get("$schema").is_none());
    }

    #[test]
    fn a_defaulted_field_is_not_required() {
        let (_, _, result) = derived_schemas().remove(0);
        let required = result["required"].as_array().expect("required list");
        assert!(!required.iter().any(|name| name == "bootstrap_core_token"));
        assert_eq!(
            result["properties"]["bootstrap_core_token"]["default"],
            "not_configured"
        );
    }
}
