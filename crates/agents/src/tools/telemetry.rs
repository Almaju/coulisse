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
