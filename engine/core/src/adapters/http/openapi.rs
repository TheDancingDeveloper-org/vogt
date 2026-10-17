//! The OpenAPI document. Ports the document FastAPI builds in
//! `src/vogt/adapters/http/app.py` and serves beside the probes.
//!
//! Registry routes are derived from each operation's pydantic-compatible schemas.
//! A GET takes its parameters from the query model, as FastAPI expands
//! `Annotated[Model, Query()]`; any other verb takes the model as its JSON body.
//! The result is the response schema. Each `$defs` entry is hoisted into
//! `components.schemas`, the way FastAPI does it. The routes the registry does not
//! own (login, install, the health probes, `/mcp`) come from `openapi_hand.json`,
//! extracted from the Python document, because their models are not in the registry.
//!
//! The golden `tests/parity/golden/openapi.json` is the document the Python server
//! serves. Regenerate it with `VOGT_DATA_DIR=<scratch> uv run vogt init`, then
//! `uv run vogt serve --host 127.0.0.1 --port <p> --no-auth --no-schedule` and
//! `curl http://127.0.0.1:<p>/openapi.json`.

use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Map, Value};

use crate::registry::{
    default_registry, params_schema_for, result_schema_for, HttpMethod, Operation, Transport,
};

/// Served at the root, beside the health probes, exactly where FastAPI puts it.
pub const OPENAPI_PATH: &str = "/openapi.json";

/// The routes the registry does not own, with their components, as Python serves them.
const HAND_ROUTES: &str = include_str!("openapi_hand.json");

pub fn router() -> Router {
    Router::new().route(OPENAPI_PATH, get(document))
}

async fn document() -> Response {
    Json(openapi()).into_response()
}

pub fn openapi() -> Value {
    let registry = default_registry();
    let mut paths = Map::new();
    let mut components = Map::new();
    for operation in registry.for_transport(Transport::Http) {
        let path = format!("/api{}", operation.http_route().path);
        let entry = paths.entry(path).or_insert_with(|| json!({}));
        entry[method_name(operation.http_route().method)] = operation_object(operation, &mut components);
    }
    let hand: Value = serde_json::from_str(HAND_ROUTES).expect("openapi_hand.json is JSON");
    for (path, item) in hand["paths"].as_object().into_iter().flatten() {
        assert!(
            paths.insert(path.clone(), item.clone()).is_none(),
            "a hand-owned route {path} collides with a registry route"
        );
    }
    for (name, schema) in hand["components"].as_object().into_iter().flatten() {
        components.entry(name).or_insert_with(|| schema.clone());
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Vogt",
            "version": crate::VERSION,
            "description": "Every capability is available here, on the CLI, and over MCP — all three generated from one operation registry.",
        },
        "paths": paths,
        "components": {"schemas": components},
    })
}

fn operation_object(operation: &Operation, components: &mut Map<String, Value>) -> Value {
    let params = hoist(
        params_schema_for(operation.name)
            .expect("every registry operation has a params schema")
            .clone(),
        components,
    );
    let result = hoist(
        result_schema_for(operation.name)
            .expect("every registry operation has a result schema")
            .clone(),
        components,
    );
    let mut object = json!({
        "operationId": operation.name.replace('.', "_"),
        "summary": operation.summary,
        // FastAPI's description is the endpoint docstring, which is the summary.
        "description": operation.summary,
    });
    let takes_arguments;
    if operation.http_route().method == HttpMethod::Get {
        let parameters = query_parameters(&params);
        takes_arguments = !parameters.is_empty();
        if takes_arguments {
            object["parameters"] = Value::Array(parameters);
        }
    } else {
        takes_arguments = true;
        let title = title_of(&params);
        components.entry(title.clone()).or_insert(params);
        object["requestBody"] = json!({
            "required": true,
            "content": {"application/json": {"schema": component_ref(&title)}},
        });
    }
    let mut responses = json!({
        "200": {
            "description": "Successful Response",
            "content": {"application/json": {"schema": response_schema(result, components)}},
        },
    });
    // FastAPI validates only what the endpoint takes, so a GET with no query
    // parameters has no validation failure to describe.
    if takes_arguments {
        responses["422"] = json!({
            "description": "Validation Error",
            "content": {"application/json": {"schema": component_ref("HTTPValidationError")}},
        });
    }
    object["responses"] = responses;
    object
}

