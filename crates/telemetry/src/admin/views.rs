//! Display-oriented view models built from `Sink` records.

use std::collections::HashMap;

use coulisse_core::ToolCallKind;

use crate::{Event, EventId, EventKind, ToolCall};

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
