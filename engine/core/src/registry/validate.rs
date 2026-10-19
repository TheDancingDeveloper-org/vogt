//! Parameter validation, from the schemas recorded off pydantic.
//!
//! One place, called by `Operation::run`, so the CLI, HTTP and MCP all apply
//! the same defaults and bounds before a service sees the parameters. The
//! recorded schema is pydantic's own JSON schema, which is what makes the
//! rejection match Python's: a bound that is absent there is not invented here.

use serde_json::{Map, Value};

use crate::errors::{record_validation, FieldError, Loc, ValidationReport, VogtError};

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
        return Err(invalid(
            operation,
            &[FieldError::bare(&format!("{operation} takes an object"))],
        ));
    };
    let given_value = Value::Object(given.clone());
    let given = apply_aliases(operation, given)
        .map_err(|complaint| invalid(operation, &[model_error(&complaint, &given_value)]))?;
    let resolved = apply_object(operation, &[], schema, given, schema)
        .and_then(|resolved| after_fields(operation, resolved))
        .map_err(|problems| {
            let problems: Vec<FieldError> = problems
                .into_iter()
                .map(|problem| {
                    if problem.text.contains("[type=") {
                        problem
                    } else {
                        finish(&problem, &input_for(&problem.text, &given_value))
                    }
                })
                .collect();
            invalid(operation, &problems)
        })?;
    after_model(operation, &resolved)
        .map_err(|complaint| invalid(operation, &[model_error(&complaint, &given_value)]))?;
    Ok(Value::Object(resolved))
}

/// A complaint from a whole-model validator: pydantic reports it with an empty
/// location and the whole input as the value.
fn model_error(complaint: &str, input: &Value) -> FieldError {
    finish(&pydantic(&[], &format!("Value error, {complaint}")), input)
}

/// The tolerated aliases, the `apply_aliases` `mode="before"` model validators
/// on `ProjectBriefParams`, `GetWorkParams`, `ListWorkParams` and
/// `TransitionWorkParams`. An alias is taken only when the canonical field is
/// absent; naming both is refused.
fn apply_aliases(
    operation: &str,
    mut given: Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let aliases: &[(&str, &str)] = match operation {
        "project.brief" => &[("project", "slug"), ("id", "slug")],
        "work.get" => &[("id", "ref")],
        "work.list" => &[
            ("status", "states"),
            ("state", "states"),
            ("text", "query"),
            ("search", "query"),
            ("q", "query"),
        ],
        "work.transition" => &[("id", "ref"), ("to", "to_state"), ("state", "to_state")],
        _ => &[],
    };
    for (alias, field) in aliases {
        let Some(value) = given.remove(*alias) else {
            continue;
        };
        if given.contains_key(*field) {
            return Err(format!(
                "{operation}: '{alias}' is an alias of '{field}'; pass one of them, not both"
            ));
        }
        given.insert((*field).to_string(), value);
    }
    if operation == "work.list" {
        if let Some(Value::String(text)) = given.get("states") {
            let parts = text
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| Value::String(part.to_string()))
                .collect();
            given.insert("states".to_string(), Value::Array(parts));
        }
    }
    Ok(given)
}

/// Pydantic `field_validator`s in the default (after) mode: they see a value
/// that already passed its type check.
fn after_fields(
    operation: &str,
    mut resolved: Map<String, Value>,
) -> Result<Map<String, Value>, Vec<FieldError>> {
    let complain = |name: &str, complaint: &str, input: &Value| {
        vec![finish(
            &pydantic(
                &[Loc::Field(name.to_string())],
                &format!("Value error, {complaint}"),
            ),
            input,
        )]
    };
    match operation {
        // `GetManyWorkParams._refs_are_refs`.
        "work.get_many" => {
            if let Some(refs @ Value::Array(items)) = resolved.get("refs") {
                if items.iter().any(|item| {
                    item.as_str()
                        .is_some_and(|text| text.is_empty() || text.chars().count() > 200)
                }) {
                    return Err(complain("refs", "each ref is 1-200 characters", refs));
                }
            }
        }
        // `ListSessionsParams._starter_is_an_id`.
        "session.list" => {
            if let Some(Value::String(text)) = resolved.get("started_by_session") {
                if text.trim().is_empty() || text.chars().count() > 200 {
                    let input = Value::String(text.clone());
                    return Err(complain(
                        "started_by_session",
                        "started_by_session is 1-200 characters",
                        &input,
                    ));
                }
                let trimmed = Value::String(text.trim().to_string());
                resolved.insert("started_by_session".to_string(), trimmed);
            }
        }
        // `SessionWaitAnyParams._cursor_not_negative`.
        "session.wait_any" => {
            if let Some(cursor @ Value::Number(number)) = resolved.get("cursor") {
                if number.as_i64().is_some_and(|n| n < 0)
                    || number.as_f64().is_some_and(|n| n < 0.0)
                {
                    return Err(complain("cursor", "cursor is at least 0", cursor));
                }
            }
        }
        _ => {}
    }
    Ok(resolved)
}

