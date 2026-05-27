use std::sync::Arc;

use coulisse_core::UserId;
use providers::{Message, Role};
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{Instrument, info_span};

use crate::runtime::AgentsInner;

/// A rig tool that, when called, invokes another agent as a fresh
/// conversation. The subagent runs under its own preamble, its own MCP tool
/// list, and its own bounded tool loop; the final assistant text becomes
/// this tool's return value. Hop depth is captured at construction so
/// pathological A→B→A→B chains are bounded by `MAX_SUBAGENT_DEPTH`.
///
/// `target_name` is the addressable name as written in YAML — either an
/// agent or an experiment. Resolution happens at call time so each
/// subagent invocation goes through the router, picking a variant for
/// the calling user.
///
/// Each invocation opens a `tool_call` tracing span so the subagent's
/// inner tool calls nest underneath it in the studio tree.
///
/// When `handoff_tx` is `Some`, the tool sends the resolved agent name
/// on it before starting the subagent call, so the parent SSE stream can
/// emit a `handoff_started` event immediately.
pub(crate) struct SubagentTool {
    pub(crate) depth: usize,
    /// Sends the resolved agent name to the parent streaming loop before
    /// the blocking subagent call starts, enabling immediate SSE notification.
    pub(crate) handoff_tx: Option<mpsc::Sender<String>>,
    pub(crate) inner: Arc<AgentsInner>,
    pub(crate) purpose: String,
    pub(crate) target_name: String,
    /// Calling user — only used for sticky-by-user variant resolution
    /// when the subagent target is an experiment. Not observability.
    pub(crate) user_id: UserId,
}

impl ToolDyn for SubagentTool {
    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let depth = self.depth;
        let handoff_tx = self.handoff_tx.clone();
        let inner = Arc::clone(&self.inner);
        let target = self.target_name.clone();
        let user_id = self.user_id;
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = "subagent",
            result = tracing::field::Empty,
            tool_name = %target,
        );
        Box::pin(
            async move {
                let parsed: serde_json::Value =
                    serde_json::from_str(&args).map_err(ToolError::JsonError)?;
                let message = parsed
                    .get("message")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::ToolCallError(Box::<dyn std::error::Error + Send + Sync>::from(
                            "subagent tool call is missing required 'message' field",
                        ))
                    })?
                    .to_string();
                let messages = vec![Message {
                    role: Role::User,
                    content: message,
                }];
                let next_depth = depth.saturating_add(1);
                // WHY: subagent name may be an experiment — defer resolution
                // to the runtime's resolver so the variant is picked at call
                // time, consistent with the sticky-by-user hashing the proxy
                // applies at the top level.
                let agent_name = inner.resolver.resolve(&target, user_id).await;

                // Notify the parent SSE stream immediately so the client sees
                // a `handoff_started` event rather than silence.
                if let Some(tx) = &handoff_tx {
                    let _ = tx.try_send(agent_name.clone());
                }

                let outcome = AgentsInner::complete_with_depth(
                    &inner,
                    &agent_name,
                    messages,
                    next_depth,
                    user_id,
                )
                .await;

                let span = tracing::Span::current();
                match &outcome {
                    Err(err) => span.record("error", err.to_string().as_str()),
                    Ok(completion) => span.record("result", completion.text.as_str()),
                };

                match outcome {
                    Err(err) => Err(ToolError::ToolCallError(Box::new(err))),
                    Ok(completion) => Ok(completion.text),
                }
            }
            .instrument(span),
        )
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let name = self.target_name.clone();
        let description = self.purpose.clone();
        Box::pin(async move {
            ToolDefinition {
                name,
                description,
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Natural-language message or instruction to send to the subagent. The subagent starts with a fresh context and sees only this message.",
                        }
                    },
                    "required": ["message"],
                }),
            }
        })
    }

    fn name(&self) -> String {
        self.target_name.clone()
    }
}
