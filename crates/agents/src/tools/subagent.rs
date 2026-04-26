use std::sync::Arc;

use coulisse_core::UserId;
use providers::{Message, Role};
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde_json::json;
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
pub(crate) struct SubagentTool {
    pub(crate) depth: usize,
    pub(crate) inner: Arc<AgentsInner>,
    pub(crate) purpose: String,
    pub(crate) target_name: String,
    /// Calling user — only used for sticky-by-user variant resolution
    /// when the subagent target is an experiment. Not observability.
    pub(crate) user_id: UserId,
}

impl ToolDyn for SubagentTool {
    fn name(&self) -> String {
        self.target_name.clone()
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

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let depth = self.depth;
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
                // Subagent name may be an experiment — resolve per-user so the
                // variant is picked at call time, consistent with the
                // sticky-by-user hashing the proxy applies at the top level.
                // Bandit experiments additionally consult recent mean scores;
                // without a score store wired in, the lookup returns no data
                // and the bandit falls back to forced exploration.
                let scores = if let (Some(scores), Some((judge, criterion, since))) =
                    (inner.scores.as_ref(), inner.router.bandit_query(&target))
                {
                    scores
                        .mean_scores_by_agent(&judge, &criterion, since)
                        .await
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let resolved = inner.router.resolve_with_scores(&target, user_id, &scores);
                let agent_name = resolved.agent.into_owned();
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
                    Ok(completion) => span.record("result", completion.text.as_str()),
                    Err(err) => span.record("error", err.to_string().as_str()),
                };

                match outcome {
                    Ok(completion) => Ok(completion.text),
                    Err(err) => Err(ToolError::ToolCallError(Box::new(err))),
                }
            }
            .instrument(span),
        )
    }
}
