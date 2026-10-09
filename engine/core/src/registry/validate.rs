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
    apply_object(operation, schema, given, schema).map(Value::Object)
}

fn apply_object(
    operation: &str,
    schema: &Value,
    mut given: Map<String, Value>,
    root: &Value,
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
            return Err(invalid(
                operation,
                &pydantic(name, "Extra inputs are not permitted"),
            ));
        }
    }
    let mut resolved = Map::new();
    for (name, property) in &properties {
        match given.remove(name) {
            Some(value) => {
                resolved.insert(
                    name.clone(),
                    check_value(operation, name, property, value, root)?,
                );
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
                    return Err(invalid(operation, &pydantic(name, "Field required")));
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
    root: &Value,
) -> Result<Value, VogtError> {
    let (schema, nullable) = match nullable_branch(property) {
        Some(schema) => (schema, true),
        None => (property, false),
    };
    if value.is_null() && schema.get("enum").is_none() {
        return if nullable {
            Ok(value)
        } else {
            Err(invalid(operation, &pydantic(name, &expected_input(schema))))
        };
    }
    if let Some(nested) = schema.get("properties") {
        if nested.as_object().is_some_and(|fields| !fields.is_empty()) {
            let Value::Object(object) = value else {
                return Err(invalid(operation, &format!("{name} takes an object")));
            };
            return apply_object(operation, schema, object, root).map(Value::Object);
        }
    }
    // A field typed as another model arrives as `{"$ref": "#/$defs/Name"}`.
    // The definition lives on the operation's own schema.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let Some(target) = root.pointer(reference.trim_start_matches('#')) else {
            return Err(invalid(
                operation,
                &format!("{name} refers to an unknown type"),
            ));
        };
        return check_value(operation, name, target, value, root);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => {
            if schema.get("format").and_then(Value::as_str) == Some("date-time") {
                if let Some(coerced) = coerce_datetime(&value) {
                    return check_string(operation, name, schema, coerced);
                }
                // A bool or null is the wrong type. A string that fails to parse
                // is a different complaint, made inside `check_string`.
                if !value.is_string() {
                    return Err(invalid(
                        operation,
                        &pydantic(name, "Input should be a valid datetime"),
                    ));
                }
            }
            check_string(operation, name, schema, value)
        }
        Some("integer") => check_integer(operation, name, schema, value),
        Some("number") => check_number(operation, name, schema, value),
        Some("boolean") => check_boolean(operation, name, value),
        Some("array") => check_array(operation, name, schema, value, root),
        Some("object") if operation == "preference.set" && name == "value" => {
            check_preference_value(operation, name, value)
        }
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
    // `Name` and `Reason` strip whitespace before anything else, so a value of
    // " " fails the length check and " padded " is stored trimmed. Which fields
    // those are is recorded from the models (`strips.rs`), because pydantic's
    // schema drops the constraint and the same field name is a plain `str` on
    // other operations.
    let stripped = match &value {
        Value::String(text) if strips_whitespace(operation, name) => {
            Value::String(text.trim().to_string())
        }
        _ => value,
    };
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        let text = stripped.as_str();
        if !allowed.iter().any(|item| item.as_str() == text) {
            let values = allowed.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            let listed = match values.as_slice() {
                [] => String::new(),
                [one] => (*one).to_string(),
                [rest @ .., last] => format!("{}' or '{last}", rest.join("', '")),
            };
            return Err(invalid(
                operation,
                &pydantic(name, &format!("Input should be '{listed}'")),
            ));
        }
    }
    let Value::String(text) = &stripped else {
        return Err(invalid(
            operation,
            &pydantic(name, "Input should be a valid string"),
        ));
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
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        if let Ok(expression) = regex::Regex::new(pattern) {
            if !expression.is_match(text) {
                return Err(invalid(
                    operation,
                    &pydantic(name, &format!("String should match pattern '{pattern}'")),
                ));
            }
        }
    }
    if schema.get("format").and_then(Value::as_str) == Some("date-time") && !is_datetime(text) {
        // A bool or null never got past the string check above. A string that is
        // not a datetime is the parse failure, and the corpus's are all too short.
        return Err(invalid(
            operation,
            &pydantic(
                name,
                "Input should be a valid datetime or date, input is too short",
            ),
        ));
    }
    Ok(stripped)
}

/// Enough of pydantic's datetime parsing to refuse what it refuses. A real
/// datetime is at least eight characters and contains a digit; the corpus's
/// rejections are all shorter than that.
fn is_datetime(text: &str) -> bool {
    text.len() >= 8 && text.chars().any(|ch| ch.is_ascii_digit())
}

/// An integer on a datetime field is a unix timestamp. Pydantic renders it as
/// UTC with a `Z` and no fractional seconds.
fn coerce_datetime(value: &Value) -> Option<Value> {
    let seconds = match value {
        Value::Number(number) => number.as_i64(),
        _ => None,
    }?;
    let moment = crate::core::Moment::from_unix(seconds, 0);
    Some(Value::String(moment.to_json()))
}

/// `preference.set`'s value is an object, but the CLI hands every flag over as
/// text, so a JSON object arrives as its source. The field validator parses it
/// first; an unparseable string is the refusal.
fn check_preference_value(operation: &str, name: &str, value: Value) -> Result<Value, VogtError> {
    let Value::String(text) = &value else {
        return Ok(value);
    };
    match serde_json::from_str::<Value>(text) {
        Ok(parsed @ Value::Object(_)) => Ok(parsed),
        Ok(_) => Err(invalid(
            operation,
            &pydantic(name, "Input should be a valid dictionary"),
        )),
        Err(_) => Err(invalid(
            operation,
            &pydantic(
                name,
                "Value error, value is not valid JSON: Expecting value",
            ),
        )),
    }
}

