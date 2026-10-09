//! Parameter validation, from the schemas recorded off pydantic.
//!
//! One place, called by `Operation::run`, so the CLI, HTTP and MCP all apply
//! the same defaults and bounds before a service sees the parameters. The
//! recorded schema is pydantic's own JSON schema, which is what makes the
//! rejection match Python's: a bound that is absent there is not invented here.

use serde_json::{Map, Value};

use crate::errors::VogtError;

/// Apply an operation's parameter schema: fill defaults, reject anything the
/// schema does not allow, and check types and bounds. A `Null` is an empty
/// object, matching the transports that pass one when there is no body.
pub fn prepare(operation: &str, params: Value) -> Result<Value, VogtError> {
    let Some(schema) = super::params_schema_for(operation) else {
        return Ok(params);
    };
    let params = if params.is_null() {
        Value::Object(Map::new())
    } else {
        params
    };
    let Value::Object(given) = params else {
        return Err(invalid(operation, &format!("{operation} takes an object")));
    };
    apply_object(operation, schema, given).map(Value::Object)
}

fn apply_object(
    operation: &str,
    schema: &Value,
    mut given: Map<String, Value>,
) -> Result<Map<String, Value>, VogtError> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
        let extra: Vec<String> = given
            .keys()
            .filter(|name| !properties.contains_key(*name))
            .cloned()
            .collect();
        if let Some(name) = extra.first() {
            return Err(invalid(operation, &format!("unexpected parameter {name}")));
        }
    }
    let mut resolved = Map::new();
    for (name, property) in &properties {
        match given.remove(name) {
            Some(value) => {
                resolved.insert(name.clone(), check_value(operation, name, property, value)?);
            }
            None => {
                if let Some(default) = property.get("default") {
                    // Pydantic records `null` as the default of an optional field
                    // the caller left out. The CLI omits such a flag entirely, so
                    // a null default is left absent rather than inserted.
                    if !default.is_null() {
                        resolved.insert(name.clone(), default.clone());
                    }
                } else if required_of(schema).iter().any(|item| item == name) {
                    return Err(invalid(operation, &format!("{name} is required")));
                }
            }
        }
    }
    Ok(resolved)
}

