use std::collections::HashMap;
use std::fmt;

use coulisse_core::UserId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::language::{LanguageTag, LanguageTagError};
use crate::response_format::ResponseFormat;
use crate::{Tool, ToolCall, ToolCallId, ToolChoice};

pub(crate) const METADATA_LANGUAGE: &str = "language";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
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
    /// Deprecated by `OpenAI` in favor of `safety_identifier`. Still accepted
    /// for backwards compatibility; `safety_identifier` takes precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

impl ChatCompletionRequest {
    /// True when the client asked for `stream_options.include_usage`. The
    /// `usage` field is then included on the terminal `chat.completion.chunk`
    /// (matching `OpenAI`'s contract); otherwise it's omitted.
    #[must_use]
    pub fn include_usage(&self) -> bool {
        self.stream_options
            .as_ref()
            .and_then(|o| o.include_usage)
            .unwrap_or(false)
    }

    /// True when the client asked for a streamed (SSE) response.
    #[must_use]
    pub fn is_streaming(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    /// Language preference parsed from `metadata["language"]`, if present.
    /// `Ok(None)` means the key is absent — the model falls back to whatever
    /// language the user wrote in. `Err` means the key is present but
    /// malformed.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn language(&self) -> Result<Option<LanguageTag>, LanguageTagError> {
        match self.metadata.get(METADATA_LANGUAGE) {
            None => Ok(None),
            Some(raw) => LanguageTag::parse(raw).map(Some),
        }
    }

    /// The last user message in the request. Only this message is treated
    /// as new input; the rest of the conversation history comes from the
    /// memory store.
    #[must_use]
    pub fn last_user_message(&self) -> Option<&Message> {
        self.messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
    }