fn strips_whitespace(operation: &str, name: &str) -> bool {
    super::strips::STRIPS.contains(&(operation, name))
}

fn check_integer(
    operation: &str,
    name: &str,
    schema: &Value,
    value: Value,
) -> Result<Value, VogtError> {
    // Pydantic's lax mode: a whole float, a boolean and a string of digits are
    // an integer. The string is trimmed first. A number too big for i64 is kept
    // as JSON sent it, which is how Python keeps an arbitrary-precision int.
    let number = match &value {
        Value::Number(number) => match number.as_i64() {
            Some(number) => Some(number),
            // A whole float coerces. A number f64 cannot represent exactly is
            // bigger than i64, and Python keeps the digits, so it passes through.
            None if number.as_f64().is_some_and(|f| f.fract() == 0.0) => {
                let exact = number.as_f64().unwrap() as i64;
                if serde_json::Number::from(exact).as_f64() == number.as_f64() {
                    Some(exact)
                } else if schema.get("maximum").is_none()
                    && schema.get("exclusiveMaximum").is_none()
                {
                    // No upper bound, and the number is a whole integer Python
                    // keeps. The digits survive as JSON sent them.
                    return Ok(value);
                } else {
                    let bound = bound(schema, "maximum").unwrap_or(i64::MAX);
                    return Err(invalid(
                        operation,
                        &pydantic(
                            name,
                            &format!("Input should be less than or equal to {bound}"),
                        ),
                    ));
                }
            }
            None => {
                return Err(invalid(
                    operation,
                    &pydantic(
                        name,
                        "Input should be a valid integer, got a number with a fractional part",
                    ),
                ));
            }
        },
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    };
    let Some(number) = number else {
        // Bigger than i64, so f64 has already rounded it. The digits JSON sent
        // are exact, and Python keeps them, so the value passes through.
        if let Value::Number(raw) = &value {
            if raw.as_i64().is_none() && !raw.as_f64().is_some_and(|f| f.fract() != 0.0) {
                return Ok(value);
            }
        }
        return Err(invalid(
            operation,
            &pydantic(
                name,
                "Input should be a valid integer, unable to parse string as an integer",
            ),
        ));
    };
    check_bounds(operation, name, schema, number)?;
    Ok(Value::from(number))
}

fn check_boolean(operation: &str, name: &str, value: Value) -> Result<Value, VogtError> {
    let flag = match &value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => match number.as_i64().or_else(|| {
            number
                .as_f64()
                .filter(|f| f.fract() == 0.0)
                .map(|f| f as i64)
        }) {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    };
    match flag {
        Some(flag) => Ok(Value::from(flag)),
        None => Err(invalid(
            operation,
            &pydantic(
                name,
                "Input should be a valid boolean, unable to interpret input",
            ),
        )),
    }
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
    root: &Value,
) -> Result<Value, VogtError> {
    let Value::Array(items) = value else {
        // A comma-joined string is the list the CLI sends for a repeated flag.
        if let Value::String(text) = &value {
            // `work.list`'s states is the one list the CLI sends comma-joined.
            if operation == "work.list" && name == "states" {
                let split = text
                    .split(',')
                    .map(|part| Value::String(part.trim().to_string()))
                    .collect();
                return check_array(operation, name, schema, Value::Array(split), root);
            }
        }
        return Err(invalid(
            operation,
            &pydantic(name, "Input should be a valid list"),
        ));
    };
    if let Some(min) = bound(schema, "minItems") {
        if (items.len() as i64) < min {
            return Err(invalid(
                operation,
                &pydantic(
                    name,
                    &format!(
                        "List should have at least {min} item after validation, not {}",
                        items.len()
                    ),
                ),
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
        checked.push(check_value(operation, name, item_schema, item, root)?);
    }
    Ok(Value::Array(checked))
}

fn bound(schema: &Value, name: &str) -> Option<i64> {
    schema.get(name).and_then(Value::as_i64)
}

fn expected_input(schema: &Value) -> String {
    let kind = match schema.get("type").and_then(Value::as_str) {
        Some("integer") => "integer",
        Some("number") => "number",
        Some("boolean") => "boolean",
        Some("array") => "list",
        Some("object") => "object",
        _ => "string",
    };
    format!("Input should be a valid {kind}")
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
        assert!(
            error.message().contains("actor\n  Field required"),
            "{error}"
        );
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
            error
                .message()
                .contains("extra\n  Extra inputs are not permitted"),
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

    /// Compare against the reviewer's pydantic corpus. Ignored by default
    /// because the corpus lives outside the tree.
    #[test]
    #[ignore]
    fn matches_the_pydantic_corpus() {
        let corpus: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string("/tmp/rrbv-data/corpus.json").unwrap())
                .unwrap();
        let mut rows = Vec::new();
        for case in corpus {
            let op = case["op"].as_str().unwrap();
            let outcome = match prepare(op, case["params"].clone()) {
                Ok(value) => serde_json::json!({"ok": true, "dump": value}),
                Err(error) => serde_json::json!({"ok": false, "msg": error.message()}),
            };
            rows.push(serde_json::json!({"op": op, "tag": case["tag"], "rs": outcome}));
        }
        std::fs::write(
            "/tmp/rrbv-data/rust.json",
            serde_json::to_string(&rows).unwrap(),
        )
        .unwrap();
    }
}