/// Pydantic `model_validator(mode="after")`s, run once every field passed.
fn after_model(operation: &str, resolved: &Map<String, Value>) -> Result<(), String> {
    // `CommentParams._handoff_matches_kind`.
    if operation == "work.comment" {
        let kind = resolved
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("note");
        let handoff = resolved
            .get("handoff")
            .is_some_and(|value| !value.is_null());
        if kind == "handoff" && !handoff {
            return Err("a hand-off comment requires handoff fields".to_string());
        }
        if kind == "note" && handoff {
            return Err("handoff fields belong on a comment of kind handoff".to_string());
        }
    }
    Ok(())
}

fn apply_object(
    operation: &str,
    location: &[Loc],
    schema: &Value,
    mut given: Map<String, Value>,
    root: &Value,
) -> Result<Map<String, Value>, Vec<FieldError>> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut problems = Vec::new();
    let mut resolved = Map::new();
    // Pydantic reports a field's own error before it reports an extra key, so
    // the known fields are checked first and the extras appended after.
    let mut extras = Vec::new();
    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
        let unexpected: Vec<String> = given
            .keys()
            .filter(|name| !properties.contains_key(*name))
            .cloned()
            .collect();
        for name in unexpected {
            if let Some(value) = given.remove(&name) {
                extras.push(finish(
                    &pydantic(
                        &descend(location, Loc::Field(name.clone())),
                        "Extra inputs are not permitted",
                    ),
                    &value,
                ));
            }
        }
    }
    for (name, property) in &properties {
        match given.remove(name) {
            Some(value) => match before_validate(
                operation,
                name,
                value,
                &descend(location, Loc::Field(name.clone())),
            )
            .and_then(|value| {
                check_value(
                    operation,
                    &descend(location, Loc::Field(name.clone())),
                    property,
                    value,
                    root,
                )
            }) {
                Ok(checked) => {
                    resolved.insert(name.clone(), checked);
                }
                Err(found) => problems.extend(found),
            },
            None => {
                if let Some(default) = property.get("default") {
                    // Pydantic records `null` as the default of an optional field
                    // the caller left out. The CLI omits such a flag entirely, so
                    // a null default is left absent rather than inserted.
                    if !default.is_null() {
                        resolved.insert(name.clone(), default.clone());
                    }
                } else if required_of(schema).iter().any(|item| item == name) {
                    problems.push(pydantic(
                        &descend(location, Loc::Field(name.clone())),
                        "Field required",
                    ));
                }
            }
        }
    }
    problems.extend(extras);
    if problems.is_empty() {
        Ok(resolved)
    } else {
        Err(problems)
    }
}

/// Pydantic `mode="before"` validators that run ahead of the type check, for the
/// fields that declare one. Each mirrors the Python validator it stands for:
/// `comma_list` on `session.list` `fields`, and `_cursor_is_seq_or_now` on
/// `session.wait_any` `cursor`.
fn before_validate(
    operation: &str,
    name: &str,
    value: Value,
    location: &[Loc],
) -> Result<Value, Vec<FieldError>> {
    match (operation, name) {
        ("session.list", "fields") => Ok(match value {
            Value::String(text) => Value::Array(
                text.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(|part| Value::String(part.to_string()))
                    .collect(),
            ),
            other => other,
        }),
        ("session.wait_any", "cursor") => {
            let input = value.clone();
            cursor_before(value).map_err(|complaint| {
                let problem = pydantic(location, &format!("Value error, {complaint}"));
                vec![finish(&problem, &input)]
            })
        }
        _ => Ok(value),
    }
}

const CURSOR_COMPLAINT: &str =
    "cursor is a sequence number, or 'now' to skip events already published";

fn cursor_before(value: Value) -> Result<Value, &'static str> {
    match value {
        // A bool is an int to Python, so it reaches the type check unchanged.
        Value::Null | Value::Bool(_) => Ok(value),
        Value::Number(ref number) if number.is_i64() || number.is_u64() => Ok(value),
        // A whole number too big for i64 is kept as JSON sent it, as
        // `check_integer` does for every int field.
        Value::Number(ref number)
            if number
                .as_f64()
                .is_some_and(|f| f.fract() == 0.0 && f.abs() >= 9.2e18) =>
        {
            Ok(value)
        }
        Value::String(text) => {
            let text = text.trim().to_lowercase();
            if text.is_empty() {
                Ok(Value::Null)
            } else if text == "now" {
                Ok(Value::String(text))
            } else if text.chars().all(|c| c.is_ascii_digit()) {
                // Pydantic takes an int of any size. One past i64 cannot be
                // held as a number here, so it passes as its digits, which the
                // int-or-str check accepts.
                Ok(text
                    .parse::<i64>()
                    .map(Value::from)
                    .unwrap_or(Value::String(text)))
            } else {
                Err(CURSOR_COMPLAINT)
            }
        }
        _ => Err(CURSOR_COMPLAINT),
    }
}

