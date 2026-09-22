//! Display-oriented view models built from `Sink` records.

use std::collections::{HashMap, HashSet};

use crate::{Event, EventId, ToolCall, ToolCallStats};

pub(super) struct ToolCallRow {
    pub args: String,
    pub error: Option<String>,
    pub kind_label: &'static str,
    pub outcome_label: &'static str,
    pub result: Option<String>,
    pub tool_name: String,
}

impl From<ToolCall> for ToolCallRow {
    fn from(t: ToolCall) -> Self {
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
            kind_label: t.kind.as_str(),
            outcome_label,
            result: t.result,
            tool_name: t.tool_name,
        }
    }
}

pub(super) struct EventRow {
    /// Pre-formatted "$0.0123" string for `llm_call` events whose payload
    /// carries a `cost_usd` field. Empty for other kinds and for misses
    /// in the pricing table — the template shows the badge only when
    /// non-empty.
    pub cost: String,
    pub duration: String,
    pub indent_px: usize,
    pub kind: &'static str,
    pub label: String,
    pub payload_pretty: String,
}

impl EventRow {
    fn new(event: &Event, depth: usize) -> Self {
        let payload_pretty = match serde_json::to_string_pretty(&event.payload) {
            Ok(pretty) => pretty,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    event_id = %event.id.0,
                    "event payload could not be pretty-printed; showing it compact"
                );
                event.payload.to_string()
            }
        };
        let kind = event.kind.as_str();
        Self {
            cost: format_cost(&event.payload),
            duration: event
                .duration_ms
                .map(|d| format!("{d}ms"))
                .unwrap_or_default(),
            indent_px: depth.saturating_mul(12),
            kind,
            label: label_for(kind, &event.payload),
            payload_pretty,
        }
    }
}

/// The causal tree of one turn, keyed by parent. Events whose parent
/// isn't in the set attach to the root so we don't silently swallow
/// orphans.
pub(super) struct EventTree {
    children_of: HashMap<Option<EventId>, Vec<Event>>,
}

impl EventTree {
    /// Flatten the tree into depth-tagged rows in DFS order.
    pub(super) fn into_rows(mut self) -> Vec<EventRow> {
        let mut out = Vec::new();
        self.walk(None, 0, &mut out);
        out
    }

    fn walk(&mut self, parent: Option<EventId>, depth: usize, out: &mut Vec<EventRow>) {
        let Some(siblings) = self.children_of.remove(&parent) else {
            return;
        };
        for event in siblings {
            out.push(EventRow::new(&event, depth));
            self.walk(Some(event.id), depth + 1, out);
        }
    }
}

impl From<Vec<Event>> for EventTree {
    fn from(events: Vec<Event>) -> Self {
        let ids: HashSet<_> = events.iter().map(|e| e.id).collect();
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
        Self { children_of }
    }
}

/// Pull the most-informative inline label for an event row's collapsed
/// header. `tool_call` shows the tool name; `llm_call` shows
/// `provider/model` so the user can tell at a glance which model the
/// cost belongs to.
fn label_for(kind: &str, payload: &serde_json::Value) -> String {
    match kind {
        "llm_call" => {
            let provider = payload
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if provider.is_empty() && model.is_empty() {
                String::new()
            } else if provider.is_empty() {
                model.to_string()
            } else {
                format!("{provider}/{model}")
            }
        }
        _ => payload
            .get("tool_name")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
    }
}

/// Render `cost_usd` as a human-readable USD amount. Sub-cent values
/// stay readable as `$0.0001` rather than rounding to `$0.00`. Empty
/// when the field is missing or non-numeric.
fn format_cost(payload: &serde_json::Value) -> String {
    let usd = payload.get("cost_usd").and_then(serde_json::Value::as_f64);
    match usd {
        Some(v) if v >= 0.01 => format!("${v:.4}"),
        Some(v) if v > 0.0 => format!("${v:.6}"),
        _ => String::new(),
    }
}

/// "just now" / "5m ago" / "3h ago" / "2d ago" for something `age_secs`
/// old.
fn relative_time(diff: u64) -> String {
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

pub(super) struct RecentToolCallRow {
    pub args: String,
    pub created_at: String,
    pub error: Option<String>,
    pub result: Option<String>,
    pub user_id: String,
}

impl RecentToolCallRow {
    /// `now` is the Unix-seconds instant the page is rendered at, so the
    /// "5m ago" column is relative to it.
    pub(super) fn new(call: ToolCall, now: u64) -> Self {
        Self {
            args: call.args,
            created_at: relative_time(now.saturating_sub(call.created_at)),
            error: call.error,
            result: call.result,
            user_id: call.user_id.0.to_string(),
        }
    }
}

pub(super) struct ToolDetailRow {
    pub call_count: u32,
    pub error_count: u32,
    pub error_rate: String,
    pub kind_label: &'static str,
    pub tool_name: String,
    pub user_count: u32,
}

impl From<&ToolCallStats> for ToolDetailRow {
    fn from(stats: &ToolCallStats) -> Self {
        Self {
            call_count: stats.call_count,
            error_count: stats.error_count,
            error_rate: stats.error_rate_label(),
            kind_label: stats.kind.as_str(),
            tool_name: stats.tool_name.clone(),
            user_count: stats.user_count,
        }
    }
}

pub(super) struct ToolListRow {
    pub call_count: u32,
    pub error_count: u32,
    pub error_rate: String,
    pub error_rate_high: bool,
    pub kind_label: &'static str,
    pub tool_name: String,
    pub user_count: u32,
}

impl From<ToolCallStats> for ToolListRow {
    fn from(stats: ToolCallStats) -> Self {
        Self {
            call_count: stats.call_count,
            error_count: stats.error_count,
            error_rate: stats.error_rate_label(),
            error_rate_high: stats.error_rate() > 0.1,
            kind_label: stats.kind.as_str(),
            tool_name: stats.tool_name,
            user_count: stats.user_count,
        }
    }
}

impl ToolCallStats {
    /// Fraction of calls that errored, `0.0` when nothing was called.
    fn error_rate(&self) -> f64 {
        if self.call_count == 0 {
            return 0.0;
        }
        f64::from(self.error_count) / f64::from(self.call_count)
    }

    fn error_rate_label(&self) -> String {
        let pct = self.error_rate() * 100.0;
        if pct == 0.0 {
            "0%".into()
        } else if pct < 0.1 {
            "<0.1%".into()
        } else {
            format!("{pct:.1}%")
        }
    }
}
