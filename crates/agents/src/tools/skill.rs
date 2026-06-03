use std::sync::Arc;

use coulisse_core::SkillCatalog;
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde_json::{Value, json};
use tracing::{Instrument, info_span};

/// One tool per skill an agent opts into. The model sees the skill's
/// description in the tool list (cheap to advertise) and only receives the
/// full instruction body when it calls the tool — the progressive-
/// disclosure model that mirrors Claude Code / Codex skills. The body may
/// reference bundled resource files, which the model then fetches through
/// [`SkillFileTool`].
pub(crate) struct SkillTool {
    pub(crate) catalog: Arc<dyn SkillCatalog>,
    pub(crate) description: String,
    pub(crate) name: String,
}

impl ToolDyn for SkillTool {
    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let catalog = Arc::clone(&self.catalog);
        let name = self.name.clone();
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = "skill",
            result = tracing::field::Empty,
            tool_name = %name,
        );
        Box::pin(
            async move {
                if let Some(body) = catalog.body(&name) {
                    tracing::Span::current().record("result", body.as_str());
                    Ok(body)
                } else {
                    let message = format!("skill '{name}' is no longer available");
                    tracing::Span::current().record("error", message.as_str());
                    Err(ToolError::ToolCallError(Box::<
                        dyn std::error::Error + Send + Sync,
                    >::from(
                        message
                    )))
                }
            }
            .instrument(span),
        )
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let description = self.description.clone();
        let name = self.name.clone();
        Box::pin(async move {
            ToolDefinition {
                name,
                description,
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                }),
            }
        })
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

/// Companion to [`SkillTool`]: reads a bundled resource file referenced by a
/// skill's instructions. Sandboxed by the catalog — only files that live
/// under the skill's own directory are reachable.
pub(crate) struct SkillFileTool {
    pub(crate) catalog: Arc<dyn SkillCatalog>,
}

impl ToolDyn for SkillFileTool {
    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let catalog = Arc::clone(&self.catalog);
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = "skill",
            result = tracing::field::Empty,
            tool_name = "skill_file",
        );
        Box::pin(
            async move {
                let parsed: Value = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
                let skill = required_str(&parsed, "skill")?;
                let path = required_str(&parsed, "path")?;
                match catalog.read_file(&skill, &path) {
                    Ok(contents) => {
                        tracing::Span::current().record("result", contents.as_str());
                        Ok(contents)
                    }
                    Err(err) => {
                        tracing::Span::current().record("error", err.to_string().as_str());
                        Err(ToolError::ToolCallError(Box::new(err)))
                    }
                }
            }
            .instrument(span),
        )
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: "skill_file".to_string(),
                description: "Read a bundled resource file from a skill's directory — use this \
                              when a skill's instructions point you at one of its files (a \
                              template, reference doc, or checklist). `skill` is the skill name; \
                              `path` is relative to that skill's directory."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path relative to the skill's directory, e.g. \"refs/style.md\".",
                        },
                        "skill": {
                            "type": "string",
                            "description": "Name of the skill that owns the file.",
                        }
                    },
                    "required": ["path", "skill"],
                }),
            }
        })
    }

    fn name(&self) -> String {
        "skill_file".to_string()
    }
}

fn required_str(value: &Value, field: &str) -> Result<String, ToolError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            ToolError::ToolCallError(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                "skill_file tool call is missing required '{field}' field"
            )))
        })
}
