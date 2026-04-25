use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{SqlitePool, sqlite::SqliteRow};
use uuid::Uuid;

use crate::{
    BackendConfig, BundledEmbedder, ConfigError, Memory, MemoryConfig, MemoryError, MemoryId,
    MemoryKind, Message, MessageId, Role, Score, ScoreId, StoredMessage, StoredToolCall,
    TokenCount, ToolCallId, ToolCallKind, UserId,
};

const SCHEMA_SQL: &str = include_str!("../migrations/schema.sql");
const MIGRATE_SQL: &str = include_str!("../migrations/migrate.sql");

/// Top-level memory infrastructure. Owns the embedder and the SQLite pool
/// where all per-user data lives.
///
/// Callers can never touch user data except through `Store::for_user`, which
/// returns a `UserMemory` handle scoped to a single `UserId`. That handle
/// cannot observe or mutate any other user's data — isolation is a
/// structural property of the API, enforced by every SQL query.
pub struct Store {
    config: MemoryConfig,
    embedder: BundledEmbedder,
    pool: SqlitePool,
}

impl Store {
    /// Open a Store using the given config. For `Sqlite` backends, creates
    /// the database file (and parent directory) if missing, applies the
    /// schema, and runs the forward-migration step. For `InMemory`, opens
    /// an ephemeral in-process database that evaporates when the pool drops.
    ///
    /// `fallback_api_key` is tried when the embedder config does not carry
    /// its own key — caller passes the matching entry from `providers:`.
    pub async fn open(
        config: MemoryConfig,
        fallback_api_key: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let embedder = BundledEmbedder::from_config(&config.embedder, fallback_api_key)?;
        let pool = open_pool(&config.backend).await?;
        apply_schema(&pool).await?;
        Ok(Self {
            config,
            embedder,
            pool,
        })
    }

    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    pub fn embedder(&self) -> &BundledEmbedder {
        &self.embedder
    }

    /// Shared SQLite pool. Exposed so sibling crates (e.g. `limits`) can
    /// apply their own schema and write into the same database without each
    /// opening its own connection to the same file.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Obtain a scoped handle for `user_id`. Does not create any rows until
    /// the caller writes something.
    pub fn for_user(&self, user_id: UserId) -> UserMemory<'_> {
        UserMemory {
            store: self,
            user_id,
        }
    }

    /// Summaries of every user the store has seen, ordered by most recent
    /// activity first. Intended for read-only studio views.
    pub async fn list_user_summaries(&self) -> Result<Vec<UserSummary>, MemoryError> {
        let rows = sqlx::query(
            "SELECT u.user_id AS user_id, \
                    COALESCE((SELECT COUNT(*) FROM messages m WHERE m.user_id = u.user_id), 0) AS message_count, \
                    COALESCE((SELECT COUNT(*) FROM memories mm WHERE mm.user_id = u.user_id), 0) AS memory_count, \
                    COALESCE((SELECT COUNT(*) FROM scores s WHERE s.user_id = u.user_id), 0) AS score_count, \
                    COALESCE((SELECT COUNT(*) FROM tool_calls tc WHERE tc.user_id = u.user_id), 0) AS tool_call_count, \
                    COALESCE(( \
                        SELECT MAX(created_at) FROM ( \
                            SELECT created_at FROM messages WHERE user_id = u.user_id \
                            UNION ALL \
                            SELECT created_at FROM memories WHERE user_id = u.user_id \
                            UNION ALL \
                            SELECT created_at FROM scores WHERE user_id = u.user_id \
                            UNION ALL \
                            SELECT created_at FROM tool_calls WHERE user_id = u.user_id \
                        ) \
                    ), 0) AS last_activity_at \
             FROM ( \
                 SELECT DISTINCT user_id FROM messages \
                 UNION \
                 SELECT DISTINCT user_id FROM memories \
                 UNION \
                 SELECT DISTINCT user_id FROM scores \
                 UNION \
                 SELECT DISTINCT user_id FROM tool_calls \
             ) u \
             ORDER BY last_activity_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let user_id: String = row.try_get("user_id")?;
            let message_count: i64 = row.try_get("message_count")?;
            let memory_count: i64 = row.try_get("memory_count")?;
            let score_count: i64 = row.try_get("score_count")?;
            let tool_call_count: i64 = row.try_get("tool_call_count")?;
            let last_activity_at: i64 = row.try_get("last_activity_at")?;
            out.push(UserSummary {
                last_activity_at: last_activity_at as u64,
                memory_count: clamp_u32(memory_count),
                message_count: clamp_u32(message_count),
                score_count: clamp_u32(score_count),
                tool_call_count: clamp_u32(tool_call_count),
                user_id: UserId(parse_uuid(&user_id, "user id")?),
            });
        }
        Ok(out)
    }
}