    #[must_use]
    pub fn response_with(&self, text: String, usage: Usage) -> ChatCompletionResponse {
        let created = coulisse_core::now_secs();
        let message = Message {
            content: Some(MessageContent::Text(text)),
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
            id: CompletionId::from_created(created),
            model: self.model.clone(),
            object: "chat.completion".into(),
            usage,
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

/// Identifier of one chat completion (`chatcmpl-coulisse-<created>`), shared
/// by the non-streaming response and every chunk of a streamed one.
#[derive(Clone, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CompletionId(String);

impl CompletionId {
    #[must_use]
    pub fn from_created(created: u64) -> Self {
        Self(format!("chatcmpl-coulisse-{created}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CompletionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<Choice>,
    pub created: u64,
    pub id: CompletionId,
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

/// A single part within a multipart message content array. Unknown fields
/// are preserved in `extra` so non-text parts (images, files) survive a
/// round-trip to the upstream provider unchanged.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContentPart {
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
    #[serde(rename = "type")]
    pub kind: ContentPartKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// The `type` of a content part. The named variants are the ones `OpenAI`'s
/// chat API documents; `Other` carries any other spelling verbatim so a part
/// this proxy does not understand still reaches the upstream provider
/// unchanged.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(from = "String", into = "String")]
pub enum ContentPartKind {
    File,
    ImageUrl,
    InputAudio,
    Other(String),
    Text,
}

impl ContentPartKind {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::File => "file",
            Self::ImageUrl => "image_url",
            Self::InputAudio => "input_audio",
            Self::Other(raw) => raw,
            Self::Text => "text",
        }
    }
}

impl From<String> for ContentPartKind {
    fn from(raw: String) -> Self {
        match raw.as_str() {
            "file" => Self::File,
            "image_url" => Self::ImageUrl,
            "input_audio" => Self::InputAudio,
            "text" => Self::Text,
            _ => Self::Other(raw),
        }
    }
}

impl From<ContentPartKind> for String {
    fn from(kind: ContentPartKind) -> Self {
        kind.as_str().to_owned()
    }
}

/// Message content: either a plain string (simple case) or an array of typed
/// parts (multimodal — text, images, files). Mirrors the `OpenAI` API contract.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MessageContent {
    Parts(Vec<ContentPart>),
    Text(String),
}

impl MessageContent {
    /// Returns the concatenated text from all text parts, or the string
    /// itself. Non-text parts (images, files) are silently skipped.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    #[must_use]
    pub fn content_or_empty(&self) -> String {
        self.content
            .as_ref()
            .map_or_else(String::new, MessageContent::text)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
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

/// Token counts as providers report them (`u64`), before clamping into the
/// `u32` fields `OpenAI` clients expect. Named fields so prompt and
/// completion counts cannot be swapped at the call site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub completion: u64,
    pub prompt: u64,
    pub total: u64,
}

impl From<TokenCounts> for Usage {
    fn from(counts: TokenCounts) -> Self {
        Self {
            completion_tokens: clamp_u32(counts.completion),
            prompt_tokens: clamp_u32(counts.prompt),
            total_tokens: clamp_u32(counts.total),
        }
    }
}

fn clamp_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// One frame of an SSE-streamed chat completion. Mirrors `OpenAI`'s
/// `chat.completion.chunk` object: each frame carries a `delta` for one
/// choice. The first frame announces the role, mid-frames carry text, and
/// the terminal frame sets `finish_reason` (and `usage` when requested).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatCompletionChunk {
    pub choices: Vec<ChunkChoice>,
    pub created: u64,
    pub id: CompletionId,
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
            response_format: None,
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
        metadata.insert("language".into(), String::new());
        let req = request_with_metadata(metadata);
        assert!(req.language().is_err());
    }

    #[test]
    fn message_content_deserializes_plain_string() {
        let content: MessageContent = serde_json::from_str(r#""hello""#).unwrap();
        assert!(matches!(content, MessageContent::Text(ref s) if s == "hello"));
        assert_eq!(content.text(), "hello");
    }

    #[test]
    fn message_content_deserializes_parts_array() {
        let content: MessageContent = serde_json::from_str(
            r#"[{"type":"text","text":"first"},{"type":"text","text":"second"}]"#,
        )
        .unwrap();
        assert!(matches!(content, MessageContent::Parts(_)));
        assert_eq!(content.text(), "first\nsecond");
    }

    #[test]
    fn message_content_text_skips_non_text_parts() {
        let content: MessageContent = serde_json::from_str(
            r#"[{"type":"text","text":"caption"},{"type":"input_file","file_id":"file-1"}]"#,
        )
        .unwrap();
        assert_eq!(content.text(), "caption");
    }

    #[test]
    fn content_part_round_trips_unknown_fields() {
        let raw = r#"{"type":"input_file","file_id":"file-1"}"#;
        let part: ContentPart = serde_json::from_str(raw).unwrap();
        assert_eq!(part.kind, ContentPartKind::Other("input_file".into()));
        assert!(part.text.is_none());
        let reserialized = serde_json::to_value(&part).unwrap();
        assert_eq!(reserialized["type"], "input_file");
        assert_eq!(reserialized["file_id"], "file-1");
    }

    #[test]
    fn content_part_kind_round_trips_known_and_unknown_values() {
        for (raw, kind) in [
            ("\"text\"", ContentPartKind::Text),
            ("\"image_url\"", ContentPartKind::ImageUrl),
            ("\"input_audio\"", ContentPartKind::InputAudio),
            ("\"file\"", ContentPartKind::File),
            ("\"refusal\"", ContentPartKind::Other("refusal".into())),
        ] {
            let parsed: ContentPartKind = serde_json::from_str(raw).unwrap();
            assert_eq!(parsed, kind);
            assert_eq!(serde_json::to_string(&parsed).unwrap(), raw);
            assert_eq!(format!("\"{}\"", parsed.as_str()), raw);
        }
    }

    #[test]
    fn tool_message_round_trips_tool_call_id() {
        let raw = r#"{"content":"42","role":"tool","tool_call_id":"call_1"}"#;
        let message: Message = serde_json::from_str(raw).unwrap();
        assert_eq!(
            message.tool_call_id.as_ref().map(ToolCallId::as_str),
            Some("call_1")
        );
        assert_eq!(serde_json::to_string(&message).unwrap(), raw);
    }

    #[test]
    fn usage_clamps_provider_counts() {
        let usage = Usage::from(TokenCounts {
            completion: 2,
            prompt: 1,
            total: u64::from(u32::MAX) + 1,
        });
        assert_eq!(usage.prompt_tokens, 1);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(usage.total_tokens, u32::MAX);
    }

    #[test]
    fn response_serializes_openai_shape() {
        let response = request_with_metadata(HashMap::new()).response_with(
            "hi".into(),
            Usage::from(TokenCounts {
                completion: 1,
                prompt: 1,
                total: 2,
            }),
        );
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["object"], "chat.completion");
        assert_eq!(
            value["id"],
            format!("chatcmpl-coulisse-{}", response.created)
        );
        assert_eq!(value["choices"][0]["message"]["role"], "assistant");
        assert_eq!(value["choices"][0]["message"]["content"], "hi");
        assert_eq!(value["usage"]["total_tokens"], 2);
    }
}
