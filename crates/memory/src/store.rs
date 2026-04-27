use std::path::Path;
use std::str::FromStr;

use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{UnknownRole, i64_to_u32, i64_to_u64, u64_to_i64};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{SqliteConnection, SqlitePool, sqlite::SqliteRow};
use uuid::Uuid;

use crate::types::UnknownMemoryKind;
use crate::{
    BackendConfig, BundledEmbedder, ConfigError, Memory, MemoryConfig, MemoryError, MemoryId,
    MemoryKind, Message, MessageId, Role, StoredMessage, TokenCount, UserId,
};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "memory";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("memory has only one schema version")
    }
}

/// Top-level memory infrastructure. Owns the embedder and the `SQLite` pool
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
    /// Open a Store against an externally-provided `SQLite` pool. Cli
    /// owns the pool (via `memory::open_pool`) and hands clones to
    /// every persistent crate. Memory runs its own schema migrations
    /// against the pool — it owns only the `messages` and `memories`
    /// tables.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    ///
    /// `fallback_api_key` is tried when the embedder config does not carry
    /// its own key — caller passes the matching entry from `providers:`.
    pub async fn open(
        pool: SqlitePool,
        config: MemoryConfig,
        fallback_api_key: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let embedder = BundledEmbedder::from_config(&config.embedder, fallback_api_key)?;
        migrate::run(&pool, &Schema).await?;
        Ok(Self {
            config,
            embedder,
            pool,
        })
    }

    #[must_use]
    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    #[must_use]
    pub fn embedder(&self) -> &BundledEmbedder {
        &self.embedder
    }

    /// Obtain a scoped handle for `user_id`. Does not create any rows until
    /// the caller writes something.
    #[must_use]
    pub fn for_user(&self, user_id: UserId) -> UserMemory<'_> {
        UserMemory {
            store: self,
            user_id,
        }
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn conversation_summaries(&self) -> Result<Vec<ConversationSummary>, MemoryError> {
        let rows = sqlx::query(
            "SELECT user_id, \
                    COUNT(*) AS message_count, \
                    SUM(token_count) AS total_tokens, \
                    MIN(created_at) AS first_message_at, \
                    MAX(created_at) AS last_message_at \
             FROM messages \
             GROUP BY user_id \
             ORDER BY last_message_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let user_id: String = row.try_get("user_id")?;
            let message_count: i64 = row.try_get("message_count")?;
            let total_tokens: i64 = row.try_get("total_tokens")?;
            let first_message_at: i64 = row.try_get("first_message_at")?;
            let last_message_at: i64 = row.try_get("last_message_at")?;
            out.push(ConversationSummary {
                first_message_at: first_message_at.try_into().unwrap_or(0u64),
                last_message_at: last_message_at.try_into().unwrap_or(0u64),
                message_count: clamp_u32(message_count),
                total_tokens: total_tokens.try_into().unwrap_or(0u64),
                user_id: UserId(parse_uuid(&user_id, "user id")?),
            });
        }
        Ok(out)
    }

    /// Summaries of every user the store has seen, ordered by most recent
    /// activity first. Intended for read-only studio views. Counts and
    /// activity timestamps reflect only memory-owned tables (messages,
    /// memories); studio composes other per-feature counts (scores,
    /// tool calls) from the crates that own them.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn list_user_summaries(&self) -> Result<Vec<UserSummary>, MemoryError> {
        let rows = sqlx::query(
            "SELECT u.user_id AS user_id, \
                    COALESCE((SELECT COUNT(*) FROM messages m WHERE m.user_id = u.user_id), 0) AS message_count, \
                    COALESCE((SELECT COUNT(*) FROM memories mm WHERE mm.user_id = u.user_id), 0) AS memory_count, \
                    COALESCE(( \
                        SELECT MAX(created_at) FROM ( \
                            SELECT created_at FROM messages WHERE user_id = u.user_id \
                            UNION ALL \
                            SELECT created_at FROM memories WHERE user_id = u.user_id \
                        ) \
                    ), 0) AS last_activity_at \
             FROM ( \
                 SELECT DISTINCT user_id FROM messages \
                 UNION \
                 SELECT DISTINCT user_id FROM memories \
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
            let last_activity_at: i64 = row.try_get("last_activity_at")?;
            out.push(UserSummary {
                last_activity_at: i64_to_u64(last_activity_at),
                memory_count: clamp_u32(memory_count),
                message_count: clamp_u32(message_count),
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

impl UserMemory<'_> {
    #[must_use]
    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    /// Append a message to the user's conversation history.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(u64_to_i64(stored.created_at))
        .bind(stored.id.0.to_string())
        .bind(stored.role.as_str())
        .bind(i64::from(stored.token_count.0))
        .bind(self.user_id.0.to_string())
        .execute(&self.store.pool)
        .await?;
        Ok(stored.id)
    }

    /// Record a long-term memory (fact or preference) for this user. Always
    /// inserts — use `remember_if_novel` to skip near-duplicates.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn message_count(&self) -> Result<usize, MemoryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages WHERE user_id = ?")
            .bind(self.user_id.0.to_string())
            .fetch_one(&self.store.pool)
            .await?;
        Ok(usize::try_from(row.0.max(0)).unwrap_or(0))
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn memory_count(&self) -> Result<usize, MemoryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memories WHERE user_id = ?")
            .bind(self.user_id.0.to_string())
            .fetch_one(&self.store.pool)
            .await?;
        Ok(usize::try_from(row.0.max(0)).unwrap_or(0))
    }

    /// Full conversation history for this user, in chronological order.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn messages(&self) -> Result<Vec<StoredMessage>, MemoryError> {
        let rows = sqlx::query(
            "SELECT content, created_at, id, role, token_count, user_id \
             FROM messages WHERE user_id = ? ORDER BY rowid ASC",
        )
        .bind(self.user_id.0.to_string())
        .fetch_all(&self.store.pool)
        .await?;
        rows.iter().map(row_to_stored_message).collect()
    }

    /// All long-term memories recorded for this user, in insertion order.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn memories(&self) -> Result<Vec<Memory>, MemoryError> {
        self.load_memories().await
    }

    /// Assemble a context window for an upcoming prompt. Takes the new user
    /// message (used for semantic recall) and a total token budget. Returns
    /// recalled memories and the most-recent conversation messages that fit
    /// within the budget, in chronological order. The new message is *not*
    /// included — the caller appends it.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn assemble_context(
        &self,
        new_user_message: &str,
        budget: TokenCount,
    ) -> Result<AssembledContext, MemoryError> {
        let recalled = self
            .recall(new_user_message, self.store.config.recall_k)
            .await?;

        let scaled = f64::from(budget.0) * f64::from(self.store.config.memory_budget_fraction);
        // Clamped to [0, u32::MAX] just above; the truncating cast cannot
        // overflow or sign-flip and only drops the fractional part.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let memory_budget = TokenCount(scaled.clamp(0.0, f64::from(u32::MAX)) as u32);
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
        .bind(u64_to_i64(memory.created_at))
        .bind(vec_to_bytes(&memory.embedding))
        .bind(i64::try_from(memory.embedding.len()).unwrap_or(i64::MAX))
        .bind(self.store.config.embedder.model_id())
        .bind(memory.id.0.to_string())
        .bind(memory.kind.as_str())
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
        rows.iter().map(row_to_memory).collect()
    }
}

