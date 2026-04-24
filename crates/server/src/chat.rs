use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use limits::{LimitError, RequestLimits};
use memory::UserId;
use serde::{Deserialize, Serialize};

use crate::{Tool, ToolCall, ToolChoice};

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

    pub fn response_with(&self, text: String, usage: prompter::Usage) -> ChatCompletionResponse {
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
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
            id: format!("chatcmpl-coulisse-{created}"),
            model: self.model.clone(),
            object: "chat.completion".into(),
            usage: Usage {
                completion_tokens: clamp_u32(usage.output_tokens),
                prompt_tokens: clamp_u32(usage.input_tokens),
                total_tokens: clamp_u32(usage.total_tokens),
            },
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