/// One query parameter per property of the query model, required when the model
/// says so. The property schema is the parameter's schema, as FastAPI copies it.
fn query_parameters(params: &Value) -> Vec<Value> {
    let required: Vec<&str> = params["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    params["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, schema)| {
            let mut parameter = json!({
                "name": name,
                "in": "query",
                "required": required.contains(&name.as_str()),
                "schema": schema,
            });
            if let Some(description) = schema.get("description") {
                parameter["description"] = description.clone();
            }
            parameter
        })
        .collect()
}

/// A result model is a named component. A result without a title is inlined.
fn response_schema(result: Value, components: &mut Map<String, Value>) -> Value {
    match result
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        Some(title) => {
            components.entry(title.clone()).or_insert(result);
            component_ref(&title)
        }
        None => result,
    }
}

/// Move a schema's `$defs` into `components` and point every reference at them.
fn hoist(mut schema: Value, components: &mut Map<String, Value>) -> Value {
    if let Some(defs) = schema
        .as_object_mut()
        .and_then(|object| object.remove("$defs"))
    {
        for (name, definition) in defs.as_object().into_iter().flatten() {
            components
                .entry(name.clone())
                .or_insert_with(|| rewrite_refs(definition.clone()));
        }
    }
    rewrite_refs(schema)
}

/// Point references at `components`, and carry pydantic's defaults the way the
/// served document does: a `null` default is not emitted, and a `null` field in a
/// default object is not either.
fn rewrite_refs(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter(|(key, value)| !(key == "default" && value.is_null()))
                .map(|(key, value)| {
                    let value = match (key.as_str(), value) {
                        ("$ref", Value::String(target)) => {
                            Value::String(target.replace("#/$defs/", "#/components/schemas/"))
                        }
                        ("default", value) => without_null_fields(value),
                        (_, value) => rewrite_refs(value),
                    };
                    (key, value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(rewrite_refs).collect()),
        other => other,
    }
}

fn without_null_fields(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, without_null_fields(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(without_null_fields).collect()),
        other => other,
    }
}

fn title_of(schema: &Value) -> String {
    schema["title"]
        .as_str()
        .expect("a registry model schema carries a title")
        .to_string()
}

fn component_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
    }
}

/// JSON numbers compare by value, as Python does. The served document writes some
/// bounds as floats (`99.0`) where the registry has integers (`99`), and the two
/// are the same number to a client.
fn integers_where_whole(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, integers_where_whole(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(integers_where_whole).collect()),
        Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() && float.fract() == 0.0 => json!(float as i64),
            _ => Value::Number(number),
        },
        other => other,
    }
}

/// Parameters are compared by name. FastAPI lists them in the order the model
/// declares its fields, and the registry golden does not carry that order (its
/// keys are sorted), so the order is not the contract here.
fn normalised(document: Value) -> Value {
    let mut document = integers_where_whole(document);
    for item in document["paths"]
        .as_object_mut()
        .into_iter()
        .flat_map(|paths| paths.values_mut())
    {
        for operation in item
            .as_object_mut()
            .into_iter()
            .flat_map(|ops| ops.values_mut())
        {
            if let Some(parameters) = operation["parameters"].as_array_mut() {
                parameters.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            }
        }
    }
    document
}

/// Where two documents differ, one line each, so a failure names the route or
/// component rather than dumping both documents.
fn differences(ours: &Value, expected: &Value) -> Vec<String> {
    let mut found = Vec::new();
    for key in ["openapi", "info"] {
        if ours[key] != expected[key] {
            found.push(format!("{key} differs"));
        }
    }
    let empty = Map::new();
    let (our_paths, their_paths) = (
        ours["paths"].as_object().unwrap_or(&empty),
        expected["paths"].as_object().unwrap_or(&empty),
    );
    for path in our_paths
        .keys()
        .filter(|path| !their_paths.contains_key(*path))
    {
        found.push(format!("path {path} is not in the Python document"));
    }
    for path in their_paths
        .keys()
        .filter(|path| !our_paths.contains_key(*path))
    {
        found.push(format!("path {path} is missing"));
    }
    for (path, their_item) in their_paths {
        let Some(our_item) = our_paths.get(path) else {
            continue;
        };
        let methods = their_item
            .as_object()
            .into_iter()
            .flat_map(|ops| ops.keys())
            .chain(our_item.as_object().into_iter().flat_map(|ops| ops.keys()));
        for method in methods.collect::<std::collections::BTreeSet<_>>() {
            if our_item.get(method) != their_item.get(method) {
                found.push(format!("{method} {path} differs"));
            }
        }
    }
    let empty = Map::new();
    let (our_components, their_components) = (
        ours["components"]["schemas"].as_object().unwrap_or(&empty),
        expected["components"]["schemas"]
            .as_object()
            .unwrap_or(&empty),
    );
    for name in our_components
        .keys()
        .chain(their_components.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if our_components.get(name) != their_components.get(name) {
            found.push(format!(
                "component {name} differs{}",
                first_difference(our_components.get(name), their_components.get(name))
            ));
        }
    }
    found
}

