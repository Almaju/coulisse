//! Display-oriented view models built from `Store` records. Templates
//! render these directly; they're a thin layer that pre-formats anything
//! askama can't easily do (relative timestamps, role classes).

use coulisse_core::now_secs;

use crate::{ConversationSummary, Memory, MemoryKind, Role, StoredMessage};

pub(super) struct AgentConversationRow {
    pub last_activity_at: String,
    pub message_count: u32,
    pub user_id: String,
    pub user_id_short: String,
}

impl From<ConversationSummary> for AgentConversationRow {
    fn from(s: ConversationSummary) -> Self {
        let full = s.user_id.0.to_string();
        let short = if full.len() > 8 {
            format!("{}…", &full[..8])
        } else {
            full.clone()
        };
        Self {
            last_activity_at: relative_time(s.last_message_at),
            message_count: s.message_count,
            user_id: full,
            user_id_short: short,
        }
    }
}

pub(super) struct ConversationRow {
    pub duration: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub total_tokens: String,
    pub user_id: String,
}

impl From<ConversationSummary> for ConversationRow {
    fn from(s: ConversationSummary) -> Self {
        Self {
            duration: format_duration(s.first_message_at, s.last_message_at),
            last_activity_at: relative_time(s.last_message_at),
            message_count: s.message_count,
            total_tokens: format_tokens(s.total_tokens),
            user_id: s.user_id.0.to_string(),
        }
    }
}

fn format_duration(first: u64, last: u64) -> String {
    let diff = last.saturating_sub(first);
    if diff < 60 {
        return "< 1m".into();
    }
    let minutes = diff / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if days > 0 {
        let remaining_hours = hours % 24;
        if remaining_hours > 0 {
            return format!("{days}d {remaining_hours}h");
        }
        return format!("{days}d");
    }
    if hours > 0 {
        let remaining_minutes = minutes % 60;
        if remaining_minutes > 0 {
            return format!("{hours}h {remaining_minutes}m");
        }
        return format!("{hours}h");
    }
    format!("{minutes}m")
}

fn format_tokens(n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
}

pub(super) struct MessageRow {
    pub content: String,
    pub created_at: String,
    pub id: String,
    pub is_assistant: bool,
    pub role_label: &'static str,
    pub role_tone: &'static str,
    pub token_count: u32,
}

pub(super) fn message_rows(messages: Vec<StoredMessage>) -> Vec<MessageRow> {
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

pub(super) struct MemoryRow {
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
    let diff = now_secs().saturating_sub(seconds);
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
