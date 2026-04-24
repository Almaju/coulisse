use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    Embedder, Memory, MemoryConfig, MemoryError, MemoryId, MemoryKind, Message, MessageId, Role,
    StoredMessage, TokenCount, UserId,
};

/// Top-level memory infrastructure. Owns the embedder and all per-user data.
///
/// Callers can never touch user data except through `Store::for_user`, which
/// returns a `UserMemory` handle scoped to a single `UserId`. That handle
/// cannot observe or mutate any other user's data — isolation is a structural
/// property of the type, not a runtime filter.
pub struct Store<E: Embedder> {
    config: MemoryConfig,
    embedder: E,
    users: RwLock<HashMap<UserId, Arc<RwLock<UserData>>>>,
}

impl<E: Embedder> Store<E> {
    pub fn new(embedder: E, config: MemoryConfig) -> Self {
        Self {
            config,
            embedder,
            users: RwLock::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    /// Obtain a scoped handle for `user_id`. Creates an empty record on first access.
    pub async fn for_user(&self, user_id: UserId) -> UserMemory<'_, E> {
        let data = {
            let users = self.users.read().await;
            users.get(&user_id).cloned()
        };
        let data = match data {
            Some(d) => d,
            None => {
                let mut users = self.users.write().await;
                users
                    .entry(user_id)
                    .or_insert_with(|| Arc::new(RwLock::new(UserData::empty(user_id))))
                    .clone()
            }
        };
        UserMemory {
            data,
            store: self,
            user_id,
        }
    }
}

/// Per-user record. Never exposed; reachable only through `UserMemory`.
struct UserData {
    memories: Vec<Memory>,
    messages: Vec<StoredMessage>,
    user_id: UserId,
}

impl UserData {
    fn empty(user_id: UserId) -> Self {
        Self {
            memories: Vec::new(),
            messages: Vec::new(),
            user_id,
        }
    }
}

/// Scoped handle to one user's memory. All reads and writes are bound to
/// `self.user_id`; there is no API that accepts a different user id.
pub struct UserMemory<'a, E: Embedder> {
    data: Arc<RwLock<UserData>>,
    store: &'a Store<E>,
    user_id: UserId,
}

impl<'a, E: Embedder> UserMemory<'a, E> {
    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    /// Append a message to the user's conversation history.
    pub async fn append_message(
        &self,
        role: Role,
        content: String,
    ) -> Result<MessageId, MemoryError> {
        let stored = StoredMessage::new(self.user_id, role, content);
        let id = stored.id;
        let mut data = self.data.write().await;
        debug_assert_eq!(data.user_id, self.user_id);
        data.messages.push(stored);
        Ok(id)
    }

    /// Record a long-term memory (fact or preference) for this user.
    pub async fn remember(
        &self,
        kind: MemoryKind,
        content: String,
    ) -> Result<MemoryId, MemoryError> {
        let embedding = self.store.embedder.embed(&content).await?;
        check_dims(&embedding, self.store.embedder.ndims())?;
        let memory = Memory::new(self.user_id, kind, content, embedding);
        let id = memory.id;
        let mut data = self.data.write().await;
        debug_assert_eq!(data.user_id, self.user_id);
        data.memories.push(memory);
        Ok(id)
    }

    /// Return top-`k` memories most relevant to `query` by cosine similarity.
    pub async fn recall(&self, query: &str, k: usize) -> Result<Vec<Memory>, MemoryError> {
        let query_embedding = self.store.embedder.embed(query).await?;
        check_dims(&query_embedding, self.store.embedder.ndims())?;
        let data = self.data.read().await;
        debug_assert_eq!(data.user_id, self.user_id);
        let mut scored: Vec<(f32, &Memory)> = data
            .memories
            .iter()
            .map(|m| (cosine_similarity(&query_embedding, &m.embedding), m))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(k).map(|(_, m)| m.clone()).collect())
    }

    /// Count of messages currently persisted for this user.
    pub async fn message_count(&self) -> usize {
        self.data.read().await.messages.len()
    }

    /// Count of long-term memories for this user.
    pub async fn memory_count(&self) -> usize {
        self.data.read().await.memories.len()
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
        let data = self.data.read().await;
        debug_assert_eq!(data.user_id, self.user_id);

        let memory_budget =
            TokenCount(((budget.0 as f32) * self.store.config.memory_budget_fraction) as u32);
        let memories = fit_memories(recalled, memory_budget);
        let memories_used: TokenCount = memories
            .iter()
            .map(|m| TokenCount::estimate(&m.content))
            .fold(TokenCount(0), |a, b| a + b);
        let history_budget = budget.saturating_sub(memories_used);
        let messages = fit_messages(&data.messages, history_budget);

        Ok(AssembledContext { memories, messages })
    }
}

/// Context ready to forward to an LLM. `memories` should typically be rendered
/// as a system/preamble block; `messages` are the recent conversation verbatim.
#[derive(Clone, Debug)]
pub struct AssembledContext {
    pub memories: Vec<Memory>,
    pub messages: Vec<Message>,
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