/// The first key, and under `properties` the first property, where two schemas
/// disagree, so a failure names what to fix.
fn first_difference(ours: Option<&Value>, expected: Option<&Value>) -> String {
    let (Some(ours), Some(expected)) = (ours, expected) else {
        return " (absent on one side)".to_string();
    };
    let keys: std::collections::BTreeSet<&String> = ours
        .as_object()
        .into_iter()
        .flat_map(|object| object.keys())
        .chain(
            expected
                .as_object()
                .into_iter()
                .flat_map(|object| object.keys()),
        )
        .collect();
    for key in keys {
        if ours.get(key) != expected.get(key) {
            if key == "properties" {
                let inner = first_difference(
                    ours.get(key)
                        .and_then(|p| p.as_object())
                        .and_then(|p| p.values().next()),
                    None,
                );
                let _ = inner;
                let props = ours[key].as_object().into_iter().flatten().map(|(n, _)| n);
                for name in props.chain(
                    expected[key]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(n, _)| n),
                ) {
                    if ours[key].get(name) != expected[key].get(name) {
                        return format!(
                            " at properties.{name}: {} vs golden {}",
                            ours[key]
                                .get(name)
                                .map_or("absent".into(), |v| v.to_string()),
                            expected[key]
                                .get(name)
                                .map_or("absent".into(), |v| v.to_string()),
                        );
                    }
                }
            }
            return format!(
                " at {key}: {} vs golden {}",
                ours.get(key).map_or("absent".into(), |v| v.to_string()),
                expected.get(key).map_or("absent".into(), |v| v.to_string()),
            );
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../../../../../tests/parity/golden/openapi.json");

    #[test]
    fn the_document_names_vogt_and_lists_a_registry_route() {
        let document = openapi();
        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["info"]["title"], "Vogt");
        assert!(document["paths"].get("/api/projects").is_some());
        assert_eq!(
            document["paths"]["/api/projects"]["get"]["operationId"],
            "project_list"
        );
    }

    #[test]
    fn a_local_only_operation_is_not_in_the_document() {
        let document = openapi();
        assert!(document["paths"].get("/api/instance/init").is_none());
    }

    #[test]
    fn a_get_takes_its_query_model_as_parameters_and_a_write_as_a_body() {
        let document = openapi();
        let list = &document["paths"]["/api/projects"]["get"];
        assert!(list.get("requestBody").is_none());
        let limit = list["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "limit")
            .expect("limit is a query parameter");
        assert_eq!(limit["in"], "query");
        assert_eq!(limit["required"], false);
        assert_eq!(limit["schema"]["default"], 50);

        let register = &document["paths"]["/api/projects"]["post"];
        assert_eq!(
            register["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/RegisterProjectParams"
        );
        assert_eq!(
            register["responses"]["422"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/HTTPValidationError"
        );
    }

    #[test]
    fn every_reference_resolves_to_a_component() {
        let document = openapi();
        let schemas = &document["components"]["schemas"];
        let text = document.to_string();
        for segment in text.split("\"#/components/schemas/").skip(1) {
            let name: String = segment.chars().take_while(|c| *c != '"').collect();
            assert!(schemas.get(&name).is_some(), "dangling reference to {name}");
        }
        assert!(
            !text.contains("#/$defs/"),
            "an unhoisted $defs reference remains"
        );
    }

    /// The document the Python server serves, byte for byte in meaning. Any drift
    /// names the route or component that differs.
    #[test]
    fn the_document_matches_the_python_golden() {
        let golden: Value = serde_json::from_str(GOLDEN).expect("the golden is JSON");
        let ours = normalised(openapi());
        let expected = normalised(golden);
        let found = differences(&ours, &expected);
        assert!(
            found.is_empty(),
            "{} differences:\n{}",
            found.len(),
            found.join("\n")
        );
    }
}