/// Scoped handle to one user's memory. All reads and writes are bound to
/// `self.user_id`; there is no API that accepts a different user id.
pub struct UserMemory<'a> {
    store: &'a Store,
    user_id: UserId,
}

impl<'a> UserMemory<'a> {
    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    /// Append a message to the user's conversation history.
    pub async fn append_message(
        &self,
        role: Role,
        content: String,
    ) -> Result<MessageId, MemoryError> {
        self.append_message_with_id(role, content, MessageId::new())
            .await
    }

    /// Same as [`append_message`], but lets the caller supply the row's id
    /// up front. Used by the chat handler so the assistant message's id can
    /// double as the telemetry turn correlation id — one value identifies
    /// both the stored message and the event tree that produced it.
    pub async fn append_message_with_id(
        &self,
        role: Role,
        content: String,
        id: MessageId,
    ) -> Result<MessageId, MemoryError> {
        let stored = StoredMessage::new_with_id(self.user_id, role, content, id);
        sqlx::query(
            "INSERT INTO messages (content, created_at, id, role, token_count, user_id) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&stored.content)
        .bind(stored.created_at as i64)
        .bind(stored.id.0.to_string())
        .bind(role_as_str(stored.role))
        .bind(stored.token_count.0 as i64)
        .bind(self.user_id.0.to_string())
        .execute(&self.store.pool)
        .await?;
        Ok(stored.id)
    }

    /// Record a long-term memory (fact or preference) for this user. Always
    /// inserts — use `remember_if_novel` to skip near-duplicates.
    pub async fn remember(
        &self,
        kind: MemoryKind,
        content: String,
    ) -> Result<MemoryId, MemoryError> {
        let embedding = self.store.embedder.embed(&content).await?;
        check_dims(&embedding, self.store.embedder.ndims())?;
        let memory = Memory::new(self.user_id, kind, content, embedding);
        self.insert_memory(&memory).await?;
        Ok(memory.id)
    }

    /// Record a memory only if no existing memory for this user has a
    /// cosine similarity above `threshold`. Returns `Ok(None)` when a
    /// near-duplicate is already stored. Used by the auto-extractor to
    /// avoid writing the same fact on every turn.
    pub async fn remember_if_novel(
        &self,
        kind: MemoryKind,
        content: String,
        threshold: f32,
    ) -> Result<Option<MemoryId>, MemoryError> {
        let embedding = self.store.embedder.embed(&content).await?;
        check_dims(&embedding, self.store.embedder.ndims())?;
        let existing = self.load_memories().await?;
        for m in &existing {
            if cosine_similarity(&embedding, &m.embedding) >= threshold {
                return Ok(None);
            }
        }
        let memory = Memory::new(self.user_id, kind, content, embedding);
        self.insert_memory(&memory).await?;
        Ok(Some(memory.id))
    }

    /// Return top-`k` memories most relevant to `query` by cosine similarity.
    /// Only memories embedded with the currently-configured embedder model
    /// are considered — stale rows are ignored.
    pub async fn recall(&self, query: &str, k: usize) -> Result<Vec<Memory>, MemoryError> {
        let query_embedding = self.store.embedder.embed(query).await?;
        check_dims(&query_embedding, self.store.embedder.ndims())?;
        let memories = self.load_memories().await?;
        let mut scored: Vec<(f32, Memory)> = memories
            .into_iter()
            .map(|m| (cosine_similarity(&query_embedding, &m.embedding), m))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(k).map(|(_, m)| m).collect())
    }

