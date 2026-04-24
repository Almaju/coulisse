//! HTTP client for the admin JSON API. Mirrors the wire types defined in
//! `crates/server/src/admin.rs` — keep the two in sync when either side
//! changes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const BASE: &str = "/admin/api";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserView {
    pub last_activity_at: u64,
    pub memory_count: u32,
    pub message_count: u32,
    pub score_count: u32,
    pub tool_call_count: u32,
    pub user_id: Uuid,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Assistant,
    System,
    User,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::User => "user",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MessageView {
    pub content: String,
    pub created_at: u64,
    pub id: String,
    pub role: Role,
    pub token_count: u32,
    /// Tool invocations that fired during this assistant turn, in fire
    /// order. Always empty for user and system messages.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallView>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallKind {
    Mcp,
    Subagent,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCallView {
    pub args: String,
    pub created_at: u64,
    pub error: Option<String>,
    pub id: String,
    pub kind: ToolCallKind,
    pub message_id: String,
    pub ordinal: u32,
    pub result: Option<String>,
    pub tool_name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    Fact,
    Preference,
}

impl MemoryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Preference => "preference",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryView {
    pub content: String,
    pub created_at: u64,
    pub id: Uuid,
    pub kind: MemoryKind,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsersResponse {
    pub users: Vec<UserView>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MessagesResponse {
    pub messages: Vec<MessageView>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MemoriesResponse {
    pub memories: Vec<MemoryView>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScoreView {
    pub created_at: u64,
    pub criterion: String,
    pub id: String,
    pub judge_model: String,
    pub judge_name: String,
    pub message_id: String,
    pub reasoning: String,
    pub score: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CriterionAverage {
    pub average: f32,
    pub count: u32,
    pub criterion: String,
    pub judge_name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScoresResponse {
    pub averages: Vec<CriterionAverage>,
    pub scores: Vec<ScoreView>,
}

#[derive(Clone, Debug)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<gloo_net::Error> for ApiError {
    fn from(e: gloo_net::Error) -> Self {
        Self(e.to_string())
    }
}

pub async fn list_users() -> Result<Vec<UserView>, ApiError> {
    let resp: UsersResponse = gloo_net::http::Request::get(&format!("{BASE}/users"))
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.users)
}

pub async fn user_messages(user_id: Uuid) -> Result<Vec<MessageView>, ApiError> {
    let url = format!("{BASE}/users/{user_id}/messages");
    let resp: MessagesResponse = gloo_net::http::Request::get(&url)
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.messages)
}

pub async fn user_memories(user_id: Uuid) -> Result<Vec<MemoryView>, ApiError> {
    let url = format!("{BASE}/users/{user_id}/memories");
    let resp: MemoriesResponse = gloo_net::http::Request::get(&url)
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.memories)
}

pub async fn user_scores(user_id: Uuid) -> Result<ScoresResponse, ApiError> {
    let url = format!("{BASE}/users/{user_id}/scores");
    let resp: ScoresResponse = gloo_net::http::Request::get(&url)
        .send()
        .await?
        .json()
        .await?;
    Ok(resp)
}
