use std::sync::Arc;

use coulisse_core::{TaskQueue, UserId};
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde_json::json;
use tracing::{Instrument, info_span};

/// Fire-and-forget cousin of `SubagentTool`. Where `SubagentTool` runs the
/// target agent inline and waits for the reply, this enqueues the work and
/// returns a `task_id` immediately. A worker pool in `cli` picks the task up
/// and runs it through the same handler as a sync request.
///
/// Bound at tool-construction time to the caller's `user_id`, so the model
/// can't dispatch work under another user's identity — only the agent name
/// and the initial prompt come from the model.
pub(crate) struct DispatchTaskTool {
    pub(crate) queue: Arc<dyn TaskQueue>,
    pub(crate) user_id: UserId,
}

impl ToolDyn for DispatchTaskTool {
    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let queue = Arc::clone(&self.queue);
        let user_id = self.user_id;
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = "dispatch_task",
            result = tracing::field::Empty,
            tool_name = "dispatch_task",
        );
        Box::pin(
            async move {
                let parsed: serde_json::Value =
                    serde_json::from_str(&args).map_err(ToolError::JsonError)?;
                let agent = parsed
                    .get("agent")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::ToolCallError(Box::<dyn std::error::Error + Send + Sync>::from(
                            "dispatch_task call is missing required 'agent' field",
                        ))
                    })?;
                let prompt = parsed
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::ToolCallError(Box::<dyn std::error::Error + Send + Sync>::from(
                            "dispatch_task call is missing required 'prompt' field",
                        ))
                    })?;
                let task_id = queue
                    .submit(agent, prompt, user_id)
                    .await
                    .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;
                let result = format!(
                    "task {task_id} queued for agent '{agent}'. \
                     The worker pool will run it in the background; \
                     refer to it by id when narrating progress.",
                    task_id = task_id.0,
                );
                tracing::Span::current().record("result", result.as_str());
                Ok(result)
            }
            .instrument(span),
        )
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: "dispatch_task".to_string(),
                description: "Enqueue a fire-and-forget background task that runs the named \
                              agent with the given prompt. Returns immediately with a task_id. \
                              Use this when the request is genuinely async — research, long \
                              analyses, periodic narration — rather than for steps you need an \
                              answer to before you can continue."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "agent": {
                            "type": "string",
                            "description": "Name of the agent to run in the background.",
                        },
                        "prompt": {
                            "type": "string",
                            "description": "Initial user message for the background agent. \
                                            It starts with a fresh context.",
                        }
                    },
                    "required": ["agent", "prompt"],
                }),
            }
        })
    }

    fn name(&self) -> String {
        "dispatch_task".to_string()
    }
}
