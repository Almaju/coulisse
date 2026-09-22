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
//! Coulisse stays platform-agnostic: it knows nothing about Slack,
//! GitHub, or any other source. Bridges live outside the binary as
//! separate processes that POST JSON to the configured path.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use coulisse_core::{TaskQueue, TaskSubmission, UserId};
use serde_json::Value;
use tracing::{error, info};

use crate::config::{TriggerConfig, TriggerKind};

/// One `type: webhook` entry, ready to serve `POST <path>`. Doubles as the
/// axum handler state so each mounted route carries its own trigger.
#[derive(Clone)]
pub(crate) struct WebhookTrigger {
    agent_template: String,
    name: String,
    path: String,
    prompt_template: String,
    queue: Arc<dyn TaskQueue>,
    user_id: UserId,
}

impl WebhookTrigger {
    /// `None` when `config` is not a webhook trigger.
    pub(crate) fn from_config(
        config: &TriggerConfig,
        queue: Arc<dyn TaskQueue>,
        user_id: UserId,
    ) -> Option<Self> {
        let TriggerKind::Webhook { path } = &config.kind else {
            return None;
        };
        Some(Self {
            agent_template: config.agent.clone(),
            name: config.name.clone(),
            path: path.clone(),
            prompt_template: config.prompt.clone(),
            queue,
            user_id,
        })
    }

    pub(crate) fn mount(self, router: Router) -> Router {
        let path = self.path.clone();
        router.route(&path, post(Self::handle).with_state(self))
    }

    async fn handle(
        State(trigger): State<Self>,
        Json(payload): Json<Value>,
    ) -> Result<Json<Value>, StatusCode> {
        let agent = substitute(&trigger.agent_template, &payload);
        let prompt = substitute(&trigger.prompt_template, &payload);
        // Reject payloads that left the `agent` field unresolved. A literal
        // `{{ name }}` survives substitution when the path is missing — at
        // that point we can't enqueue, the worker would just fail later
        // with an "unknown agent" task error.
        if agent.contains("{{") || agent.trim().is_empty() {
            error!(
                trigger = %trigger.name,
                template = %trigger.agent_template,
                resolved = %agent,
                "webhook agent template did not resolve to a name"
            );
            return Err(StatusCode::BAD_REQUEST);
        }
        let submission = TaskSubmission {
            agent: &agent,
            prompt: &prompt,
            user_id: trigger.user_id,
        };
        match trigger.queue.submit(submission).await {
            Ok(task_id) => {
                info!(
                    trigger = %trigger.name,
                    agent = %agent,
                    task_id = %task_id.0,
                    "webhook trigger fired"
                );
                Ok(Json(serde_json::json!({
                    "ok": true,
                    "task_id": task_id.0.to_string(),
                })))
            }
            Err(e) => {
                error!(trigger = %trigger.name, %e, "webhook trigger failed to enqueue");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
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

    use std::sync::Mutex;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::Response;
    use coulisse_core::{BoxFuture, TaskId, TaskQueue, TaskQueueError, TaskSubmission, UserId};
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::Triggers;
    use crate::config::{TriggerConfig, TriggerKind};

    #[derive(Default)]
    struct CapturingQueue {
        calls: Mutex<Vec<(String, String)>>,
    }

    impl TaskQueue for CapturingQueue {
        fn submit<'a>(
            &'a self,
            submission: TaskSubmission<'a>,
        ) -> BoxFuture<'a, Result<TaskId, TaskQueueError>> {
            let captured = (submission.agent.to_string(), submission.prompt.to_string());
            Box::pin(async move {
                self.calls.lock().unwrap().push(captured);
                Ok(TaskId::new())
            })
        }
    }

    fn router_with(triggers: &[TriggerConfig], queue: Arc<CapturingQueue>) -> Router {
        Triggers::new(triggers, queue as Arc<dyn TaskQueue>, UserId::new()).webhook_router()
    }

    fn chat_trigger(agent_template: &str) -> TriggerConfig {
        TriggerConfig {
            agent: agent_template.to_string(),
            kind: TriggerKind::Webhook {
                path: "/hooks/chat".to_string(),
            },
            name: "chat-mention".to_string(),
            prompt: "@{{sender}}: {{body}}".to_string(),
        }
    }

    async fn post_json(app: Router, body: serde_json::Value) -> Response {
        let req = Request::builder()
            .method("POST")
            .uri("/hooks/chat")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        app.oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn templated_agent_resolves_from_payload() {
        let queue = Arc::new(CapturingQueue::default());
        let app = router_with(&[chat_trigger("{{agent}}")], Arc::clone(&queue));
        let resp = post_json(
            app,
            serde_json::json!({"agent": "pm", "sender": "almaju", "body": "hi"}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let calls = queue.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "pm");
        assert_eq!(calls[0].1, "@almaju: hi");
    }

    #[tokio::test]
    async fn static_agent_still_works() {
        let queue = Arc::new(CapturingQueue::default());
        let app = router_with(&[chat_trigger("coder")], Arc::clone(&queue));
        let resp = post_json(app, serde_json::json!({"sender": "almaju", "body": "x"})).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let calls = queue.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "coder");
    }

    #[tokio::test]
    async fn unresolved_agent_template_returns_400() {
        let queue = Arc::new(CapturingQueue::default());
        let app = router_with(&[chat_trigger("{{agent}}")], Arc::clone(&queue));
        // Payload is missing the `agent` field — the placeholder survives
        // substitution and the handler should reject before enqueueing.
        let resp = post_json(app, serde_json::json!({"sender": "almaju", "body": "x"})).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        assert!(queue.calls.lock().unwrap().is_empty());
    }
}
