//! OpenAI-compatible HTTP wire schema. Pure leaf crate: serializable
//! request/response/stream types, plus tool schema. The chat handler that
//! consumes these lives in `cli` — there is no orchestration here.

mod chat;
mod tool;

pub use chat::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice, ChunkChoice,
    ChunkDelta, FinishReason, Message, Role, StreamOptions, Usage, now_secs, response_id,
};
pub use tool::{
    Tool, ToolCall, ToolCallFunction, ToolChoice, ToolChoiceFunction, ToolChoiceMode, ToolFunction,
};
