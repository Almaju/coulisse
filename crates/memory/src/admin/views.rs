//! Display-oriented view models built from `Store` records. Templates
//! render these directly; they're a thin layer that pre-formats anything
//! askama can't easily do (relative timestamps, role classes).

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Memory, MemoryKind, Role, StoredMessage, UserSummary};

pub struct UserRow {
    pub last_activity_at: String,
    pub memory_count: u32,
    pub message_count: u32,
    pub user_id: String,
}

impl From<UserSummary> for UserRow {
    fn from(s: UserSummary) -> Self {
        Self {
            last_activity_at: relative_time(s.last_activity_at),
            memory_count: s.memory_count,
            message_count: s.message_count,
            user_id: s.user_id.0.to_string(),
        }
    }
}

pub struct MessageRow {
    pub content: String,
    pub created_at: String,
    pub id: String,
    pub is_assistant: bool,
    pub role_label: &'static str,
    pub role_tone: &'static str,
    pub token_count: u32,
}

pub fn message_rows(messages: Vec<StoredMessage>) -> Vec<MessageRow> {
    messages
        .into_iter()
        .map(|m| {
            let (role_label, role_tone) = match m.role {
                Role::Assistant => ("assistant", "bg-indigo-950/40 border-indigo-900/60"),
                Role::System => ("system", "bg-slate-950/60 border-slate-800"),
                Role::User => ("user", "bg-slate-900 border-slate-800"),
            };
            MessageRow {
                content: m.content,
                created_at: relative_time(m.created_at),
                id: m.id.0.to_string(),
                is_assistant: matches!(m.role, Role::Assistant),
                role_label,
                role_tone,
                token_count: m.token_count.0,
            }
        })
        .collect()
}

pub struct MemoryRow {
    pub content: String,
    pub created_at: String,
    pub kind_label: &'static str,
}

impl From<Memory> for MemoryRow {
    fn from(m: Memory) -> Self {
        let kind_label = match m.kind {
            MemoryKind::Fact => "fact",
            MemoryKind::Preference => "preference",
        };
        Self {
            content: m.content,
            created_at: relative_time(m.created_at),
            kind_label,
        }
    }
}

/// Format a unix timestamp (seconds) as a coarse "5m ago" / "2h ago"
/// string. Server-side because the page is reloaded on navigation.
fn relative_time(seconds: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(seconds);
    let diff = now.saturating_sub(seconds);
    if diff < 60 {
        return "just now".into();
    }
    if diff < 3600 {
        return format!("{}m ago", diff / 60);
    }
    if diff < 86_400 {
        return format!("{}h ago", diff / 3600);
    }
    format!("{}d ago", diff / 86_400)
}
