use providers::ToolCallKind;
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use tracing::{Instrument, info_span};

/// Tool decorator that opens a `tool_call` tracing span around any inner
/// `ToolDyn`. The telemetry crate's SqliteLayer mirrors the span (with
/// `args`, `result`, `error`, `tool_name`, `kind` fields) into the
/// `events` and `tool_calls` tables — closing the blind spot that let
/// hallucinated tool-failure replies hide real upstream errors.
pub(crate) struct TelemetryTool {
    pub(crate) inner: Box<dyn ToolDyn>,
    pub(crate) kind: ToolCallKind,
}

impl ToolDyn for TelemetryTool {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn definition(&self, prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        self.inner.definition(prompt)
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let kind_str = match self.kind {
            ToolCallKind::Mcp => "mcp",
            ToolCallKind::Subagent => "subagent",
        };
        let name = self.inner.name();
        let inner_call = self.inner.call(args.clone());
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = kind_str,
            result = tracing::field::Empty,
            tool_name = %name,
        );
        Box::pin(
            async move {
                let result = inner_call.await;
                let span = tracing::Span::current();
                match &result {
                    Ok(text) => span.record("result", text.as_str()),
                    Err(err) => span.record("error", err.to_string().as_str()),
                };
                result
            }
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use coulisse_core::UserId;
    use rig::completion::ToolDefinition;
    use serde_json::json;
    use sqlx::SqlitePool;
    use sqlx::sqlite::SqliteConnectOptions;
    use telemetry::{Sink, SqliteLayer, TurnId};
    use tracing::Instrument;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    use super::*;

    /// Minimal `ToolDyn` that returns whatever result/error the test wires
    /// in. Replaces `McpTool` in tests so the wrapper logic can be exercised
    /// without a live MCP server.
    struct FakeTool {
        name: String,
        outcome: Result<String, String>,
    }

    impl ToolDyn for FakeTool {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
            let name = self.name.clone();
            Box::pin(async move {
                ToolDefinition {
                    name,
                    description: "fake".into(),
                    parameters: json!({}),
                }
            })
        }

        fn call(&self, _args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
            let outcome = self.outcome.clone();
            Box::pin(async move {
                outcome.map_err(|e| {
                    ToolError::ToolCallError(Box::<dyn std::error::Error + Send + Sync>::from(e))
                })
            })
        }
    }

    async fn fresh_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(options).await.unwrap();
        Sink::open(pool.clone()).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn wrapper_emits_event_on_success() {
        let pool = fresh_pool().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let (layer, guard) = SqliteLayer::spawn(pool.clone());
        let _default = tracing_subscriber::registry().with(layer).set_default();

        async {
            let wrapper = TelemetryTool {
                inner: Box::new(FakeTool {
                    name: "search_jobs".into(),
                    outcome: Ok("hello".into()),
                }),
                kind: ToolCallKind::Mcp,
            };
            let out = wrapper.call("{\"q\":\"x\"}".into()).await.unwrap();
            assert_eq!(out, "hello");
        }
        .instrument(tracing::info_span!(
            "turn",
            agent = "test",
            turn_id = %turn.0,
            user_id = %user.0,
            user_message = "",
        ))
        .await;

        guard.flush().await;
        let sink = Sink::open(pool).await.unwrap();
        let events = sink.fetch_turn(user, turn).await.unwrap();
        let tool_evt = events
            .iter()
            .find(|e| e.kind == telemetry::EventKind::ToolCall)
            .expect("tool_call event recorded");
        assert_eq!(tool_evt.payload["tool_name"], "search_jobs");
        assert_eq!(tool_evt.payload["kind"], "mcp");
        assert_eq!(tool_evt.payload["result"], "hello");
        assert!(
            tool_evt
                .payload
                .get("error")
                .map(|v| v.is_null())
                .unwrap_or(true)
        );
        assert!(tool_evt.duration_ms.is_some());
    }

    #[tokio::test]
    async fn wrapper_captures_tool_error() {
        let pool = fresh_pool().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let (layer, guard) = SqliteLayer::spawn(pool.clone());
        let _default = tracing_subscriber::registry().with(layer).set_default();

        let err = async {
            let wrapper = TelemetryTool {
                inner: Box::new(FakeTool {
                    name: "search_jobs".into(),
                    outcome: Err("column j.search_vector does not exist".into()),
                }),
                kind: ToolCallKind::Mcp,
            };
            wrapper.call("{}".into()).await.unwrap_err()
        }
        .instrument(tracing::info_span!(
            "turn",
            agent = "test",
            turn_id = %turn.0,
            user_id = %user.0,
            user_message = "",
        ))
        .await;
        // rig's ToolError renders to the underlying Display; assert that the
        // error text flows through unchanged.
        assert!(err.to_string().contains("search_vector"));

        guard.flush().await;
        let sink = Sink::open(pool).await.unwrap();
        let events = sink.fetch_turn(user, turn).await.unwrap();
        let tool_evt = events
            .iter()
            .find(|e| e.kind == telemetry::EventKind::ToolCall)
            .expect("tool_call event recorded");
        let payload = &tool_evt.payload;
        assert_eq!(payload["kind"], "mcp");
        assert!(payload.get("result").map(|v| v.is_null()).unwrap_or(true));
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or("")
                .contains("search_vector"),
            "expected error text in payload, got {}",
            payload["error"]
        );
    }

    #[tokio::test]
    async fn wrapper_is_transparent_without_subscriber() {
        // No subscriber installed — span emissions are no-ops; the tool still
        // runs and returns its underlying result.
        let wrapper = TelemetryTool {
            inner: Box::new(FakeTool {
                name: "search_jobs".into(),
                outcome: Ok("ok".into()),
            }),
            kind: ToolCallKind::Mcp,
        };
        let out = wrapper.call("{}".into()).await.unwrap();
        assert_eq!(out, "ok");
    }
}