    pub async fn message_count(&self) -> Result<usize, MemoryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages WHERE user_id = ?")
            .bind(self.user_id.0.to_string())
            .fetch_one(&self.store.pool)
            .await?;
        Ok(row.0 as usize)
    }

    pub async fn memory_count(&self) -> Result<usize, MemoryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memories WHERE user_id = ?")
            .bind(self.user_id.0.to_string())
            .fetch_one(&self.store.pool)
            .await?;
        Ok(row.0 as usize)
    }

    /// Full conversation history for this user, in chronological order.
    pub async fn messages(&self) -> Result<Vec<StoredMessage>, MemoryError> {
        let rows = sqlx::query(
            "SELECT content, created_at, id, role, token_count, user_id \
             FROM messages WHERE user_id = ? ORDER BY rowid ASC",
        )
        .bind(self.user_id.0.to_string())
        .fetch_all(&self.store.pool)
        .await?;
        rows.into_iter().map(row_to_stored_message).collect()
    }

    /// All long-term memories recorded for this user, in insertion order.
    pub async fn memories(&self) -> Result<Vec<Memory>, MemoryError> {
        self.load_memories().await
    }

    /// Persist one LLM-judge score row. Called after a scored turn, typically
    /// from a background task spawned off the response path so the client is
    /// never blocked on the judge call.
    pub async fn append_score(&self, score: Score) -> Result<ScoreId, MemoryError> {
        sqlx::query(
            "INSERT INTO scores (created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(score.created_at as i64)
        .bind(&score.criterion)
        .bind(score.id.0.to_string())
        .bind(&score.judge_model)
        .bind(&score.judge_name)
        .bind(score.message_id.0.to_string())
        .bind(&score.reasoning)
        .bind(score.score)
        .bind(self.user_id.0.to_string())
        .execute(&self.store.pool)
        .await?;
        Ok(score.id)
    }

    /// All judge scores recorded for this user, chronological.
    pub async fn scores(&self) -> Result<Vec<Score>, MemoryError> {
        let rows = sqlx::query(
            "SELECT created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id \
             FROM scores WHERE user_id = ? ORDER BY rowid ASC",
        )
        .bind(self.user_id.0.to_string())
        .fetch_all(&self.store.pool)
        .await?;
        rows.into_iter().map(row_to_score).collect()
    }

    pub async fn score_count(&self) -> Result<usize, MemoryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM scores WHERE user_id = ?")
            .bind(self.user_id.0.to_string())
            .fetch_one(&self.store.pool)
            .await?;
        Ok(row.0 as usize)
    }

    /// Record one tool invocation attached to an assistant message. Called
    /// once per tool call from the streaming path — callers know the final
    /// `message_id` by the time the stream completes, and the `ordinal`
    /// reflects the order rig fired the tools within the turn.
    pub async fn append_tool_call(
        &self,
        tool_call: StoredToolCall,
    ) -> Result<ToolCallId, MemoryError> {
        sqlx::query(
            "INSERT INTO tool_calls (args, created_at, error, id, kind, \
             message_id, ordinal, result, tool_name, user_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&tool_call.args)
        .bind(tool_call.created_at as i64)
        .bind(tool_call.error.as_deref())
        .bind(tool_call.id.0.to_string())
        .bind(tool_call_kind_as_str(tool_call.kind))
        .bind(tool_call.message_id.0.to_string())
        .bind(tool_call.ordinal as i64)
        .bind(tool_call.result.as_deref())
        .bind(&tool_call.tool_name)
        .bind(self.user_id.0.to_string())
        .execute(&self.store.pool)
        .await?;
        Ok(tool_call.id)
    }

    /// Every tool call this user has ever triggered, oldest first.
    pub async fn tool_calls(&self) -> Result<Vec<StoredToolCall>, MemoryError> {
        let rows = sqlx::query(
            "SELECT args, created_at, error, id, kind, message_id, ordinal, \
             result, tool_name, user_id \
             FROM tool_calls WHERE user_id = ? ORDER BY rowid ASC",
        )
        .bind(self.user_id.0.to_string())
        .fetch_all(&self.store.pool)
        .await?;
        rows.into_iter().map(row_to_tool_call).collect()
    }

    pub async fn tool_call_count(&self) -> Result<usize, MemoryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tool_calls WHERE user_id = ?")
            .bind(self.user_id.0.to_string())
            .fetch_one(&self.store.pool)
            .await?;
        Ok(row.0 as usize)
    }

    /// Assemble a context window for an upcoming prompt. Takes the new user
    /// message (used for semantic recall) and a total token budget. Returns
    /// recalled memories and the most-recent conversation messages that fit
    /// within the budget, in chronological order. The new message is *not*
    /// included — the caller appends it.
    pub async fn assemble_context(
        &self,
        new_user_message: &str,
        budget: TokenCount,
    ) -> Result<AssembledContext, MemoryError> {
        let recalled = self
            .recall(new_user_message, self.store.config.recall_k)
            .await?;

        let memory_budget =
            TokenCount(((budget.0 as f32) * self.store.config.memory_budget_fraction) as u32);
        let memories = fit_memories(recalled, memory_budget);
        let memories_used: TokenCount = memories
            .iter()
            .map(|m| TokenCount::estimate(&m.content))
            .fold(TokenCount(0), |a, b| a + b);
        let history_budget = budget.saturating_sub(memories_used);

        let stored = self.messages().await?;
        let messages = fit_messages(&stored, history_budget);

        Ok(AssembledContext { memories, messages })
    }

    async fn insert_memory(&self, memory: &Memory) -> Result<(), MemoryError> {
        sqlx::query(
            "INSERT INTO memories (content, created_at, embedding, embedding_dims, \
             embedding_model, id, kind, user_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&memory.content)
        .bind(memory.created_at as i64)
        .bind(vec_to_bytes(&memory.embedding))
        .bind(memory.embedding.len() as i64)
        .bind(self.store.config.embedder.model_id())
        .bind(memory.id.0.to_string())
        .bind(kind_as_str(memory.kind))
        .bind(self.user_id.0.to_string())
        .execute(&self.store.pool)
        .await?;
        Ok(())
    }

    async fn load_memories(&self) -> Result<Vec<Memory>, MemoryError> {
        let model_id = self.store.config.embedder.model_id();
        let rows = sqlx::query(
            "SELECT content, created_at, embedding, id, kind, user_id \
             FROM memories WHERE user_id = ? AND embedding_model = ? \
             ORDER BY rowid ASC",
        )
        .bind(self.user_id.0.to_string())
        .bind(&model_id)
        .fetch_all(&self.store.pool)
        .await?;
        rows.into_iter().map(row_to_memory).collect()
    }
}

