use std::sync::Arc;

use coulisse_core::TaskStatus;
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde_json::{Value, json};
use tracing::{Instrument, info_span};

/// Read-only counterpart to `DispatchTaskTool`. Reports recent tasks across
/// every agent — queued, running, done, or errored — so an orchestrator can
/// answer "what's going on right now?" from chat without needing operational
/// access to `/admin/live`.
///
/// The tool is bound to a `TaskStatus` impl at construction time; the model
/// chooses only the limit and the optional state filter.
pub(crate) struct TasksStatusTool {
    pub(crate) status: Arc<dyn TaskStatus>,
}

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;

impl ToolDyn for TasksStatusTool {
    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let status = Arc::clone(&self.status);
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = "tasks_status",
            result = tracing::field::Empty,
            tool_name = "tasks_status",
        );
        Box::pin(
            async move {
                let parsed: Value = if args.trim().is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&args).map_err(ToolError::JsonError)?
                };
                let limit = parsed
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(DEFAULT_LIMIT, |n| {
                        u32::try_from(n).unwrap_or(MAX_LIMIT).min(MAX_LIMIT)
                    });
                let state_filter = parsed
                    .get("state")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let summaries = status
                    .recent(limit)
                    .await
                    .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;
                let filtered: Vec<_> = summaries
                    .into_iter()
                    .filter(|s| state_filter.as_ref().is_none_or(|want| s.state == *want))
                    .map(|s| {
                        json!({
                            "agent": s.agent,
                            "created_at": s.created_at,
                            "error": s.error,
                            "finished_at": s.finished_at,
                            "id": s.id.0.to_string(),
                            "prompt": truncate(&s.prompt, 200),
                            "result": s.result.as_deref().map(|r| truncate(r, 200)),
                            "started_at": s.started_at,
                            "state": s.state,
                        })
                    })
                    .collect();
                let result = serde_json::to_string(&json!({ "tasks": filtered }))
                    .map_err(ToolError::JsonError)?;
                tracing::Span::current().record("result", result.as_str());
                Ok(result)
            }
            .instrument(span),
        )
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: "tasks_status".to_string(),
                description: "Report recent background tasks across every agent — queued, \
                              running, done, or errored. Use this to answer \"what's going on \
                              right now?\" without needing the studio /admin/live page. \
                              Returns a JSON object with a `tasks` array; each entry has \
                              agent, state, prompt (truncated), and timestamps."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_LIMIT,
                            "description": "Maximum number of tasks to return, newest first.",
                        },
                        "state": {
                            "type": "string",
                            "enum": ["queued", "running", "done", "errored"],
                            "description": "Optional filter — only return tasks in this state.",
                        }
                    },
                    "required": [],
                }),
            }
        })
    }

    fn name(&self) -> String {
        "tasks_status".to_string()
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}
