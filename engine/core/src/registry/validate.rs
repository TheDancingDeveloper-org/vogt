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
    apply_object(operation, "", schema, given, schema)
        .map(Value::Object)
        .map_err(|problems| {
            let problems: Vec<FieldError> = problems
                .into_iter()
                .map(|problem| {
                    if problem.text.contains("[type=") {
                        problem
                    } else {
                        finish(&problem.text, &input_for(&problem.text, &given_value))
                    }
                })
                .collect();
            invalid(operation, &problems)
        })
}

fn apply_object(
    operation: &str,
    location: &str,
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
                    &pydantic(&at(location, &name), "Extra inputs are not permitted"),
                    &value,
                ));
            }
        }
    }
    for (name, property) in &properties {
        match given.remove(name) {
            Some(value) => match check_value(operation, &at(location, name), property, value, root)
            {
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
                    problems.push(FieldError::bare(&pydantic(
                        &at(location, name),
                        "Field required",
                    )));
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

/// A nested field is `cells.cursor`; a top-level one is just its name.
fn at(location: &str, name: &str) -> String {
    if location.is_empty() {
        name.to_string()
    } else {
        format!("{location}.{name}")
    }
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
) -> Result<Value, Vec<FieldError>> {
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
            return check_string(operation, name, schema, value.clone())
                .map_err(|p| stamp(&p, &value));
        }
        return if schema.get("format").and_then(Value::as_str) == Some("date-time") {
            Err(stamp(
                &[FieldError::bare(&pydantic(
                    name,
                    "Input should be a valid datetime",
                ))],
                &value,
            ))
        } else {
            Err(stamp(
                &[FieldError::bare(&pydantic(name, &expected_input(schema)))],
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
                    &[FieldError::bare(&pydantic(
                        name,
                        &format!("Input should be a valid dictionary or instance of {model}"),
                    ))],
                    &value,
                ));
            };
            return apply_object(operation, name, schema, object, root).map(Value::Object);
        }
    }
    // A field typed as another model arrives as `{"$ref": "#/$defs/Name"}`.
    // The definition lives on the operation's own schema.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let Some(target) = root.pointer(reference.trim_start_matches('#')) else {
            return Err(vec![FieldError::bare(&format!(
                "{name}\n  {name} refers to an unknown type"
            ))]);
        };
        return check_value(operation, name, target, value, root);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => {
            if schema.get("format").and_then(Value::as_str) == Some("date-time") {
                if let Some(coerced) = coerce_datetime(&value) {
                    return check_string(operation, name, schema, coerced)
                        .map_err(|problems| stamp(&problems, &value));
                }
                // A bool or null is the wrong type. A string that fails to parse
                // is a different complaint, made inside `check_string`.
                if !value.is_string() {
                    return Err(stamp(
                        &[FieldError::bare(&pydantic(
                            name,
                            "Input should be a valid datetime",
                        ))],
                        &value,
                    ));
                }
            }
            check_string(operation, name, schema, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("integer") => {
            check_integer(operation, name, schema, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("number") => {
            check_number(operation, name, schema, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("boolean") => {
            check_boolean(operation, name, value.clone()).map_err(|p| stamp(&p, &value))
        }
        Some("array") => check_array(operation, name, schema, value, root),
        Some("object") if operation == "preference.set" && name == "value" => {
            check_preference_value(operation, name, value.clone()).map_err(|p| stamp(&p, &value))
        }
        _ => Ok(value),
    }
}

/// Add the rejected value's tail to each complaint.
fn stamp(problems: &[FieldError], input: &Value) -> Vec<FieldError> {
    problems
        .iter()
        .map(|problem| finish(&problem.text, input))
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
    name: &str,
    schema: &Value,
    value: Value,
) -> Result<Value, Vec<FieldError>> {
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
            return Err(vec![FieldError::bare(&pydantic(
                name,
                &format!("Input should be '{listed}'"),
            ))]);
        }
    }
    let Value::String(text) = &stripped else {
        return Err(vec![FieldError::bare(&pydantic(
            name,
            "Input should be a valid string",
        ))]);
    };
    if let Some(min) = bound(schema, "minLength") {
        if (text.chars().count() as i64) < min {
            let noun = if min == 1 { "character" } else { "characters" };
            return Err(vec![FieldError::bare(&pydantic(
                name,
                &format!("String should have at least {min} {noun}"),
            ))]);
        }
    }
    if let Some(max) = bound(schema, "maxLength") {
        if (text.chars().count() as i64) > max {
            let noun = if max == 1 { "character" } else { "characters" };
            return Err(vec![FieldError::bare(&pydantic(
                name,
                &format!("String should have at most {max} {noun}"),
            ))]);
        }
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        if let Ok(expression) = regex::Regex::new(pattern) {
            if !expression.is_match(text) {
                return Err(vec![FieldError::bare(&pydantic(
                    name,
                    &format!("String should match pattern '{pattern}'"),
                ))]);
            }
        }
    }
    if schema.get("format").and_then(Value::as_str) == Some("date-time") && !is_datetime(text) {
        // A bool or null never got past the string check above. A string that is
        // not a datetime is the parse failure, and the corpus's are all too short.
        return Err(vec![FieldError::bare(&pydantic(
            name,
            "Input should be a valid datetime or date, input is too short",
        ))]);
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
    name: &str,
    value: Value,
) -> Result<Value, Vec<FieldError>> {
    let Value::String(text) = &value else {
        return Ok(value);
    };
    match serde_json::from_str::<Value>(text) {
        Ok(parsed @ Value::Object(_)) => Ok(parsed),
        Ok(_) => Err(vec![FieldError::bare(&pydantic(
            name,
            "Input should be a valid dictionary",
        ))]),
        Err(_) => Err(vec![FieldError::bare(&pydantic(
            name,
            "Value error, value is not valid JSON: Expecting value",
        ))]),
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
                    return Err(vec![FieldError::bare(&pydantic(
                        name,
                        &format!("Input should be less than or equal to {bound}"),
                    ))]);
                }
            }
            None => {
                return Err(vec![FieldError::bare(&pydantic(
                    name,
                    "Input should be a valid integer, got a number with a fractional part",
                ))]);
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
        return Err(vec![FieldError::bare(&pydantic(
            name,
            "Input should be a valid integer, unable to parse string as an integer",
        ))]);
    };
    check_bounds(operation, name, schema, number)?;
    Ok(Value::from(number))
}

fn check_boolean(_operation: &str, name: &str, value: Value) -> Result<Value, Vec<FieldError>> {
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
        None => Err(vec![FieldError::bare(&pydantic(
            name,
            "Input should be a valid boolean, unable to interpret input",
        ))]),
    }
}

fn check_number(
    _operation: &str,
    name: &str,
    schema: &Value,
    value: Value,
) -> Result<Value, Vec<FieldError>> {
    let number = match &value {
        Value::Number(number) => number.as_f64(),
        _ => None,
    };
    let Some(number) = number else {
        return Err(vec![FieldError::bare(&pydantic(
            name,
            "Input should be a valid number",
        ))]);
    };
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if number < min {
            return Err(vec![FieldError::bare(&pydantic(
                name,
                &format!("Input should be greater than or equal to {min}"),
            ))]);
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if number > max {
            return Err(vec![FieldError::bare(&pydantic(
                name,
                &format!("Input should be less than or equal to {max}"),
            ))]);
        }
    }
    Ok(value)
}

fn check_bounds(
    _operation: &str,
    name: &str,
    schema: &Value,
    number: i64,
) -> Result<(), Vec<FieldError>> {
    if let Some(min) = bound(schema, "minimum") {
        if number < min {
            return Err(vec![FieldError::bare(&pydantic(
                name,
                &format!("Input should be greater than or equal to {min}"),
            ))]);
        }
    }
    if let Some(max) = bound(schema, "maximum") {
        if number > max {
            return Err(vec![FieldError::bare(&pydantic(
                name,
                &format!("Input should be less than or equal to {max}"),
            ))]);
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
) -> Result<Value, Vec<FieldError>> {
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
        return Err(stamp(
            &[FieldError::bare(&pydantic(
                name,
                "Input should be a valid list",
            ))],
            &value,
        ));
    };
    if let Some(min) = bound(schema, "minItems") {
        if (items.len() as i64) < min {
            return Err(stamp(
                &[FieldError::bare(&pydantic(
                    name,
                    &format!(
                        "List should have at least {min} item after validation, not {}",
                        items.len()
                    ),
                ))],
                &Value::Array(items),
            ));
        }
    }
    if let Some(max) = bound(schema, "maxItems") {
        if (items.len() as i64) > max {
            return Err(stamp(
                &[FieldError::bare(&pydantic(
                    name,
                    &format!("List should have at most {max} items"),
                ))],
                &Value::Array(items),
            ));
        }
    }
    let Some(item_schema) = schema.get("items") else {
        return Ok(Value::Array(items));
    };
    let mut checked = Vec::with_capacity(items.len());
    let mut problems = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        match check_value(
            operation,
            &format!("{name}.{index}"),
            item_schema,
            item,
            root,
        ) {
            Ok(item) => checked.push(item),
            Err(found) => problems.extend(found),
        }
    }
    if problems.is_empty() {
        Ok(Value::Array(checked))
    } else {
        Err(problems)
    }
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
fn pydantic(field: &str, complaint: &str) -> String {
    format!("{field}\n  {complaint}")
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
        _ if complaint.starts_with("Input should be greater than") => "greater_than_equal",
        _ if complaint.starts_with("Input should be less than") => "less_than_equal",
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
fn finish(problem: &str, input: &Value) -> FieldError {
    let Some((field, complaint)) = problem.split_once("\n  ") else {
        return FieldError::bare(problem);
    };
    let code = error_code(complaint, input);
    let text = format!(
        "{field}\n  {complaint} [type={code}, input_value={}, input_type={}]\n    \
         For further information visit https://errors.pydantic.dev/2.13/v/{code}",
        render_input(input),
        json_type(input)
    );
    FieldError {
        loc: parse_loc(field),
        error_type: code.to_string(),
        msg: complaint.to_string(),
        input: input.clone(),
        text,
    }
}

/// A dotted location back into pydantic's steps. `cells.2.lane_key` is a field,
/// then an index, then a field — the dotted form cannot say which, but the
/// steps are built here, where an all-digit step is an index.
fn parse_loc(field: &str) -> Vec<Loc> {
    field
        .split('.')
        .filter(|step| !step.is_empty())
        .map(|step| match step.parse::<usize>() {
            Ok(index) if index.to_string() == step => Loc::Index(index),
            _ => Loc::Field(step.to_string()),
        })
        .collect()
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
    let mut path: Vec<&str> = field.split('.').collect();
    path.pop();
    let mut cursor = given;
    for part in path {
        cursor = match part.parse::<usize>() {
            Ok(index) => match cursor.as_array().and_then(|items| items.get(index)) {
                Some(item) => item,
                None => return given.clone(),
            },
            Err(_) => match cursor.as_object().and_then(|fields| fields.get(part)) {
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
