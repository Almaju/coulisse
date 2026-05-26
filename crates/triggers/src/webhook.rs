//! Webhook trigger — Coulisse's universal HTTP entry point for outside
//! systems that want to summon an agent.
//!
//! For each `type: webhook` entry under `triggers:`, Coulisse exposes
//! `POST <path>`. An inbound JSON payload is fed through a simple
//! `{{a.b.c}}` template substitution (the trigger's `prompt` field is the
//! template), and the result becomes the user message of a new task on
//! the queue. Everything else — the worker pool, the agent runtime, the
//! `/admin/live` board — sees the resulting task as identical to one
//! produced by cron or by `dispatch_task`.
//!
//! Coulisse stays platform-agnostic: it knows nothing about Matrix or
//! Slack or GitHub. Bridges live outside the binary as separate
//! processes that POST JSON to the configured path.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use coulisse_core::{TaskQueue, UserId};
use serde_json::Value;
use tracing::{error, info};

use crate::config::{TriggerConfig, TriggerKind};

#[derive(Clone)]
struct HookState {
    agent: String,
    name: String,
    prompt_template: String,
    queue: Arc<dyn TaskQueue>,
    user_id: UserId,
}

/// Build an axum router that mounts one `POST` handler per webhook
/// trigger. Non-webhook entries (cron, future variants) are ignored.
///
/// The returned router uses the unit state `()`; each handler holds its
/// own per-trigger state baked in.
pub fn webhook_router(
    triggers: &[TriggerConfig],
    queue: Arc<dyn TaskQueue>,
    user_id: UserId,
) -> Router {
    let mut router = Router::new();
    for t in triggers {
        let TriggerKind::Webhook { path } = &t.kind else {
            continue;
        };
        let state = HookState {
            agent: t.agent.clone(),
            name: t.name.clone(),
            prompt_template: t.prompt.clone(),
            queue: Arc::clone(&queue),
            user_id,
        };
        router = router.route(path, post(handle).with_state(state));
    }
    router
}

async fn handle(
    State(state): State<HookState>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let prompt = substitute(&state.prompt_template, &payload);
    match state.queue.submit(&state.agent, &prompt, state.user_id).await {
        Ok(task_id) => {
            info!(
                trigger = %state.name,
                agent = %state.agent,
                task_id = %task_id.0,
                "webhook trigger fired"
            );
            Ok(Json(serde_json::json!({
                "ok": true,
                "task_id": task_id.0.to_string(),
            })))
        }
        Err(e) => {
            error!(trigger = %state.name, %e, "webhook trigger failed to enqueue");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Substitute `{{a.b.c}}` placeholders in `template` with values walked
/// from `payload`. Missing paths render as the literal `{{ path }}` so
/// debugging is obvious. JSON strings substitute as their unquoted value;
/// other JSON types substitute as their default `Display`.
pub(crate) fn substitute(template: &str, payload: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            out.push_str("{{");
            rest = after_open;
            continue;
        };
        let path = after_open[..end].trim();
        let value = walk(payload, path).unwrap_or_else(|| format!("{{{{ {path} }}}}"));
        out.push_str(&value);
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    out
}

fn walk(value: &Value, path: &str) -> Option<String> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    match current {
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::substitute;
    use serde_json::json;

    #[test]
    fn flat_substitution() {
        let payload = json!({"name": "alex", "body": "hello"});
        let out = substitute("{{name}}: {{body}}", &payload);
        assert_eq!(out, "alex: hello");
    }

    #[test]
    fn nested_path() {
        let payload = json!({
            "event": {
                "pull_request": {
                    "html_url": "https://github.com/x/y/pull/1"
                }
            }
        });
        let out = substitute("Review {{event.pull_request.html_url}}", &payload);
        assert_eq!(out, "Review https://github.com/x/y/pull/1");
    }

    #[test]
    fn missing_path_is_visible() {
        let payload = json!({"name": "alex"});
        let out = substitute("{{name}}: {{missing}}", &payload);
        assert_eq!(out, "alex: {{ missing }}");
    }

    #[test]
    fn no_placeholders_returns_input() {
        let payload = json!({});
        let out = substitute("static message", &payload);
        assert_eq!(out, "static message");
    }

    #[test]
    fn whitespace_in_path_trimmed() {
        let payload = json!({"x": "ok"});
        let out = substitute("{{  x  }}", &payload);
        assert_eq!(out, "ok");
    }

    #[test]
    fn non_string_value_stringified() {
        let payload = json!({"count": 42});
        let out = substitute("count={{count}}", &payload);
        assert_eq!(out, "count=42");
    }

    #[test]
    fn unclosed_brace_preserved() {
        let payload = json!({});
        let out = substitute("{{unterminated", &payload);
        assert_eq!(out, "{{unterminated");
    }
}
