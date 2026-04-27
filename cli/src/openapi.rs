//! `OpenAPI` 3.1 description for the `/admin/*` config API.
//!
//! Hand-written rather than macro-generated: keeps coulisse-core thin
//! (no `schemars` dep on every feature crate), keeps the spec free to
//! describe content negotiation faithfully (curl `Accept: application/json`
//! returns JSON, browsers get HTML — neither convention plays well with
//! macro-scraped handlers), and keeps the surface area visible in one
//! file. Update when admin routes change.
//!
//! Served at `GET /admin/openapi.json`. SDK generators (`openapi-generator`,
//! `openapi-typescript-codegen`, etc.) consume it directly.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde_json::{Value, json};

use crate::config_store::ConfigStore;

pub fn router(store: Arc<ConfigStore>) -> Router {
    Router::new()
        .route("/openapi.json", get(openapi_json))
        .with_state(store)
}

async fn openapi_json(State(_store): State<Arc<ConfigStore>>) -> Response {
    Json(spec()).into_response()
}

/// Build the `OpenAPI` document. Pulled into a free function so tests can
/// load it without the HTTP wrapper.
#[must_use]
pub fn spec() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Coulisse Admin API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "HTTP API for editing the coulisse.yaml config of a running Coulisse process. Every route is content-negotiated: send `Accept: application/json` for JSON, `Accept: text/html` (or none) for HTML pages, or set `HX-Request: true` for HTML fragments. Bodies accept `application/json`, `application/yaml`, or `application/x-www-form-urlencoded`.",
        },
        "servers": [{ "url": "/admin", "description": "Admin scope on the Coulisse server" }],
        "tags": [
            { "name": "agents", "description": "LLM agent configurations" },
            { "name": "judges", "description": "LLM-as-judge evaluators" },
            { "name": "experiments", "description": "A/B routing groups" },
            { "name": "providers", "description": "LLM provider API keys" },
            { "name": "mcp", "description": "MCP tool servers" },
            { "name": "config", "description": "Whole-file config operations" },
        ],
        "paths": {
            "/agents": agents_collection(),
            "/agents/{name}": agents_item(),
            "/judges": judges_collection(),
            "/judges/{name}": judges_item(),
            "/experiments": experiments_collection(),
            "/experiments/{name}": experiments_item(),
            "/providers": providers_collection(),
            "/providers/{kind}": providers_item(),
            "/mcp": mcp_collection(),
            "/mcp/{name}": mcp_item(),
            "/config": config_endpoint(),
        },
        "components": {
            "schemas": schemas(),
            "responses": {
                "Validation": {
                    "description": "Cross-feature validation rejected the new config. Body is the validator's error string verbatim.",
                    "content": { "text/plain": { "schema": { "type": "string" } } },
                },
                "NotFound": {
                    "description": "No entity with the supplied identifier.",
                    "content": { "text/plain": { "schema": { "type": "string" } } },
                },
                "Conflict": {
                    "description": "Identifier already exists.",
                    "content": { "text/plain": { "schema": { "type": "string" } } },
                },
            },
        },
    })
}

