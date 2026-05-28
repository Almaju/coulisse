use std::sync::Arc;

use agents::DynamicAgents;
use auth::Auth;
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::extract::{Json, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use limits::Tracker;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tasks::{Task, Tasks};

#[derive(Clone)]
pub struct McpAdminState {
    pub auth: Auth,
    pub dynamic_agents: Arc<DynamicAgents>,
    pub tasks: Arc<Tasks>,
    pub tracker: Arc<Tracker>,
}

/// Build the `/mcp-admin` router. Returns `None` when `auth.mcp_admin`
/// is not configured — the endpoint should not be mounted at all in that case.
pub fn router(state: McpAdminState) -> Option<Router> {
    if !state.auth.mcp_admin_enabled() {
        return None;
    }
    let app = Router::new()
        // Agents
        .route("/mcp-admin/agents", get(list_agents))
        .route("/mcp-admin/agents/:name", get(get_agent))
        .route("/mcp-admin/agents/:name", post(update_agent))
        // Tasks
        .route("/mcp-admin/tasks", get(list_tasks))
        .route("/mcp-admin/tasks/:id", get(get_task))
        .route("/mcp-admin/tasks/:id/cancel", post(cancel_task))
        .route("/mcp-admin/tasks/:id/requeue", post(requeue_task))
        // Rate limits
        .route("/mcp-admin/rate-limits/:user_id", delete(reset_rate_limit))
        .layer(from_fn_with_state(state.clone(), mcp_admin_auth))
        .with_state(state);
    Some(app)
}

// ─── Auth middleware ───────────────────────────────────────────────────────────

async fn mcp_admin_auth(State(state): State<McpAdminState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| state.auth.check_mcp_admin_bearer(h));
    if ok {
        next.run(req).await
    } else {
        let mut resp = Response::new(Body::from("mcp_admin authentication required"));
        *resp.status_mut() = StatusCode::UNAUTHORIZED;
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            r#"Bearer realm="coulisse-mcp-admin""#.parse().expect("static"),
        );
        resp.into_response()
    }
}

// ─── Shared helpers ────────────────────────────────────────────────────────────