fn required_of(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn check_value(
    operation: &str,
    name: &str,
    property: &Value,
    value: Value,
) -> Result<Value, VogtError> {
    let (schema, nullable) = match nullable_branch(property) {
        Some(schema) => (schema, true),
        None => (property, false),
    };
    if value.is_null() {
        return if nullable {
            Ok(value)
        } else {
            Err(invalid(operation, &format!("{name} may not be null")))
        };
    }
    if let Some(nested) = schema.get("properties") {
        if nested.as_object().is_some_and(|fields| !fields.is_empty()) {
            let Value::Object(object) = value else {
                return Err(invalid(operation, &format!("{name} takes an object")));
            };
            return apply_object(operation, schema, object).map(Value::Object);
        }
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => check_string(operation, name, schema, value),
        Some("integer") => check_integer(operation, name, schema, value),
        Some("number") => check_number(operation, name, schema, value),
        Some("boolean") => match value {
            Value::Bool(_) => Ok(value),
            _ => Err(invalid(operation, &format!("{name} takes a boolean"))),
        },
        Some("array") => check_array(operation, name, schema, value),
        _ => Ok(value),
    }
}

/// `anyOf: [schema, {"type": "null"}]` is how pydantic writes an optional field.
fn nullable_branch(property: &Value) -> Option<&Value> {
    let branches = property.get("anyOf").and_then(Value::as_array)?;
    let null = branches
        .iter()
        .any(|branch| branch.get("type").and_then(Value::as_str) == Some("null"));
    if !null {
        return None;
    }
    branches
        .iter()
        .find(|branch| branch.get("type").and_then(Value::as_str) != Some("null"))
}

fn check_string(
    operation: &str,
    name: &str,
    schema: &Value,
    value: Value,
) -> Result<Value, VogtError> {
    let Value::String(text) = &value else {
        return Err(invalid(operation, &format!("{name} takes a string")));
    };
    if let Some(min) = bound(schema, "minLength") {
        if (text.chars().count() as i64) < min {
            let noun = if min == 1 { "character" } else { "characters" };
            return Err(invalid(
                operation,
                &pydantic(name, &format!("String should have at least {min} {noun}")),
            ));
        }
    }
    if let Some(max) = bound(schema, "maxLength") {
        if (text.chars().count() as i64) > max {
            let noun = if max == 1 { "character" } else { "characters" };
            return Err(invalid(
                operation,
                &pydantic(name, &format!("String should have at most {max} {noun}")),
            ));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|item| item.as_str() == Some(text)) {
            return Err(invalid(
                operation,
                &pydantic(name, "Input should be one of the allowed values"),
            ));
        }
    }
    Ok(value)
}

fn check_integer(
    operation: &str,
    name: &str,
    schema: &Value,
    value: Value,
) -> Result<Value, VogtError> {
    let number = match &value {
        Value::Number(number) => number.as_i64(),
        _ => None,
    };
    let Some(number) = number else {
        return Err(invalid(operation, &format!("{name} takes an integer")));
    };
    check_bounds(operation, name, schema, number)?;
    Ok(value)
}

fn check_number(
    operation: &str,
    name: &str,
    schema: &Value,
    value: Value,
) -> Result<Value, VogtError> {
    let number = match &value {
        Value::Number(number) => number.as_f64(),
        _ => None,
    };
    let Some(number) = number else {
        return Err(invalid(operation, &format!("{name} takes a number")));
    };
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if number < min {
            return Err(invalid(
                operation,
                &pydantic(
                    name,
                    &format!("Input should be greater than or equal to {min}"),
                ),
            ));
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if number > max {
            return Err(invalid(
                operation,
                &pydantic(
                    name,
                    &format!("Input should be less than or equal to {max}"),
                ),
            ));
        }
    }
    Ok(value)
}

fn check_bounds(operation: &str, name: &str, schema: &Value, number: i64) -> Result<(), VogtError> {
    if let Some(min) = bound(schema, "minimum") {
        if number < min {
            return Err(invalid(
                operation,
                &pydantic(
                    name,
                    &format!("Input should be greater than or equal to {min}"),
                ),
            ));
        }
    }
    if let Some(max) = bound(schema, "maximum") {
        if number > max {
            return Err(invalid(
                operation,
                &pydantic(
                    name,
                    &format!("Input should be less than or equal to {max}"),
                ),
            ));
        }
    }
    Ok(())
}

fn check_array(
    operation: &str,
    name: &str,
    schema: &Value,
    value: Value,
) -> Result<Value, VogtError> {
    let Value::Array(items) = value else {
        return Err(invalid(operation, &format!("{name} takes a list")));
    };
    if let Some(min) = bound(schema, "minItems") {
        if (items.len() as i64) < min {
            return Err(invalid(
                operation,
                &pydantic(name, &format!("List should have at least {min} item")),
            ));
        }
    }
    if let Some(max) = bound(schema, "maxItems") {
        if (items.len() as i64) > max {
            return Err(invalid(
                operation,
                &pydantic(name, &format!("List should have at most {max} items")),
            ));
        }
    }
    let Some(item_schema) = schema.get("items") else {
        return Ok(Value::Array(items));
    };
    let mut checked = Vec::with_capacity(items.len());
    for item in items {
        checked.push(check_value(operation, name, item_schema, item)?);
    }
    Ok(Value::Array(checked))
}

fn bound(schema: &Value, name: &str) -> Option<i64> {
    schema.get(name).and_then(Value::as_i64)
}

/// The field and the complaint, in the shape pydantic prints: the field on its
/// own line, the complaint indented under it. The type tag and the link pydantic
/// appends are its own and are not reproduced.
fn pydantic(field: &str, complaint: &str) -> String {
    format!("{field}\n  {complaint}")
}

fn invalid(operation: &str, detail: &str) -> VogtError {
    let model = super::params_schema_for(operation)
        .and_then(|schema| schema.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(operation);
    VogtError::InvalidRequest(format!(
        "invalid arguments for {operation}:\n1 validation error for {model}\n{detail}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare_issue(params: Value) -> Result<Value, VogtError> {
        prepare("token.issue", params)
    }

    #[test]
    fn omitted_scopes_default_to_read() {
        let resolved = prepare_issue(serde_json::json!({
            "actor": "human:ada",
            "name": "laptop",
            "reason": "needed",
        }))
        .unwrap();
        assert_eq!(resolved["scopes"], "read");
        assert!(resolved.get("expires_in_days").is_none());
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let error = prepare_issue(serde_json::json!({
            "actor": "human:ada",
            "name": "",
            "reason": "needed",
        }))
        .unwrap_err();
        assert!(
            error
                .message()
                .contains("name\n  String should have at least 1 character"),
            "{error}"
        );
    }

    #[test]
    fn expires_in_days_is_bounded() {
        for days in [0, 3651] {
            let error = prepare_issue(serde_json::json!({
                "actor": "human:ada",
                "name": "laptop",
                "reason": "needed",
                "expires_in_days": days,
            }))
            .unwrap_err();
            assert!(
                error.message().contains("expires_in_days"),
                "{days}: {error}"
            );
        }
        assert!(prepare_issue(serde_json::json!({
            "actor": "human:ada",
            "name": "laptop",
            "reason": "needed",
            "expires_in_days": 1,
        }))
        .is_ok());
    }

    #[test]
    fn a_missing_required_field_names_itself() {
        let error = prepare_issue(serde_json::json!({"name": "laptop"})).unwrap_err();
        assert!(error.message().contains("actor is required"), "{error}");
    }

    #[test]
    fn an_unknown_parameter_is_rejected() {
        let error = prepare_issue(serde_json::json!({
            "actor": "human:ada",
            "name": "laptop",
            "reason": "needed",
            "extra": true,
        }))
        .unwrap_err();
        assert!(
            error.message().contains("unexpected parameter extra"),
            "{error}"
        );
    }

    #[test]
    fn list_limit_is_bounded_and_defaults() {
        let resolved = prepare("token.list", serde_json::json!({})).unwrap();
        assert_eq!(resolved["limit"], 100);
        assert_eq!(resolved["include_revoked"], false);
        let error = prepare("token.list", serde_json::json!({"limit": 0})).unwrap_err();
        assert!(
            error
                .message()
                .contains("limit\n  Input should be greater than or equal to 1"),
            "{error}"
        );
        let error = prepare("token.list", serde_json::json!({"limit": 501})).unwrap_err();
        assert!(
            error
                .message()
                .contains("limit\n  Input should be less than or equal to 500"),
            "{error}"
        );
    }
}