/// The location one step deeper. A key is always one field step, even when the
/// key itself contains a dot: `a.b` is a single field, never two.
fn descend(location: &[Loc], step: Loc) -> Vec<Loc> {
    location
        .iter()
        .cloned()
        .chain(std::iter::once(step))
        .collect()
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
    location: &[Loc],
    property: &Value,
    value: Value,
    root: &Value,
) -> Result<Value, Vec<FieldError>> {
    // A union of several value types (`int | str | None`) accepts the value if
    // any branch does, as pydantic's smart mode does. The first branch's error
    // is the one reported when none does.
    let value_branches = union_value_branches(property);
    if value_branches.len() > 1 && !value.is_null() {
        let mut first_error = None;
        for branch in value_branches {
            match check_value(operation, location, branch, value.clone(), root) {
                Ok(checked) => return Ok(checked),
                Err(errors) => {
                    first_error.get_or_insert(errors);
                }
            }
        }
        return Err(first_error.unwrap_or_default());
    }
    let (schema, nullable) = match nullable_branch(property) {
        Some(schema) => (schema, true),
        None => (property, false),
    };
    if value.is_null() {
        // A nullable field accepts null whatever else its other branch says,
        // including an enum: the null was allowed by the anyOf, and the enum
        // constrains only the value branch.
        if nullable {
            return Ok(value);
        }
        // A null where the value must be one of an enum fails the enum, which
        // names the allowed values, rather than the underlying string check.
        if schema.get("enum").is_some() {
            return check_string(operation, location, schema, value.clone())
                .map_err(|p| stamp(&p, &value));
        }
        return if schema.get("format").and_then(Value::as_str) == Some("date-time") {
            Err(stamp(
                &[pydantic(location, "Input should be a valid datetime")],
                &value,
            ))
        } else {
            Err(stamp(
                &[pydantic(location, &expected_input(schema))],
                &value,
            ))
        };
    }
    if let Some(nested) = schema.get("properties") {
        if nested.as_object().is_some_and(|fields| !fields.is_empty()) {
            let Value::Object(object) = value else {
                let model = schema
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("object");
                return Err(stamp(
                    &[pydantic(
                        location,
                        &format!("Input should be a valid dictionary or instance of {model}"),
                    )],
                    &value,
                ));
            };
            return apply_object(operation, location, schema, object, root).map(Value::Object);
        }
    }
    // A field typed as another model arrives as `{"$ref": "#/$defs/Name"}`.
    // The definition lives on the operation's own schema.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let Some(target) = root.pointer(reference.trim_start_matches('#')) else {
            return Err(vec![FieldError::at(
                location,
                &format!(
                    "{}\n  {} refers to an unknown type",
                    dotted(location),
                    dotted(location)
                ),
            )]);
        };
        return check_value(operation, location, target, value, root);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => {
            if schema.get("format").and_then(Value::as_str) == Some("date-time") {
                if let Some(coerced) = coerce_datetime(&value) {
                    return check_string(operation, location, schema, coerced)
                        .map_err(|problems| stamp(&problems, &value));
                }
                // A bool or null is the wrong type. A string that fails to parse
                // is a different complaint, made inside `check_string`.
                if !value.is_string() {
                    return Err(stamp(
                        &[pydantic(location, "Input should be a valid datetime")],
                        &value,
                    ));
                }
            }
            check_string(operation, location, schema, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("integer") => {
            check_integer(operation, location, schema, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("number") => {
            check_number(operation, location, schema, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("boolean") => {
            check_boolean(operation, location, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("array") => check_array(operation, location, schema, value, root),
        Some("object") if operation == "preference.set" && dotted(location) == "value" => {
            check_preference_value(operation, location, value.clone())
                .map_err(|p| stamp(&p, &value))
        }
        _ => Ok(value),
    }
}

/// Add the rejected value's tail to each complaint.
fn stamp(problems: &[FieldError], input: &Value) -> Vec<FieldError> {
    problems
        .iter()
        .map(|problem| finish(problem, input))
        .collect()
}

/// The non-null branches of an `anyOf` union, in the order pydantic declares
/// them. Empty when the property is not a union.
fn union_value_branches(property: &Value) -> Vec<&Value> {
    let Some(branches) = property.get("anyOf").and_then(Value::as_array) else {
        return Vec::new();
    };
    branches
        .iter()
        .filter(|branch| branch.get("type").and_then(Value::as_str) != Some("null"))
        .collect()
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
    location: &[Loc],
    schema: &Value,
    value: Value,
) -> Result<Value, Vec<FieldError>> {
    // `Name` and `Reason` strip whitespace before anything else, so a value of
    // " " fails the length check and " padded " is stored trimmed. Which fields
    // those are is recorded from the models (`strips.rs`), because pydantic's
    // schema drops the constraint and the same field location is a plain `str` on
    // other operations.
    let stripped = match &value {
        Value::String(text) if strips_whitespace(operation, &dotted(location)) => {
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
            return Err(vec![pydantic(
                location,
                &format!("Input should be '{listed}'"),
            )]);
        }
    }
    let Value::String(text) = &stripped else {
        return Err(vec![pydantic(location, "Input should be a valid string")]);
    };
    if let Some(min) = bound(schema, "minLength") {
        if (text.chars().count() as i64) < min {
            let noun = if min == 1 { "character" } else { "characters" };
            return Err(vec![pydantic(
                location,
                &format!("String should have at least {min} {noun}"),
            )]);
        }
    }
    if let Some(max) = bound(schema, "maxLength") {
        if (text.chars().count() as i64) > max {
            let noun = if max == 1 { "character" } else { "characters" };
            return Err(vec![pydantic(
                location,
                &format!("String should have at most {max} {noun}"),
            )]);
        }
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        if let Ok(expression) = regex::Regex::new(pattern) {
            if !expression.is_match(text) {
                return Err(vec![pydantic(
                    location,
                    &format!("String should match pattern '{pattern}'"),
                )]);
            }
        }
    }
    if schema.get("format").and_then(Value::as_str) == Some("date-time") && !is_datetime(text) {
        // A bool or null never got past the string check above. A string that is
        // not a datetime is the parse failure, and the corpus's are all too short.
        return Err(vec![pydantic(
            location,
            "Input should be a valid datetime or date, input is too short",
        )]);
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
fn check_preference_value(
    _operation: &str,
    location: &[Loc],
    value: Value,
) -> Result<Value, Vec<FieldError>> {
    let Value::String(text) = &value else {
        return Ok(value);
    };
    match serde_json::from_str::<Value>(text) {
        Ok(parsed @ Value::Object(_)) => Ok(parsed),
        Ok(_) => Err(vec![pydantic(
            location,
            "Input should be a valid dictionary",
        )]),
        Err(_) => Err(vec![pydantic(
            location,
            "Value error, value is not valid JSON: Expecting value",
        )]),
    }
}

fn strips_whitespace(operation: &str, field: &str) -> bool {
    super::strips::STRIPS.contains(&(operation, field))
}

fn check_integer(
    operation: &str,
    location: &[Loc],
    schema: &Value,
    value: Value,
) -> Result<Value, Vec<FieldError>> {
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
                    return Err(vec![pydantic(
                        location,
                        &format!("Input should be less than or equal to {bound}"),
                    )]);
                }
            }
            None => {
                return Err(vec![pydantic(
                    location,
                    "Input should be a valid integer, got a number with a fractional part",
                )]);
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
        return Err(vec![pydantic(
            location,
            "Input should be a valid integer, unable to parse string as an integer",
        )]);
    };
    check_bounds(operation, location, schema, number)?;
    Ok(Value::from(number))
}

fn check_boolean(
    _operation: &str,
    location: &[Loc],
    value: Value,
) -> Result<Value, Vec<FieldError>> {
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
        None => Err(vec![pydantic(
            location,
            "Input should be a valid boolean, unable to interpret input",
        )]),
    }
}

fn check_number(
    _operation: &str,
    location: &[Loc],
    schema: &Value,
    value: Value,
) -> Result<Value, Vec<FieldError>> {
    let number = match &value {
        Value::Number(number) => number.as_f64(),
        _ => None,
    };
    let Some(number) = number else {
        return Err(vec![pydantic(location, "Input should be a valid number")]);
    };
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if number < min {
            return Err(vec![pydantic(
                location,
                &format!("Input should be greater than or equal to {min}"),
            )]);
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if number > max {
            return Err(vec![pydantic(
                location,
                &format!("Input should be less than or equal to {max}"),
            )]);
        }
    }
    Ok(value)
}

fn check_bounds(
    _operation: &str,
    location: &[Loc],
    schema: &Value,
    number: i64,
) -> Result<(), Vec<FieldError>> {
    if let Some(min) = bound(schema, "minimum") {
        if number < min {
            return Err(vec![pydantic(
                location,
                &format!("Input should be greater than or equal to {min}"),
            )]);
        }
    }
    if let Some(min) = bound(schema, "exclusiveMinimum") {
        if number <= min {
            return Err(vec![pydantic(
                location,
                &format!("Input should be greater than {min}"),
            )]);
        }
    }
    if let Some(max) = bound(schema, "exclusiveMaximum") {
        if number >= max {
            return Err(vec![pydantic(
                location,
                &format!("Input should be less than {max}"),
            )]);
        }
    }
    if let Some(max) = bound(schema, "maximum") {
        if number > max {
            return Err(vec![pydantic(
                location,
                &format!("Input should be less than or equal to {max}"),
            )]);
        }
    }
    Ok(())
}

fn check_array(
    operation: &str,
    location: &[Loc],
    schema: &Value,
    value: Value,
    root: &Value,
) -> Result<Value, Vec<FieldError>> {
    let Value::Array(items) = value else {
        // A comma-joined string is the list the CLI sends for a repeated flag.
        if let Value::String(text) = &value {
            // `work.list`'s states is the one list the CLI sends comma-joined.
            if operation == "work.list" && dotted(location) == "states" {
                let split = text
                    .split(',')
                    .map(|part| Value::String(part.trim().to_string()))
                    .collect();
                return check_array(operation, location, schema, Value::Array(split), root);
            }
        }
        return Err(stamp(
            &[pydantic(location, "Input should be a valid list")],
            &value,
        ));
    };
    if let Some(max) = bound(schema, "maxItems") {
        if (items.len() as i64) > max {
            return Err(stamp(
                &[pydantic(
                    location,
                    &format!("List should have at most {max} items"),
                )],
                &Value::Array(items),
            ));
        }
    }
    // Pydantic checks `min_length` only "after validation": an item's own
    // error is reported first, and the length only once every item passed.
    let Some(item_schema) = schema.get("items") else {
        return min_items(location, schema, items);
    };
    let mut checked = Vec::with_capacity(items.len());
    let mut problems = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        match check_value(
            operation,
            &descend(location, Loc::Index(index)),
            item_schema,
            item,
            root,
        ) {
            Ok(item) => checked.push(item),
            Err(found) => problems.extend(found),
        }
    }
    if problems.is_empty() {
        min_items(location, schema, checked)
    } else {
        Err(problems)
    }
}

fn min_items(
    location: &[Loc],
    schema: &Value,
    items: Vec<Value>,
) -> Result<Value, Vec<FieldError>> {
    if let Some(min) = bound(schema, "minItems") {
        if (items.len() as i64) < min {
            return Err(stamp(
                &[pydantic(
                    location,
                    &format!(
                        "List should have at least {min} item{} after validation, not {}",
                        if min == 1 { "" } else { "s" },
                        items.len()
                    ),
                )],
                &Value::Array(items),
            ));
        }
    }
    Ok(Value::Array(items))
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
        Some("object") => "dictionary",
        _ => "string",
    };
    format!("Input should be a valid {kind}")
}

/// The field and the complaint, in the shape pydantic prints. The tail that names
/// the rejected value is added by `finish` once that value is known.
fn pydantic(location: &[Loc], complaint: &str) -> FieldError {
    FieldError::at(location, &format!("{}\n  {complaint}", dotted(location)))
}

/// Pydantic's error code. The complaint decides it, except that a null is
/// rejected for its type, which is a different code from a value that fails to
/// parse.
fn error_code(complaint: &str, input: &Value) -> &'static str {
    if input.is_null() && !complaint.starts_with("Input should be '") {
        return match complaint {
            s if s.contains("valid integer") => "int_type",
            s if s.contains("valid number") => "float_type",
            s if s.contains("valid boolean") => "bool_type",
            s if s.contains("valid list") => "list_type",
            s if s.contains("valid dictionary") => "dict_type",
            s if s.contains("datetime") => "datetime_type",
            _ => "string_type",
        };
    }
    match complaint {
        "Field required" => "missing",
        "Extra inputs are not permitted" => "extra_forbidden",
        "Input should be a valid string" => "string_type",
        "Input should be a valid integer, unable to parse string as an integer" => "int_parsing",
        "Input should be a valid integer, got a number with a fractional part" => "int_from_float",
        "Input should be a valid boolean, unable to interpret input" => "bool_parsing",
        "Input should be a valid datetime" => "datetime_type",
        "Input should be a valid datetime or date, input is too short" => {
            "datetime_from_date_parsing"
        }
        "Input should be a valid list" => "list_type",
        "Input should be a valid dictionary" => "dict_type",
        _ if complaint.starts_with("Input should be a valid dictionary or instance of") => {
            "model_type"
        }
        _ if complaint.starts_with("String should have at least") => "string_too_short",
        _ if complaint.starts_with("String should have at most") => "string_too_long",
        _ if complaint.starts_with("String should match pattern") => "string_pattern_mismatch",
        _ if complaint.starts_with("Input should be '") => "literal_error",
        _ if complaint.starts_with("Input should be greater than or equal to") => {
            "greater_than_equal"
        }
        _ if complaint.starts_with("Input should be greater than") => "greater_than",
        _ if complaint.starts_with("Input should be less than or equal to") => "less_than_equal",
        _ if complaint.starts_with("Input should be less than") => "less_than",
        _ if complaint.starts_with("List should have at least") => "too_short",
        _ if complaint.starts_with("List should have at most") => "too_long",
        _ if complaint.starts_with("Value error,") => "value_error",
        _ => "unknown",
    }
}

/// How pydantic names the JSON type of the rejected value.
fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        // A JSON `5.0` is a float and a JSON `5` is an int; `is_f64` is what
        // records the difference. A whole number past 2^63 is also stored as f64,
        // but only because it overflowed, and it is still an int.
        Value::Number(number)
            if number.is_f64() && number.as_f64().is_some_and(|f| f.abs() < i64::MAX as f64) =>
        {
            "float"
        }
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// The value as pydantic prints it, in Python's spelling. A string is shortened
/// before it is quoted, so the quotes are not part of its length.
fn render_input(value: &Value) -> String {
    match value {
        Value::String(text) => format!("'{}'", shorten(text, true)),
        other => shorten(&render_full(other), false),
    }
}

fn render_full(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("'{text}'"),
        Value::Array(items) => {
            let shown = items.iter().map(render_full).collect::<Vec<_>>().join(", ");
            format!("[{shown}]")
        }
        Value::Object(fields) => {
            let shown = fields
                .iter()
                .map(|(name, value)| format!("'{name}': {}", render_full(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{shown}}}")
        }
    }
}

/// Attach the rejected value to a complaint, and keep the structured half: the
/// location, the error code, the message and the value itself.
fn finish(problem: &FieldError, input: &Value) -> FieldError {
    let Some((field, complaint)) = problem.text.split_once("\n  ") else {
        return FieldError::bare(&problem.text);
    };
    // The steps were recorded when the complaint was built. Reading them back
    // out of the dotted text would split a key that looks like an index path:
    // an unknown key literally named `x.0` is one field, not a field and an index.
    let loc = if problem.loc.is_empty() {
        parse_loc(field)
    } else {
        problem.loc.clone()
    };
    let code = error_code(complaint, input);
    // A whole-model complaint has no field line.
    let field = if field.is_empty() {
        String::new()
    } else {
        format!("{field}\n")
    };
    let text = format!(
        "{field}  {complaint} [type={code}, input_value={}, input_type={}]\n    \
         For further information visit https://errors.pydantic.dev/2.13/v/{code}",
        render_input(input),
        json_type(input)
    );
    FieldError {
        loc,
        error_type: code.to_string(),
        msg: complaint.to_string(),
        input: input.clone(),
        text,
    }
}

/// The steps back out of the dotted spelling `dotted` writes. An index is
/// digits and a field step ends at the next `.` that introduces an index, so a
/// key that itself contains a dot stays one field.
fn parse_loc(field: &str) -> Vec<Loc> {
    let mut steps = Vec::new();
    let mut rest = field;
    while !rest.is_empty() {
        let split = rest.match_indices('.').map(|(at, _)| at).find(|at| {
            let step = &rest[..*at];
            let digits = step.rsplit('.').next().unwrap_or("");
            !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit())
        });
        let (head, after) = match split {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, ""),
        };
        // `cells.0` splits into the field and then the index.
        let (step, index) = match head.rfind('.') {
            Some(dot)
                if head[dot + 1..].chars().all(|ch| ch.is_ascii_digit())
                    && !head[dot + 1..].is_empty() =>
            {
                (&head[..dot], Some(&head[dot + 1..]))
            }
            _ => (head, None),
        };
        if !step.is_empty() {
            steps.push(match step.parse::<usize>() {
                Ok(index) if index.to_string() == step => Loc::Index(index),
                _ => Loc::Field(step.to_string()),
            });
        }
        if let Some(index) = index {
            steps.push(Loc::Index(index.parse::<usize>().unwrap()));
        }
        rest = after;
    }
    steps
}

