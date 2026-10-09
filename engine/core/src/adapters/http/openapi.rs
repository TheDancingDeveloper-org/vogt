//! The OpenAPI document, generated from the registry. Ports the document
//! FastAPI builds for `src/vogt/adapters/http/app.py`.
//!
//! The document describes the routes rather than the bodies. A parameter and
//! result schema lands with the service that owns it, because inventing one
//! ahead of the ported handler would document a shape the server does not yet
//! return.

use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;

use crate::registry::{default_registry, HttpMethod, Operation, Transport};

/// Served at the root, beside the health probes, exactly where FastAPI puts it.
pub const OPENAPI_PATH: &str = "/openapi.json";

pub fn router() -> Router {
    Router::new().route(OPENAPI_PATH, get(document))
}

async fn document() -> Response {
    Json(openapi()).into_response()
}

fn openapi() -> serde_json::Value {
    let registry = default_registry();
    let mut paths = serde_json::Map::new();
    for operation in registry.for_transport(Transport::Http) {
        let path = format!("/api{}", operation.route.path);
        let entry = paths.entry(path).or_insert_with(|| serde_json::json!({}));
        entry[method_name(operation.route.method)] = operation_object(operation);
    }
    serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Vogt",
            "version": crate::VERSION,
            "description": "Every capability is available here, on the CLI, and over MCP — all three generated from one operation registry.",
        },
        "paths": paths,
    })
}

fn operation_object(operation: &Operation) -> serde_json::Value {
    serde_json::json!({
        "operationId": operation.name.replace('.', "_"),
        "summary": operation.summary,
        "responses": {"200": {"description": "The operation's result."}},
    })
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
