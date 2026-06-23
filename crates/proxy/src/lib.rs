//! OpenAI-compatible HTTP wire schema. Pure leaf crate: serializable
//! request/response/stream types, plus tool schema. The chat handler that
//! consumes these lives in `cli` — there is no orchestration here.

mod chat;
mod language;
mod response_format;
mod tool;

pub use chat::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice, ChunkChoice,
    ChunkDelta, ContentPart, FinishReason, Message, MessageContent, Role, StreamOptions, Usage,
    response_id,
};
pub use language::{LanguageTag, LanguageTagError};
pub use response_format::{JsonSchemaSpec, ResponseFormat, ResponseFormatError};
pub use tool::{
    Tool, ToolCall, ToolCallFunction, ToolChoice, ToolChoiceFunction, ToolChoiceMode, ToolFunction,
};