/// Context ready to forward to an LLM. `memories` should typically be rendered
/// as a system/preamble block; `messages` are the recent conversation verbatim.
#[derive(Clone, Debug)]
pub struct AssembledContext {
    pub memories: Vec<Memory>,
    pub messages: Vec<Message>,
}

/// Aggregate view of a single user's stored data. Returned by
/// `Store::list_user_summaries` for studio-style overviews.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct UserSummary {
    pub last_activity_at: u64,
    pub memory_count: u32,
    pub message_count: u32,
    pub score_count: u32,
    pub tool_call_count: u32,
    pub user_id: UserId,
}

async fn open_pool(backend: &BackendConfig) -> Result<SqlitePool, ConfigError> {
    let options = match backend {
        BackendConfig::InMemory => SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(ConfigError::from)?
            .create_if_missing(true),
        BackendConfig::Sqlite { path } => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                ensure_dir(parent)?;
            }
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .foreign_keys(true)
        }
    };
    let max_connections = if matches!(backend, BackendConfig::InMemory) {
        1
    } else {
        5
    };
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;
    Ok(pool)
}

fn ensure_dir(path: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(path).map_err(|source| ConfigError::CreateDir {
        path: path.to_path_buf(),
        source,
    })
}

async fn apply_schema(pool: &SqlitePool) -> Result<(), ConfigError> {
    for stmt in split_sql(SCHEMA_SQL)
        .into_iter()
        .chain(split_sql(MIGRATE_SQL))
    {
        sqlx::query(&stmt).execute(pool).await?;
    }
    Ok(())
}

