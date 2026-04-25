use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use agents::{LanguageTag, LanguageTagError};
use coulisse_core::UserId;
use limits::{LimitError, RequestLimits};
use serde::{Deserialize, Serialize};

use crate::{Tool, ToolCall, ToolChoice};

pub const METADATA_LANGUAGE: &str = "language";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Deprecated by OpenAI in favor of `safety_identifier`. Still accepted
    /// for backwards compatibility; `safety_identifier` takes precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

impl ChatCompletionRequest {
    /// True when the client asked for `stream_options.include_usage`. The
    /// `usage` field is then included on the terminal `chat.completion.chunk`
    /// (matching OpenAI's contract); otherwise it's omitted.
    pub fn include_usage(&self) -> bool {
        self.stream_options
            .as_ref()
            .and_then(|o| o.include_usage)
            .unwrap_or(false)
    }

    /// True when the client asked for a streamed (SSE) response.
    pub fn is_streaming(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    /// Language preference parsed from `metadata["language"]`, if present.
    /// `Ok(None)` means the key is absent — the model falls back to whatever
    /// language the user wrote in. `Err` means the key is present but
    /// malformed.
    pub fn language(&self) -> Result<Option<LanguageTag>, LanguageTagError> {
        match self.metadata.get(METADATA_LANGUAGE) {
            Some(raw) => LanguageTag::parse(raw).map(Some),
            None => Ok(None),
        }
    }

    /// The last user message in the request. Only this message is treated
    /// as new input; the rest of the conversation history comes from the
    /// memory store.
    pub fn last_user_message(&self) -> Option<&Message> {
        self.messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
    }

    /// Per-user token limits parsed from `metadata`. Empty when no limit keys
    /// are present.
    pub fn request_limits(&self) -> Result<RequestLimits, LimitError> {
        RequestLimits::from_metadata(&self.metadata)
    }

    pub fn response_with(&self, text: String, usage: agents::Usage) -> ChatCompletionResponse {
        let created = now_secs();
        let message = Message {
            content: Some(text),
            name: None,
            role: Role::Assistant,
            tool_call_id: None,
            tool_calls: None,
        };
        ChatCompletionResponse {
            choices: vec![Choice {
                finish_reason: FinishReason::Stop,
                index: 0,
                message,
            }],
            created,
            id: response_id(created),
            model: self.model.clone(),
            object: "chat.completion".into(),
            usage: Usage::from_agents(usage),
        }
    }

    /// System messages supplied in the request, in order. Preserved verbatim
    /// and forwarded to the model alongside memory-backed history.
    pub fn system_messages(&self) -> impl Iterator<Item = &Message> {
        self.messages
            .iter()
            .filter(|m| matches!(m.role, Role::System))
    }

    /// `UserId` derived from `safety_identifier`, falling back to the
    /// deprecated `user` field. Stable: the same string always maps to the
    /// same id, whether it's a UUID or arbitrary opaque. Whitespace-only
    /// values are treated as missing to avoid blank-string identifiers
    /// collapsing distinct users onto a single memory bucket.
    pub fn user_id(&self) -> Option<UserId> {
        self.user_key().map(UserId::from_string)
    }

    /// Trimmed caller-supplied user string, if any. Serves as the key for
    /// rate-limit bookkeeping so the same caller always maps to the same
    /// bucket regardless of how `UserId` encodes it internally.
    pub fn user_key(&self) -> Option<&str> {
        self.safety_identifier
            .as_deref()
            .or(self.user.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

fn clamp_u32(n: u64) -> u32 {
    n.min(u32::MAX as u64) as u32
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn response_id(created: u64) -> String {
    format!("chatcmpl-coulisse-{created}")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<Choice>,
    pub created: u64,
    pub id: String,
    pub model: String,
    pub object: String,
    pub usage: Usage,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Choice {
    pub finish_reason: FinishReason,
    pub index: u32,
    pub message: Message,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    ContentFilter,
    Length,
    Stop,
    ToolCalls,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    pub fn content_or_empty(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Assistant,
    System,
    Tool,
    User,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Usage {
    pub completion_tokens: u32,
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

impl Usage {
    pub fn from_agents(usage: agents::Usage) -> Self {
        Self {
            completion_tokens: clamp_u32(usage.output_tokens),
            prompt_tokens: clamp_u32(usage.input_tokens),
            total_tokens: clamp_u32(usage.total_tokens),
        }
    }
}

/// One frame of an SSE-streamed chat completion. Mirrors OpenAI's
/// `chat.completion.chunk` object: each frame carries a `delta` for one
/// choice. The first frame announces the role, mid-frames carry text, and
/// the terminal frame sets `finish_reason` (and `usage` when requested).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatCompletionChunk {
    pub choices: Vec<ChunkChoice>,
    pub created: u64,
    pub id: String,
    pub model: String,
    pub object: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChunkChoice {
    pub delta: ChunkDelta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    pub index: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ChunkDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StreamOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_metadata(metadata: HashMap<String, String>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            max_tokens: None,
            messages: vec![],
            metadata,
            model: "test".into(),
            safety_identifier: None,
            stream: None,
            stream_options: None,
            temperature: None,
            tool_choice: None,
            tools: None,
            user: None,
        }
    }

    #[test]
    fn language_is_none_when_metadata_key_is_absent() {
        let req = request_with_metadata(HashMap::new());
        assert!(req.language().unwrap().is_none());
    }

    #[test]
    fn language_parses_valid_tag_from_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert("language".into(), "fr-FR".into());
        let req = request_with_metadata(metadata);
        let tag = req.language().unwrap().expect("language present");
        assert_eq!(tag.as_str(), "fr-FR");
        assert_eq!(
            tag.instruction(),
            "Always reply in French, even when the user writes in a different language. Do not include translations in any other language."
        );
    }

    #[test]
    fn language_rejects_malformed_tag() {
        let mut metadata = HashMap::new();
        metadata.insert("language".into(), "not a tag!".into());
        let req = request_with_metadata(metadata);
        assert!(req.language().is_err());
    }

    #[test]
    fn language_rejects_empty_value() {
        let mut metadata = HashMap::new();
        metadata.insert("language".into(), "".into());
        let req = request_with_metadata(metadata);
        assert!(req.language().is_err());
    }
}