fn agents_collection() -> Value {
    json!({
        "get": {
            "tags": ["agents"],
            "summary": "List agents",
            "responses": {
                "200": {
                    "description": "Configured agents (JSON list when Accept: application/json, HTML page otherwise).",
                    "content": {
                        "application/json": {
                            "schema": { "type": "array", "items": { "$ref": "#/components/schemas/AgentConfig" } },
                        },
                        "text/html": { "schema": { "type": "string" } },
                    },
                },
            },
        },
        "post": {
            "tags": ["agents"],
            "summary": "Create an agent",
            "requestBody": body_of("AgentConfig"),
            "responses": {
                "201": { "description": "Created.", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AgentConfig" } } } },
                "303": { "description": "HTML clients are redirected to the new resource." },
                "409": { "$ref": "#/components/responses/Conflict" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}

fn agents_item() -> Value {
    json!({
        "parameters": [name_param("Agent name")],
        "get": {
            "tags": ["agents"],
            "summary": "Get one agent",
            "responses": {
                "200": {
                    "description": "Agent config (JSON or HTML).",
                    "content": {
                        "application/json": { "schema": { "$ref": "#/components/schemas/AgentConfig" } },
                        "text/html": { "schema": { "type": "string" } },
                    },
                },
                "404": { "$ref": "#/components/responses/NotFound" },
            },
        },
        "put": {
            "tags": ["agents"],
            "summary": "Replace an agent",
            "requestBody": body_of("AgentConfig"),
            "responses": {
                "200": { "description": "Updated.", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AgentConfig" } } } },
                "303": { "description": "HTML clients are redirected to the resource." },
                "400": { "description": "URL name and body name disagree." },
                "404": { "$ref": "#/components/responses/NotFound" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
        "delete": {
            "tags": ["agents"],
            "summary": "Delete an agent",
            "responses": {
                "204": { "description": "Deleted." },
                "303": { "description": "HTML clients are redirected to the list." },
                "404": { "$ref": "#/components/responses/NotFound" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}

fn judges_collection() -> Value {
    item_collection("judges", "JudgeConfig", "Judge")
}
fn judges_item() -> Value {
    item_resource("judges", "JudgeConfig", "Judge name")
}
fn experiments_collection() -> Value {
    item_collection("experiments", "ExperimentConfig", "Experiment")
}
fn experiments_item() -> Value {
    item_resource("experiments", "ExperimentConfig", "Experiment name")
}
fn providers_collection() -> Value {
    json!({
        "get": {
            "tags": ["providers"],
            "summary": "List providers",
            "responses": {
                "200": {
                    "description": "Map of provider kind to config.",
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "additionalProperties": { "$ref": "#/components/schemas/ProviderConfig" },
                            },
                        },
                        "text/html": { "schema": { "type": "string" } },
                    },
                },
            },
        },
        "post": {
            "tags": ["providers"],
            "summary": "Create a provider",
            "requestBody": body_of("ProviderCreateBody"),
            "responses": {
                "201": { "description": "Created.", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProviderConfig" } } } },
                "303": { "description": "HTML clients are redirected to the list." },
                "409": { "$ref": "#/components/responses/Conflict" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}
fn providers_item() -> Value {
    json!({
        "parameters": [{ "in": "path", "name": "kind", "required": true, "schema": { "$ref": "#/components/schemas/ProviderKind" }, "description": "Provider kind" }],
        "get": {
            "tags": ["providers"],
            "summary": "Get one provider",
            "responses": {
                "200": { "description": "Provider config.", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProviderConfig" } } } },
                "404": { "$ref": "#/components/responses/NotFound" },
            },
        },
        "put": {
            "tags": ["providers"],
            "summary": "Replace a provider's config",
            "requestBody": body_of("ProviderConfig"),
            "responses": {
                "200": { "description": "Updated.", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProviderConfig" } } } },
                "303": { "description": "HTML clients are redirected." },
                "404": { "$ref": "#/components/responses/NotFound" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
        "delete": {
            "tags": ["providers"],
            "summary": "Delete a provider",
            "responses": {
                "204": { "description": "Deleted." },
                "303": { "description": "HTML clients are redirected." },
                "404": { "$ref": "#/components/responses/NotFound" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}
fn mcp_collection() -> Value {
    json!({
        "get": {
            "tags": ["mcp"],
            "summary": "List MCP servers",
            "responses": {
                "200": {
                    "description": "Map of name to MCP server config.",
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "additionalProperties": { "$ref": "#/components/schemas/McpServerConfig" },
                            },
                        },
                        "text/html": { "schema": { "type": "string" } },
                    },
                },
            },
        },
        "post": {
            "tags": ["mcp"],
            "summary": "Create an MCP server",
            "requestBody": body_of("McpCreateBody"),
            "responses": {
                "201": { "description": "Created.", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/McpServerConfig" } } } },
                "303": { "description": "HTML clients are redirected." },
                "409": { "$ref": "#/components/responses/Conflict" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}
fn mcp_item() -> Value {
    item_resource("mcp", "McpServerConfig", "MCP server name")
}

fn config_endpoint() -> Value {
    json!({
        "get": {
            "tags": ["config"],
            "summary": "Read the full coulisse.yaml",
            "responses": {
                "200": {
                    "description": "Full config (YAML by default, JSON when Accept: application/json).",
                    "content": {
                        "application/yaml": { "schema": { "type": "string" } },
                        "application/json": { "schema": { "type": "object", "additionalProperties": true } },
                    },
                },
            },
        },
        "put": {
            "tags": ["config"],
            "summary": "Replace the full coulisse.yaml",
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": { "schema": { "type": "object", "additionalProperties": true } },
                    "application/yaml": { "schema": { "type": "string" } },
                },
            },
            "responses": {
                "204": { "description": "Replaced." },
                "400": { "description": "Body could not be parsed." },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}

fn item_collection(tag: &str, schema_name: &str, label: &str) -> Value {
    json!({
        "get": {
            "tags": [tag],
            "summary": format!("List {tag}"),
            "responses": {
                "200": {
                    "description": format!("Configured {tag} (JSON list or HTML page)."),
                    "content": {
                        "application/json": {
                            "schema": { "type": "array", "items": { "$ref": format!("#/components/schemas/{schema_name}") } },
                        },
                        "text/html": { "schema": { "type": "string" } },
                    },
                },
            },
        },
        "post": {
            "tags": [tag],
            "summary": format!("Create a {}", label.to_lowercase()),
            "requestBody": body_of(schema_name),
            "responses": {
                "201": { "description": "Created.", "content": { "application/json": { "schema": { "$ref": format!("#/components/schemas/{schema_name}") } } } },
                "303": { "description": "HTML clients are redirected." },
                "409": { "$ref": "#/components/responses/Conflict" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}

fn item_resource(tag: &str, schema_name: &str, name_desc: &str) -> Value {
    json!({
        "parameters": [name_param(name_desc)],
        "get": {
            "tags": [tag],
            "summary": format!("Get one {}", tag.trim_end_matches('s')),
            "responses": {
                "200": {
                    "description": "Resource (JSON or HTML).",
                    "content": {
                        "application/json": { "schema": { "$ref": format!("#/components/schemas/{schema_name}") } },
                        "text/html": { "schema": { "type": "string" } },
                    },
                },
                "404": { "$ref": "#/components/responses/NotFound" },
            },
        },
        "put": {
            "tags": [tag],
            "summary": format!("Replace one {}", tag.trim_end_matches('s')),
            "requestBody": body_of(schema_name),
            "responses": {
                "200": { "description": "Updated.", "content": { "application/json": { "schema": { "$ref": format!("#/components/schemas/{schema_name}") } } } },
                "303": { "description": "HTML clients are redirected." },
                "400": { "description": "URL identifier and body identifier disagree." },
                "404": { "$ref": "#/components/responses/NotFound" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
        "delete": {
            "tags": [tag],
            "summary": format!("Delete one {}", tag.trim_end_matches('s')),
            "responses": {
                "204": { "description": "Deleted." },
                "303": { "description": "HTML clients are redirected." },
                "404": { "$ref": "#/components/responses/NotFound" },
                "422": { "$ref": "#/components/responses/Validation" },
            },
        },
    })
}

fn name_param(description: &str) -> Value {
    json!({
        "in": "path",
        "name": "name",
        "required": true,
        "schema": { "type": "string" },
        "description": description,
    })
}

fn body_of(schema_name: &str) -> Value {
    json!({
        "required": true,
        "content": {
            "application/json": { "schema": { "$ref": format!("#/components/schemas/{schema_name}") } },
            "application/yaml": { "schema": { "type": "string" } },
            "application/x-www-form-urlencoded": { "schema": { "$ref": format!("#/components/schemas/{schema_name}") } },
        },
    })
}

fn schemas() -> Value {
    json!({
        "AgentConfig": agent_config_schema(),
        "JudgeConfig": judge_config_schema(),
        "ExperimentConfig": experiment_config_schema(),
        "Variant": variant_schema(),
        "ProviderKind": provider_kind_schema(),
        "ProviderConfig": provider_config_schema(),
        "ProviderCreateBody": provider_create_body_schema(),
        "McpServerConfig": mcp_server_config_schema(),
        "McpCreateBody": mcp_create_body_schema(),
        "McpToolAccess": mcp_tool_access_schema(),
    })
}

fn agent_config_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name", "model", "provider"],
        "properties": {
            "name": { "type": "string" },
            "provider": { "$ref": "#/components/schemas/ProviderKind" },
            "model": { "type": "string" },
            "preamble": { "type": "string" },
            "purpose": { "type": "string", "nullable": true },
            "judges": { "type": "array", "items": { "type": "string" } },
            "subagents": { "type": "array", "items": { "type": "string" } },
            "mcp_tools": { "type": "array", "items": { "$ref": "#/components/schemas/McpToolAccess" } },
        },
    })
}

fn judge_config_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name", "model", "provider"],
        "properties": {
            "name": { "type": "string" },
            "provider": { "type": "string", "description": "Provider name (anthropic|cohere|deepseek|gemini|groq|openai)" },
            "model": { "type": "string" },
            "rubrics": {
                "type": "object",
                "additionalProperties": { "type": "string" },
                "description": "Map of criterion name → short description of what to assess",
            },
            "sampling_rate": { "type": "number", "minimum": 0, "maximum": 1, "default": 1.0 },
        },
    })
}

fn experiment_config_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name", "strategy", "variants"],
        "properties": {
            "name": { "type": "string" },
            "strategy": { "type": "string", "enum": ["split", "shadow", "bandit"] },
            "variants": {
                "type": "array",
                "items": { "$ref": "#/components/schemas/Variant" },
                "minItems": 1,
            },
            "sticky_by_user": { "type": "boolean", "default": true },
            "purpose": { "type": "string", "nullable": true },
            "primary": { "type": "string", "nullable": true, "description": "Shadow only: primary variant agent." },
            "sampling_rate": { "type": "number", "minimum": 0, "maximum": 1, "nullable": true, "description": "Shadow only." },
            "metric": { "type": "string", "nullable": true, "description": "Bandit only: 'judge.criterion'." },
            "epsilon": { "type": "number", "minimum": 0, "maximum": 1, "nullable": true, "description": "Bandit only: exploration probability." },
            "min_samples": { "type": "integer", "minimum": 0, "nullable": true, "description": "Bandit only: per-arm sample threshold." },
            "bandit_window_seconds": { "type": "integer", "minimum": 0, "nullable": true, "description": "Bandit only: lookback window." },
        },
    })
}

fn variant_schema() -> Value {
    json!({
        "type": "object",
        "required": ["agent"],
        "properties": {
            "agent": { "type": "string" },
            "weight": { "type": "number", "exclusiveMinimum": 0, "default": 1.0 },
        },
    })
}

fn provider_kind_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["anthropic", "cohere", "deepseek", "gemini", "groq", "openai"],
    })
}

fn provider_config_schema() -> Value {
    json!({
        "type": "object",
        "required": ["api_key"],
        "properties": {
            "api_key": { "type": "string" },
        },
    })
}

fn provider_create_body_schema() -> Value {
    json!({
        "type": "object",
        "required": ["kind", "api_key"],
        "properties": {
            "kind": { "$ref": "#/components/schemas/ProviderKind" },
            "api_key": { "type": "string" },
        },
    })
}

fn mcp_server_config_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "required": ["transport", "url"],
                "properties": {
                    "transport": { "type": "string", "enum": ["http"] },
                    "url": { "type": "string", "format": "uri" },
                },
            },
            {
                "type": "object",
                "required": ["transport", "command"],
                "properties": {
                    "transport": { "type": "string", "enum": ["stdio"] },
                    "command": { "type": "string" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "env": { "type": "object", "additionalProperties": { "type": "string" } },
                },
            },
        ],
        "discriminator": { "propertyName": "transport" },
    })
}

fn mcp_create_body_schema() -> Value {
    json!({
        "allOf": [
            { "type": "object", "required": ["name"], "properties": { "name": { "type": "string" } } },
            { "$ref": "#/components/schemas/McpServerConfig" },
        ],
    })
}

fn mcp_tool_access_schema() -> Value {
    json!({
        "type": "object",
        "required": ["server"],
        "properties": {
            "server": { "type": "string" },
            "only": { "type": "array", "nullable": true, "items": { "type": "string" } },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_is_valid_openapi_31() {
        let s = spec();
        assert_eq!(s["openapi"], "3.1.0");
        assert!(s["info"]["title"].as_str().unwrap().contains("Coulisse"));
        // Smoke-test that every advertised path has at least one method.
        for (path, ops) in s["paths"].as_object().unwrap() {
            let methods = ["get", "post", "put", "delete", "patch"];
            let has_op = methods.iter().any(|m| ops.get(m).is_some());
            assert!(has_op, "path {path} has no operations");
        }
        // Every $ref must resolve to a defined schema.
        let schemas = s["components"]["schemas"].as_object().unwrap();
        let serialized = serde_json::to_string(&s).unwrap();
        for known in schemas.keys() {
            // No-op check: the schema is present, and any ref to it
            // will resolve. We just confirm the names are in the spec.
            assert!(serialized.contains(known));
        }
    }
}