fn caller_hash(req_headers: &axum::http::HeaderMap) -> String {
    req_headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|h| {
            let mut hasher = Sha256::new();
            hasher.update(h.as_bytes());
            format!("sha256:{}", hex::encode(hasher.finalize()))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

// ─── Agents ───────────────────────────────────────────────────────────────────

async fn list_agents(State(state): State<McpAdminState>) -> Response {
    match state.dynamic_agents.list().await {
        Ok(rows) => {
            let items: Vec<_> = rows
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "disabled": r.disabled,
                        "name": r.name,
                        "config": r.config,
                    })
                })
                .collect();
            Json(serde_json::json!({ "agents": items })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_agent(State(state): State<McpAdminState>, Path(name): Path<String>) -> Response {
    match state.dynamic_agents.list().await {
        Ok(rows) => match rows.into_iter().find(|r| r.name == name) {
            Some(row) => Json(serde_json::json!({
                "config": row.config,
                "disabled": row.disabled,
                "name": row.name,
            }))
            .into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "agent not found" })),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct UpdateAgentBody {
    config: agents::AgentConfig,
}

async fn update_agent(
    State(state): State<McpAdminState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<UpdateAgentBody>,
) -> Response {
    let caller = caller_hash(&headers);
    match state.dynamic_agents.put_active(&name, &body.config).await {
        Ok(()) => {
            tracing::info!(
                tool = "update_agent",
                caller = %caller,
                agent = %name,
                outcome = "ok",
                "mcp_admin_call"
            );
            Json(serde_json::json!({
                "name": name,
                "persistent": false,
                "warning": "volatile — changes are lost on Coulisse restart; \
                            edit coulisse.yaml to make them permanent",
            }))
            .into_response()
        }
        Err(e) => {
            tracing::warn!(
                tool = "update_agent",
                caller = %caller,
                agent = %name,
                outcome = "error",
                error = %e,
                "mcp_admin_call"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

// ─── Tasks ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ListTasksQuery {
    #[serde(default = "default_task_limit")]
    limit: u32,
    state: Option<String>,
}

fn default_task_limit() -> u32 {
    50
}

fn task_to_json(t: &Task) -> serde_json::Value {
    serde_json::json!({
        "agent": t.agent,
        "created_at": t.created_at,
        "error": t.error,
        "finished_at": t.finished_at,
        "id": t.id.0.to_string(),
        "prompt": t.prompt,
        "result": t.result,
        "started_at": t.started_at,
        "state": t.state.as_str(),
        "user_id": t.user_id.0.to_string(),
    })
}

async fn list_tasks(
    State(state): State<McpAdminState>,
    Query(q): Query<ListTasksQuery>,
) -> Response {
    match state.tasks.recent(q.limit).await {
        Ok(mut tasks) => {
            if let Some(filter) = &q.state {
                tasks.retain(|t| t.state.as_str() == filter.as_str());
            }
            let items: Vec<_> = tasks.iter().map(task_to_json).collect();
            Json(serde_json::json!({ "tasks": items })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_task(State(state): State<McpAdminState>, Path(id): Path<String>) -> Response {
    let task_id = match parse_task_id(&id) {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid task id" })),
            )
                .into_response();
        }
    };
    match state.tasks.get(task_id).await {
        Ok(Some(t)) => Json(task_to_json(&t)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "task not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn cancel_task(
    State(state): State<McpAdminState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let caller = caller_hash(&headers);
    let task_id = match parse_task_id(&id) {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid task id" })),
            )
                .into_response();
        }
    };
    // Cancel = mark errored with a sentinel reason. Workers that already
    // claimed the task will finish normally, but no retry will be attempted.
    match state
        .tasks
        .mark_errored(task_id, "cancelled via mcp_admin")
        .await
    {
        Ok(()) => {
            tracing::info!(
                tool = "cancel_task",
                caller = %caller,
                task = %id,
                outcome = "ok",
                "mcp_admin_call"
            );
            Json(serde_json::json!({ "id": id, "state": "errored" })).into_response()
        }
        Err(e) => {
            tracing::warn!(
                tool = "cancel_task",
                caller = %caller,
                task = %id,
                outcome = "error",
                error = %e,
                "mcp_admin_call"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

async fn requeue_task(
    State(state): State<McpAdminState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let caller = caller_hash(&headers);
    let task_id = match parse_task_id(&id) {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid task id" })),
            )
                .into_response();
        }
    };
    match state.tasks.requeue(task_id).await {
        Ok(true) => {
            tracing::info!(
                tool = "requeue_task",
                caller = %caller,
                task = %id,
                outcome = "ok",
                "mcp_admin_call"
            );
            Json(serde_json::json!({ "id": id, "state": "queued" })).into_response()
        }
        Ok(false) => {
            tracing::warn!(
                tool = "requeue_task",
                caller = %caller,
                task = %id,
                outcome = "not_found_or_not_errored",
                "mcp_admin_call"
            );
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "task not found or not in errored state; only errored tasks can be requeued"
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(
                tool = "requeue_task",
                caller = %caller,
                task = %id,
                outcome = "error",
                error = %e,
                "mcp_admin_call"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

// ─── Rate limits ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ResetRateLimitQuery {
    /// Explicit confirmation required to prevent accidental resets.
    #[serde(default)]
    confirm: bool,
}

async fn reset_rate_limit(
    State(state): State<McpAdminState>,
    headers: axum::http::HeaderMap,
    Path(user_id): Path<String>,
    Query(q): Query<ResetRateLimitQuery>,
) -> Response {
    let caller = caller_hash(&headers);
    if !q.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "pass ?confirm=true to confirm rate-limit reset; this is destructive",
            })),
        )
            .into_response();
    }
    // Reject wildcards / empty strings to prevent accidental bulk resets.
    if user_id.is_empty() || user_id.contains('*') || user_id.contains('%') {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "user_id must be a specific identifier, not a wildcard",
            })),
        )
            .into_response();
    }
    match state.tracker.reset_scope(&user_id).await {
        Ok(cleared) => {
            tracing::info!(
                tool = "reset_rate_limit",
                caller = %caller,
                user_id = %user_id,
                outcome = if cleared { "ok" } else { "not_found" },
                "mcp_admin_call"
            );
            Json(serde_json::json!({
                "cleared": cleared,
                "user_id": user_id,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::warn!(
                tool = "reset_rate_limit",
                caller = %caller,
                user_id = %user_id,
                outcome = "error",
                error = %e,
                "mcp_admin_call"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn parse_task_id(s: &str) -> Option<coulisse_core::TaskId> {
    uuid::Uuid::parse_str(s).ok().map(coulisse_core::TaskId)
}