fn split_sql(sql: &str) -> Vec<String> {
    let stripped: String = sql
        .lines()
        .map(|line| match line.find("--") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    stripped
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn check_dims(vector: &[f32], expected: usize) -> Result<(), MemoryError> {
    if vector.len() != expected {
        return Err(MemoryError::DimensionMismatch {
            expected,
            got: vector.len(),
        });
    }
    Ok(())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

fn fit_memories(recalled: Vec<Memory>, budget: TokenCount) -> Vec<Memory> {
    let mut used = TokenCount(0);
    let mut out = Vec::new();
    for m in recalled {
        let cost = TokenCount::estimate(&m.content);
        if used + cost > budget {
            break;
        }
        used += cost;
        out.push(m);
    }
    out
}

fn fit_messages(messages: &[StoredMessage], budget: TokenCount) -> Vec<Message> {
    let mut used = TokenCount(0);
    let mut taken: Vec<&StoredMessage> = Vec::new();
    for m in messages.iter().rev() {
        if used + m.token_count > budget {
            break;
        }
        used += m.token_count;
        taken.push(m);
    }
    taken.reverse();
    taken.iter().map(|m| m.as_message()).collect()
}

fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

fn bytes_to_vec(bytes: &[u8]) -> Result<Vec<f32>, MemoryError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(MemoryError::RowDecode(format!(
            "embedding blob length {} is not a multiple of 4",
            bytes.len()
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn row_to_memory(row: SqliteRow) -> Result<Memory, MemoryError> {
    let content: String = row.try_get("content")?;
    let created_at: i64 = row.try_get("created_at")?;
    let embedding_blob: Vec<u8> = row.try_get("embedding")?;
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let user_id: String = row.try_get("user_id")?;
    Ok(Memory {
        content,
        created_at: created_at as u64,
        embedding: bytes_to_vec(&embedding_blob)?,
        id: MemoryId(parse_uuid(&id, "memory id")?),
        kind: parse_kind(&kind)?,
        user_id: UserId(parse_uuid(&user_id, "user id")?),
    })
}

fn row_to_score(row: SqliteRow) -> Result<Score, MemoryError> {
    let created_at: i64 = row.try_get("created_at")?;
    let criterion: String = row.try_get("criterion")?;
    let id: String = row.try_get("id")?;
    let judge_model: String = row.try_get("judge_model")?;
    let judge_name: String = row.try_get("judge_name")?;
    let message_id: String = row.try_get("message_id")?;
    let reasoning: String = row.try_get("reasoning")?;
    let score: f32 = row.try_get("score")?;
    let user_id: String = row.try_get("user_id")?;
    Ok(Score {
        created_at: created_at as u64,
        criterion,
        id: ScoreId(parse_uuid(&id, "score id")?),
        judge_model,
        judge_name,
        message_id: MessageId(parse_uuid(&message_id, "message id")?),
        reasoning,
        score,
        user_id: UserId(parse_uuid(&user_id, "user id")?),
    })
}

fn row_to_stored_message(row: SqliteRow) -> Result<StoredMessage, MemoryError> {
    let content: String = row.try_get("content")?;
    let created_at: i64 = row.try_get("created_at")?;
    let id: String = row.try_get("id")?;
    let role: String = row.try_get("role")?;
    let token_count: i64 = row.try_get("token_count")?;
    let user_id: String = row.try_get("user_id")?;
    Ok(StoredMessage {
        content,
        created_at: created_at as u64,
        id: MessageId(parse_uuid(&id, "message id")?),
        role: parse_role(&role)?,
        token_count: TokenCount(token_count as u32),
        user_id: UserId(parse_uuid(&user_id, "user id")?),
    })
}

fn parse_uuid(s: &str, label: &str) -> Result<Uuid, MemoryError> {
    Uuid::parse_str(s).map_err(|e| MemoryError::RowDecode(format!("invalid {label}: {e}")))
}

fn parse_kind(s: &str) -> Result<MemoryKind, MemoryError> {
    match s {
        "fact" => Ok(MemoryKind::Fact),
        "preference" => Ok(MemoryKind::Preference),
        other => Err(MemoryError::RowDecode(format!(
            "unknown memory kind '{other}'"
        ))),
    }
}

fn parse_role(s: &str) -> Result<Role, MemoryError> {
    match s {
        "assistant" => Ok(Role::Assistant),
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        other => Err(MemoryError::RowDecode(format!("unknown role '{other}'"))),
    }
}

fn kind_as_str(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Fact => "fact",
        MemoryKind::Preference => "preference",
    }
}

fn role_as_str(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::User => "user",
    }
}

fn row_to_tool_call(row: SqliteRow) -> Result<StoredToolCall, MemoryError> {
    let args: String = row.try_get("args")?;
    let created_at: i64 = row.try_get("created_at")?;
    let error: Option<String> = row.try_get("error")?;
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let message_id: String = row.try_get("message_id")?;
    let ordinal: i64 = row.try_get("ordinal")?;
    let result: Option<String> = row.try_get("result")?;
    let tool_name: String = row.try_get("tool_name")?;
    let user_id: String = row.try_get("user_id")?;
    Ok(StoredToolCall {
        args,
        created_at: created_at as u64,
        error,
        id: ToolCallId(parse_uuid(&id, "tool_call id")?),
        kind: parse_tool_call_kind(&kind)?,
        message_id: MessageId(parse_uuid(&message_id, "message id")?),
        ordinal: clamp_u32(ordinal),
        result,
        tool_name,
        user_id: UserId(parse_uuid(&user_id, "user id")?),
    })
}

fn parse_tool_call_kind(s: &str) -> Result<ToolCallKind, MemoryError> {
    match s {
        "mcp" => Ok(ToolCallKind::Mcp),
        "subagent" => Ok(ToolCallKind::Subagent),
        other => Err(MemoryError::RowDecode(format!(
            "unknown tool_call kind '{other}'"
        ))),
    }
}

fn tool_call_kind_as_str(kind: ToolCallKind) -> &'static str {
    match kind {
        ToolCallKind::Mcp => "mcp",
        ToolCallKind::Subagent => "subagent",
    }
}

fn clamp_u32(n: i64) -> u32 {
    n.max(0).min(u32::MAX as i64) as u32
}
