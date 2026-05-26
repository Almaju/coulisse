//! Tool-name sanitization layer.
//!
//! MCP servers are free to expose any UTF-8 tool name they like. LLM
//! providers are not: Anthropic enforces `^[a-zA-Z0-9_-]{1,128}$` and OpenAI
//! is similar. The `ricelines/matrix-mcp` server, for instance, names every
//! tool with dots (`matrix.v1.messages.send_text`), which fails outright at
//! the provider boundary.
//!
//! This module solves it once for every MCP server. After tools are picked
//! from a server, names get rewritten to match the provider pattern; the
//! original tool name stays embedded inside the inner `McpTool`, so calls
//! still resolve to the right MCP method.

use std::collections::HashSet;

use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;

const MAX_NAME_LEN: usize = 128;

/// Replace any character outside `[a-zA-Z0-9_-]` with `_` and truncate to
/// 128 chars (the provider limit).
pub(crate) fn sanitize_name(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.len() > MAX_NAME_LEN {
        out.truncate(MAX_NAME_LEN);
    }
    out
}

/// Wraps a tool whose original name is provider-incompatible. Reports the
/// sanitized name to the model but delegates `call` to the inner tool, which
/// retains the original name for MCP dispatch.
pub(crate) struct SanitizedTool {
    inner: Box<dyn ToolDyn>,
    sanitized_name: String,
}

impl ToolDyn for SanitizedTool {
    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        self.inner.call(args)
    }

    fn definition(&self, prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let inner_fut = self.inner.definition(prompt);
        let name = self.sanitized_name.clone();
        Box::pin(async move {
            let mut def = inner_fut.await;
            def.name = name;
            def
        })
    }

    fn name(&self) -> String {
        self.sanitized_name.clone()
    }
}

/// Rewrite tool names in `tools` so each matches the LLM provider pattern,
/// resolving collisions by suffixing `_2`, `_3`, …
///
/// Tools whose original names already comply are left unwrapped.
pub(crate) fn apply(tools: Vec<Box<dyn ToolDyn>>) -> Vec<Box<dyn ToolDyn>> {
    let mut taken: HashSet<String> = HashSet::new();
    let mut out: Vec<Box<dyn ToolDyn>> = Vec::with_capacity(tools.len());
    for tool in tools {
        let original = tool.name();
        let candidate = sanitize_name(&original);
        let unique = dedupe(&candidate, &mut taken);
        if unique == original {
            out.push(tool);
        } else {
            out.push(Box::new(SanitizedTool {
                inner: tool,
                sanitized_name: unique,
            }));
        }
    }
    out
}

fn dedupe(candidate: &str, taken: &mut HashSet<String>) -> String {
    if taken.insert(candidate.to_string()) {
        return candidate.to_string();
    }
    let mut n = 2u32;
    loop {
        let suffixed = format!("{candidate}_{n}");
        if taken.insert(suffixed.clone()) {
            return suffixed;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rig::completion::ToolDefinition;
    use rig::tool::{ToolDyn, ToolError};
    use rig::wasm_compat::WasmBoxedFuture;
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::{apply, sanitize_name};

    #[test]
    fn dots_become_underscores() {
        assert_eq!(
            sanitize_name("matrix.v1.messages.send_text"),
            "matrix_v1_messages_send_text"
        );
    }

    #[test]
    fn compliant_name_passes_through() {
        assert_eq!(sanitize_name("read_file"), "read_file");
        assert_eq!(sanitize_name("Some-Tool_42"), "Some-Tool_42");
    }

    #[test]
    fn truncates_to_128_chars() {
        let long = "x".repeat(200);
        let out = sanitize_name(&long);
        assert_eq!(out.len(), 128);
    }

    #[test]
    fn unicode_becomes_underscore() {
        assert_eq!(sanitize_name("héllo.wörld"), "h_llo_w_rld");
    }

    struct FakeTool {
        name: String,
        last_called_with: Arc<Mutex<Option<String>>>,
    }

    impl ToolDyn for FakeTool {
        fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
            let store = self.last_called_with.clone();
            Box::pin(async move {
                *store.lock().await = Some(args.clone());
                Ok(args)
            })
        }

        fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
            let def = ToolDefinition {
                description: "fake".to_string(),
                name: self.name.clone(),
                parameters: json!({}),
            };
            Box::pin(async move { def })
        }

        fn name(&self) -> String {
            self.name.clone()
        }
    }

    #[tokio::test]
    async fn wrapper_renames_but_forwards_call() {
        let probe = Arc::new(Mutex::new(None));
        let tool = Box::new(FakeTool {
            name: "matrix.v1.messages.send_text".to_string(),
            last_called_with: probe.clone(),
        });
        let sanitized = apply(vec![tool]);
        assert_eq!(sanitized.len(), 1);
        assert_eq!(sanitized[0].name(), "matrix_v1_messages_send_text");

        let def = sanitized[0].definition(String::new()).await;
        assert_eq!(def.name, "matrix_v1_messages_send_text");

        sanitized[0]
            .call(r#"{"text":"hi"}"#.to_string())
            .await
            .unwrap();
        assert_eq!(
            probe.lock().await.as_deref(),
            Some(r#"{"text":"hi"}"#)
        );
    }

    #[tokio::test]
    async fn collision_resolves_with_numeric_suffix() {
        let probe = Arc::new(Mutex::new(None));
        let a = Box::new(FakeTool {
            name: "matrix.send".to_string(),
            last_called_with: probe.clone(),
        });
        let b = Box::new(FakeTool {
            name: "matrix_send".to_string(),
            last_called_with: probe.clone(),
        });
        let c = Box::new(FakeTool {
            name: "matrix-send".to_string(),
            last_called_with: probe,
        });
        let out = apply(vec![a, b, c]);
        let names: Vec<String> = out.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["matrix_send", "matrix_send_2", "matrix-send"]);
    }
}