/// Context ready to forward to an LLM. `memories` should typically be rendered
/// as a system/preamble block; `messages` are the recent conversation verbatim.
#[derive(Clone, Debug)]
pub struct AssembledContext {
    pub memories: Vec<Memory>,
    pub messages: Vec<Message>,
}

pub struct ConversationSummary {
    pub first_message_at: u64,
    pub last_message_at: u64,
    pub message_count: u32,
    pub total_tokens: u64,
    pub user_id: UserId,
}

/// Aggregate view of a single user's stored data. Returned by
/// `Store::list_user_summaries` for studio-style overviews. Counts
/// reflect only memory-owned tables; studio composes per-feature
/// counts (tool calls, scores, …) from the crates that own them.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct UserSummary {
    pub last_activity_at: u64,
    pub memory_count: u32,
    pub message_count: u32,
    pub user_id: UserId,
}

/// Open a `SQLite` pool from a `BackendConfig`. Public so cli can open
/// one pool and hand clones to every persistent crate (memory, judge,
/// telemetry, limits) instead of borrowing memory's. Each crate runs
/// its own `CREATE TABLE IF NOT EXISTS` against the shared pool, so
/// table ownership stays clear even though the connection is shared.
///
/// # Errors
///
/// Returns an error if the underlying operation fails.
pub async fn open_pool(backend: &BackendConfig) -> Result<SqlitePool, ConfigError> {
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

fn row_to_memory(row: &SqliteRow) -> Result<Memory, MemoryError> {
    let content: String = row.try_get("content")?;
    let created_at: i64 = row.try_get("created_at")?;
    let embedding_blob: Vec<u8> = row.try_get("embedding")?;
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let user_id: String = row.try_get("user_id")?;
    Ok(Memory {
        content,
        created_at: i64_to_u64(created_at),
        embedding: bytes_to_vec(&embedding_blob)?,
        id: MemoryId(parse_uuid(&id, "memory id")?),
        kind: kind
            .parse()
            .map_err(|e: UnknownMemoryKind| MemoryError::RowDecode(e.to_string()))?,
        user_id: UserId(parse_uuid(&user_id, "user id")?),
    })
}

fn row_to_stored_message(row: &SqliteRow) -> Result<StoredMessage, MemoryError> {
    let content: String = row.try_get("content")?;
    let created_at: i64 = row.try_get("created_at")?;
    let id: String = row.try_get("id")?;
    let role: String = row.try_get("role")?;
    let token_count: i64 = row.try_get("token_count")?;
    let user_id: String = row.try_get("user_id")?;
    Ok(StoredMessage {
        content,
        created_at: i64_to_u64(created_at),
        id: MessageId(parse_uuid(&id, "message id")?),
        role: role
            .parse()
            .map_err(|e: UnknownRole| MemoryError::RowDecode(e.to_string()))?,
        token_count: TokenCount(i64_to_u32(token_count)),
        user_id: UserId(parse_uuid(&user_id, "user id")?),
    })
}

fn parse_uuid(s: &str, label: &str) -> Result<Uuid, MemoryError> {
    Uuid::parse_str(s).map_err(|e| MemoryError::RowDecode(format!("invalid {label}: {e}")))
}

fn clamp_u32(n: i64) -> u32 {
    i64_to_u32(n)
}
