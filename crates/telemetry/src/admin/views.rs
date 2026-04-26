//! Display-oriented view models built from `Sink` records.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use coulisse_core::ToolCallKind;

use crate::{Event, EventId, EventKind, ToolCall, ToolCallStats};

pub struct ToolCallRow {
    pub args: String,
    pub error: Option<String>,
    pub kind_class: &'static str,
    pub kind_label: &'static str,
    pub outcome_label: &'static str,
    pub result: Option<String>,
    pub tool_name: String,
}

impl From<ToolCall> for ToolCallRow {
    fn from(t: ToolCall) -> Self {
        let (kind_label, kind_class) = match t.kind {
            ToolCallKind::Mcp => ("mcp", "bg-amber-950/60 text-amber-300 border-amber-900/60"),
            ToolCallKind::Subagent => (
                "subagent",
                "bg-emerald-950/60 text-emerald-300 border-emerald-900/60",
            ),
        };
        let outcome_label = if t.error.is_some() {
            "error"
        } else if t.result.is_some() {
            "result"
        } else {
            "pending"
        };
        Self {
            args: t.args,
            error: t.error,
            kind_class,
            kind_label,
            outcome_label,
            result: t.result,
            tool_name: t.tool_name,
        }
    }
}

pub fn tool_call_rows(calls: Vec<ToolCall>) -> Vec<ToolCallRow> {
    calls.into_iter().map(Into::into).collect()
}

pub struct EventRow {
    pub duration: String,
    pub indent_px: usize,
    pub kind: &'static str,
    pub kind_class: &'static str,
    pub label: String,
    pub payload_pretty: String,
}

/// Flatten the causal tree into depth-tagged rows in DFS order. Events
/// whose parent isn't in the current set attach to the root so we don't
/// silently swallow orphans.
pub fn event_rows(events: Vec<Event>) -> Vec<EventRow> {
    let ids: std::collections::HashSet<_> = events.iter().map(|e| e.id).collect();
    let mut children_of: HashMap<Option<EventId>, Vec<Event>> = HashMap::new();
    for e in events {
        let key = match e.parent_id {
            Some(p) if ids.contains(&p) => Some(p),
            _ => None,
        };
        children_of.entry(key).or_default().push(e);
    }
    for list in children_of.values_mut() {
        list.sort_by_key(|e| e.created_at);
    }
    let mut out = Vec::new();
    walk(None, 0, &mut children_of, &mut out);
    out
}

fn walk(
    parent: Option<EventId>,
    depth: usize,
    children_of: &mut HashMap<Option<EventId>, Vec<Event>>,
    out: &mut Vec<EventRow>,
) {
    let Some(siblings) = children_of.remove(&parent) else {
        return;
    };
    for e in siblings {
        let id = e.id;
        let payload_pretty =
            serde_json::to_string_pretty(&e.payload).unwrap_or_else(|_| e.payload.to_string());
        let label = e
            .payload
            .get("tool_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let kind = event_kind_str(e.kind);
        let kind_class = match kind {
            "tool_call" => "bg-amber-950/60 text-amber-300 border-amber-900/60",
            "llm_call" => "bg-sky-950/60 text-sky-300 border-sky-900/60",
            _ => "bg-slate-900 text-slate-300 border-slate-800",
        };
        out.push(EventRow {
            duration: e.duration_ms.map(|d| format!("{d}ms")).unwrap_or_default(),
            indent_px: depth.saturating_mul(12),
            kind,
            kind_class,
            label,
            payload_pretty,
        });
        walk(Some(id), depth + 1, children_of, out);
    }
}

fn event_kind_str(kind: EventKind) -> &'static str {
    match kind {
        EventKind::LlmCall => "llm_call",
        EventKind::ToolCall => "tool_call",
        EventKind::TurnFinish => "turn_finish",
        EventKind::TurnStart => "turn_start",
    }
}

fn kind_display(kind: ToolCallKind) -> (&'static str, &'static str) {
    match kind {
        ToolCallKind::Mcp => ("mcp", "bg-amber-950/60 text-amber-300 border-amber-900/60"),
        ToolCallKind::Subagent => (
            "subagent",
            "bg-emerald-950/60 text-emerald-300 border-emerald-900/60",
        ),
    }
}

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

pub struct RecentToolCallRow {
    pub args: String,
    pub created_at: String,
    pub error: Option<String>,
    pub result: Option<String>,
    pub user_id: String,
}

pub struct ToolDetailRow {
    pub call_count: u32,
    pub error_count: u32,
    pub error_rate: String,
    pub kind_class: &'static str,
    pub kind_label: &'static str,
    pub tool_name: String,
    pub user_count: u32,
}

pub struct ToolListRow {
    pub call_count: u32,
    pub error_count: u32,
    pub error_rate: String,
    pub error_rate_high: bool,
    pub kind_class: &'static str,
    pub kind_label: &'static str,
    pub tool_name: String,
    pub user_count: u32,
}

fn format_error_rate(error_count: u32, call_count: u32) -> String {
    if call_count == 0 {
        return "0%".into();
    }
    let pct = (error_count as f64 / call_count as f64) * 100.0;
    if pct == 0.0 {
        "0%".into()
    } else if pct < 0.1 {
        "<0.1%".into()
    } else {
        format!("{:.1}%", pct)
    }
}

pub fn recent_tool_call_rows(calls: Vec<ToolCall>) -> Vec<RecentToolCallRow> {
    calls
        .into_iter()
        .map(|c| RecentToolCallRow {
            args: c.args,
            created_at: relative_time(c.created_at),
            error: c.error,
            result: c.result,
            user_id: c.user_id.0.to_string(),
        })
        .collect()
}

pub fn tool_detail_row(stats: &ToolCallStats) -> ToolDetailRow {
    let (kind_label, kind_class) = kind_display(stats.kind);
    ToolDetailRow {
        call_count: stats.call_count,
        error_count: stats.error_count,
        error_rate: format_error_rate(stats.error_count, stats.call_count),
        kind_class,
        kind_label,
        tool_name: stats.tool_name.clone(),
        user_count: stats.user_count,
    }
}

pub fn tool_list_rows(stats: Vec<ToolCallStats>) -> Vec<ToolListRow> {
    stats
        .into_iter()
        .map(|s| {
            let (kind_label, kind_class) = kind_display(s.kind);
            let error_rate = format_error_rate(s.error_count, s.call_count);
            let error_rate_high =
                s.call_count > 0 && (s.error_count as f64 / s.call_count as f64) > 0.1;
            ToolListRow {
                call_count: s.call_count,
                error_count: s.error_count,
                error_rate,
                error_rate_high,
                kind_class,
                kind_label,
                tool_name: s.tool_name,
                user_count: s.user_count,
            }
        })
        .collect()
}