/// The dotted spelling one complaint line uses. The structured `loc` is what a
/// caller compares; this text is only the message.
fn dotted(location: &[Loc]) -> String {
    location
        .iter()
        .map(Loc::as_text)
        .collect::<Vec<_>>()
        .join(".")
}

/// A string keeps its first 24 and last 23 characters; anything else keeps 25 and
/// 24, because its opening brace or bracket counts as one. An ellipsis stands
/// between.
fn shorten(text: &str, string: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 50 {
        return text.to_string();
    }
    let (head, tail) = if string { (12, 11) } else { (25, 24) };
    format!(
        "{}...{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

/// The value a complaint is about. A nested field such as `cells.0.column` was
/// rejected for the object at `cells.0`, and pydantic prints that object rather
/// than the whole input; a top-level field was rejected for the whole input.
fn input_for(problem: &str, given: &Value) -> Value {
    let Some(field) = problem.split('\n').next() else {
        return given.clone();
    };
    let mut path = parse_loc(field);
    path.pop();
    let mut cursor = given;
    for part in &path {
        cursor = match part {
            Loc::Index(index) => match cursor.as_array().and_then(|items| items.get(*index)) {
                Some(item) => item,
                None => return given.clone(),
            },
            Loc::Field(name) => match cursor.as_object().and_then(|fields| fields.get(name)) {
                Some(item) => item,
                None => return given.clone(),
            },
        };
    }
    cursor.clone()
}

fn invalid(operation: &str, problems: &[FieldError]) -> VogtError {
    let model = super::params_schema_for(operation)
        .and_then(|schema| schema.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(operation);
    let count = problems.len();
    let noun = if count == 1 { "error" } else { "errors" };
    let text = format!(
        "invalid arguments for {operation}:\n{count} validation {noun} for {model}\n{}",
        problems
            .iter()
            .map(|problem| problem.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );
    record_validation(ValidationReport {
        text: text.clone(),
        errors: problems.to_vec(),
    });
    VogtError::InvalidRequest(text)
}

#[cfg(test)]
/// A location step as JSON: a field stays a string, an index becomes a number.
fn loc_json(step: &crate::errors::Loc) -> serde_json::Value {
    match step {
        crate::errors::Loc::Field(name) => serde_json::Value::String(name.clone()),
        crate::errors::Loc::Index(index) => serde_json::json!(index),
    }
}

mod tests {
    use super::*;

    /// The text after the `invalid arguments` header and model line.
    fn refusal(operation: &str, params: Value) -> String {
        let error = prepare(operation, params).unwrap_err();
        let message = error.message().to_string();
        message.splitn(3, '\n').nth(2).unwrap().to_string()
    }

    fn pydantic_text(field: Option<&str>, complaint: &str, input: &str, kind: &str) -> String {
        let code = if complaint.starts_with("Value error,") {
            "value_error"
        } else {
            "greater_than"
        };
        let field = field.map(|f| format!("{f}\n")).unwrap_or_default();
        format!(
            "{field}  {complaint} [type={code}, input_value={input}, input_type={kind}]\n    \
             For further information visit https://errors.pydantic.dev/2.13/v/{code}"
        )
    }

    #[test]
    fn a_handoff_comment_without_fields_is_refused() {
        let text = refusal(
            "work.comment",
            serde_json::json!({"ref": "x", "body": "x", "reason": "x", "kind": "handoff"}),
        );
        assert_eq!(
            text,
            pydantic_text(
                None,
                "Value error, a hand-off comment requires handoff fields",
                "{'ref': 'x', 'body': 'x',... 'x', 'kind': 'handoff'}",
                "dict"
            )
        );
    }

    #[test]
    fn a_note_with_handoff_fields_is_refused() {
        let text = refusal(
            "work.comment",
            serde_json::json!({"ref": "x", "body": "x", "reason": "x", "handoff": {}}),
        );
        assert_eq!(
            text,
            pydantic_text(
                None,
                "Value error, handoff fields belong on a comment of kind handoff",
                "{'ref': 'x', 'body': 'x',...on': 'x', 'handoff': {}}",
                "dict"
            )
        );
        let accepted = prepare(
            "work.comment",
            serde_json::json!({"ref": "x", "body": "x", "reason": "x", "kind": "handoff", "handoff": {}}),
        );
        assert!(accepted.is_ok(), "{accepted:?}");
    }

    #[test]
    fn a_digest_context_window_must_be_positive() {
        let text = refusal(
            "session.digest",
            serde_json::json!({"id": "x", "context_window": 0}),
        );
        assert_eq!(
            text,
            pydantic_text(
                Some("context_window"),
                "Input should be greater than 0",
                "0",
                "int"
            )
        );
        assert!(prepare(
            "session.digest",
            serde_json::json!({"id": "x", "context_window": 1})
        )
        .is_ok());
    }

    #[test]
    fn decision_options_report_the_item_before_the_length() {
        let text = refusal(
            "decision.ask",
            serde_json::json!({"question": "x", "options": [5], "reason": "x"}),
        );
        assert!(
            text.starts_with(
                "options.0\n  Input should be a valid dictionary or instance of DecisionOptionModel [type=model_type"
            ),
            "{text}"
        );
    }

    #[test]
    fn get_many_refs_are_1_to_200_characters() {
        let text = refusal("work.get_many", serde_json::json!({"refs": ["WI-1", ""]}));
        assert_eq!(
            text,
            pydantic_text(
                Some("refs"),
                "Value error, each ref is 1-200 characters",
                "['WI-1', '']",
                "list"
            )
        );
    }

    #[test]
    fn started_by_session_is_an_id_and_is_trimmed() {
        let text = refusal(
            "session.list",
            serde_json::json!({"started_by_session": "  "}),
        );
        assert_eq!(
            text,
            pydantic_text(
                Some("started_by_session"),
                "Value error, started_by_session is 1-200 characters",
                "'  '",
                "str"
            )
        );
        let ok = prepare(
            "session.list",
            serde_json::json!({"started_by_session": " ses_1 "}),
        )
        .unwrap();
        assert_eq!(ok["started_by_session"], "ses_1");
    }

    #[test]
    fn a_wait_any_cursor_is_not_negative() {
        let text = refusal("session.wait_any", serde_json::json!({"cursor": -1}));
        assert_eq!(
            text,
            pydantic_text(
                Some("cursor"),
                "Value error, cursor is at least 0",
                "-1",
                "int"
            )
        );
    }

    #[test]
    fn an_alias_and_its_field_together_are_refused() {
        let text = refusal("work.get", serde_json::json!({"id": "WI-1", "ref": "WI-2"}));
        assert_eq!(
            text,
            pydantic_text(
                None,
                "Value error, work.get: 'id' is an alias of 'ref'; pass one of them, not both",
                "{'id': 'WI-1', 'ref': 'WI-2'}",
                "dict"
            )
        );
        let ok = prepare("work.get", serde_json::json!({"id": "WI-1"})).unwrap();
        assert_eq!(ok["ref"], "WI-1");
        let listed = prepare("work.list", serde_json::json!({"status": " open, ,done"})).unwrap();
        assert_eq!(listed["states"], serde_json::json!(["open", "done"]));
        let moved = prepare(
            "work.transition",
            serde_json::json!({"id": "WI-1", "to": "done", "reason": "x"}),
        )
        .unwrap();
        assert_eq!(moved["to_state"], "done");
        let brief = prepare("project.brief", serde_json::json!({"project": "p"})).unwrap();
        assert_eq!(brief["slug"], "p");
    }

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
    fn a_nullable_enum_accepts_null() {
        // `decision` is `Literal["allow", "deny"] | None`. The null branch is
        // what allows it; the enum constrains only the other branch, so a null
        // passes and is kept rather than refused as an invalid choice.
        let resolved = prepare("auth.decisions", serde_json::json!({"decision": null})).unwrap();
        assert_eq!(resolved["decision"], serde_json::Value::Null);
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
    fn a_wait_cursor_is_a_seq_or_now() {
        let now = prepare("session.wait_any", serde_json::json!({"cursor": "now"})).unwrap();
        assert_eq!(now["cursor"], "now");
        let seq = prepare("session.wait_any", serde_json::json!({"cursor": "7"})).unwrap();
        assert_eq!(seq["cursor"], 7);
        assert!(prepare("session.wait_any", serde_json::json!({"cursor": "later"})).is_err());
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

    /// A resolved value matches pydantic's dump when every field Rust kept has
    /// the same value there. A field whose default is null is left absent here
    /// and present as null in the dump, and that difference is not a mismatch:
    /// the services read either as "not given".
    fn resolved_matches(resolved: &serde_json::Value, dump: &serde_json::Value) -> bool {
        match (resolved, dump) {
            (serde_json::Value::Object(resolved), serde_json::Value::Object(dump)) => resolved
                .iter()
                .all(|(name, value)| dump.get(name).is_some_and(|d| resolved_matches(value, d))),
            (serde_json::Value::Array(resolved), serde_json::Value::Array(dump)) => {
                resolved.len() == dump.len()
                    && resolved
                        .iter()
                        .zip(dump)
                        .all(|(r, d)| resolved_matches(r, d))
            }
            _ => resolved == dump,
        }
    }

    /// Put back the digits of a number past 2^63. The parsed value renders it as
    /// `1e+30`; the sent JSON still has every digit, which is what pydantic prints.
    fn restore_digits(message: &str, sent: &str) -> String {
        let mut restored = message.to_string();
        for token in sent.split(|ch: char| !ch.is_ascii_digit()) {
            if token.len() > 18 {
                restored = restored.replacen("1e+30", token, 1);
            }
        }
        restored
    }

    /// Every probe in `tests/parity/validator_corpus.json`, recorded from
    /// pydantic by `scripts/gen_validator_corpus.py`. An accepted probe must
    /// resolve to the same parameters; a refused one must say the same thing.
    /// Pydantic's `[type=...]` tag and its documentation link are not part of
    /// the comparison yet.
    #[test]
    fn matches_the_pydantic_corpus() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/parity/validator_corpus.json");
        let corpus: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut failures = Vec::new();
        for case in &corpus {
            let op = case["op"].as_str().unwrap();
            let tag = case["tag"].as_str().unwrap();
            let expected = &case["py"];
            match prepare(op, case["params"].clone()) {
                Ok(value) => {
                    if !expected["ok"].as_bool().unwrap() {
                        failures.push(format!("{op} {tag}: rust accepted, pydantic refused"));
                    } else if !resolved_matches(&value, &expected["dump"]) {
                        failures.push(format!(
                            "{op} {tag}: resolved {value} != {}",
                            expected["dump"]
                        ));
                    }
                }
                Err(error) => {
                    if expected["ok"].as_bool().unwrap() {
                        failures.push(format!("{op} {tag}: rust refused, pydantic accepted"));
                        continue;
                    }
                    // A number past 2^63 is rounded to `1e+30` once parsed. The
                    // corpus keeps the digits the caller sent; put them back.
                    let mut message = error.message().to_string();
                    if let Some(sent) = expected["sent"].as_str() {
                        message = restore_digits(&message, sent);
                    }
                    if message != expected["text"].as_str().unwrap() {
                        failures.push(format!(
                            "{op} {tag}:\n  rust: {message}\n  pydantic: {}",
                            expected["text"].as_str().unwrap()
                        ));
                    }
                    // The structured half, which the 422 body is built from.
                    // `loc` mixes field names and list indexes, so it is
                    // compared as JSON rather than as text.
                    if let Some(report) = crate::errors::take_validation(&error) {
                        let got = report
                            .errors
                            .iter()
                            .map(|field| {
                                serde_json::json!({
                                    "loc": field.loc.iter().map(loc_json).collect::<Vec<_>>(),
                                    "type": field.error_type,
                                })
                            })
                            .collect::<Vec<_>>();
                        if got != expected["errors"].as_array().unwrap().as_slice() {
                            failures.push(format!(
                                "{op} {tag}: loc/type {got:?} != {}",
                                expected["errors"]
                            ));
                        }
                    } else {
                        failures.push(format!("{op} {tag}: no structured report"));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} probes differ:\n{}",
            failures.len(),
            corpus.len(),
            failures.join("\n")
        );
    }
}
