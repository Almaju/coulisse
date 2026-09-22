use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Tool {
    pub function: ToolFunction,
    #[serde(rename = "type")]
    pub kind: ToolKind,
}

/// The `type` discriminator on tools, tool calls, and a specific
/// `tool_choice`. `OpenAI` defines a single value, `function`; anything
/// else is rejected at deserialization instead of flowing through as text.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum ToolKind {
    #[serde(rename = "function")]
    Function,
}

impl ToolKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
        }
    }
}

/// Identifier the model assigns to a tool call (`call_…`), echoed back on
/// the `tool` message that carries its result.
#[derive(Clone, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolFunction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub function: ToolCallFunction,
    pub id: ToolCallId,
    #[serde(rename = "type")]
    pub kind: ToolKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCallFunction {
    pub arguments: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Mode(ToolChoiceMode),
    Specific {
        function: ToolChoiceFunction,
        #[serde(rename = "type")]
        kind: ToolKind,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    Auto,
    None,
    Required,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_round_trips_openai_shape() {
        let raw =
            r#"{"function":{"name":"lookup","parameters":{"type":"object"}},"type":"function"}"#;
        let tool: Tool = serde_json::from_str(raw).unwrap();
        assert_eq!(tool.kind, ToolKind::Function);
        assert_eq!(serde_json::to_string(&tool).unwrap(), raw);
    }

    #[test]
    fn tool_call_round_trips_openai_shape() {
        let raw =
            r#"{"function":{"arguments":"{}","name":"lookup"},"id":"call_1","type":"function"}"#;
        let call: ToolCall = serde_json::from_str(raw).unwrap();
        assert_eq!(call.id.as_str(), "call_1");
        assert_eq!(serde_json::to_string(&call).unwrap(), raw);
    }

    #[test]
    fn tool_choice_round_trips_both_forms() {
        let mode: ToolChoice = serde_json::from_str(r#""auto""#).unwrap();
        assert!(matches!(mode, ToolChoice::Mode(ToolChoiceMode::Auto)));
        assert_eq!(serde_json::to_string(&mode).unwrap(), r#""auto""#);
        let raw = r#"{"function":{"name":"lookup"},"type":"function"}"#;
        let specific: ToolChoice = serde_json::from_str(raw).unwrap();
        assert!(matches!(specific, ToolChoice::Specific { .. }));
        assert_eq!(serde_json::to_string(&specific).unwrap(), raw);
    }

    #[test]
    fn unknown_tool_type_is_rejected() {
        let raw = r#"{"function":{"name":"lookup"},"type":"retrieval"}"#;
        assert!(serde_json::from_str::<Tool>(raw).is_err());
    }
}
