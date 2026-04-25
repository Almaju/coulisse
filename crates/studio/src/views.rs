//! Display-oriented view models built from `memory`/`telemetry` records.
//!
//! Templates render these directly; they're a thin layer over the storage
//! types that pre-formats anything templates can't easily do (relative
//! timestamps, JSON pretty-printing, score averaging).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use experiments::{ExperimentConfig, Strategy};
use judge::Score;
use memory::{Memory, MemoryKind, Role, StoredMessage, StoredToolCall, ToolCallKind, UserSummary};
use telemetry::{Event, EventKind};

pub struct ExperimentRow {
    pub epsilon: Option<f32>,
    pub leader: Option<String>,
    pub metric: Option<String>,
    pub min_samples: Option<u32>,
    pub name: String,
    pub purpose: Option<String>,
    pub sampling_rate: Option<f32>,
    pub show_scores: bool,
    pub sticky_by_user: bool,
    pub strategy: &'static str,
    pub variants: Vec<VariantRow>,
}

pub struct VariantRow {
    pub agent: String,
    pub is_primary: bool,
    pub mean: Option<String>,
    pub samples: Option<u32>,
    pub share: String,
    pub weight: f32,
}

impl ExperimentRow {
    /// Build the display row from a config + an optional table of
    /// mean scores keyed by agent name. Pass an empty map when no
    /// metric scores are available; the score columns then render as
    /// "—".
    pub fn build(
        exp: &ExperimentConfig,
        scores: &std::collections::HashMap<String, (f32, u32)>,
    ) -> Self {
        let total: f32 = exp.variants.iter().map(|v| v.weight).sum();
        let primary = exp.primary.as_deref();
        let show_scores = matches!(exp.strategy, Strategy::Bandit);
        let leader = if show_scores {
            exp.variants
                .iter()
                .max_by(|a, b| {
                    let mean_a = scores.get(&a.agent).map(|s| s.0).unwrap_or(f32::MIN);
                    let mean_b = scores.get(&b.agent).map(|s| s.0).unwrap_or(f32::MIN);
                    mean_a
                        .partial_cmp(&mean_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .filter(|v| scores.contains_key(&v.agent))
                .map(|v| v.agent.clone())
        } else {
            None
        };
        let variants = exp
            .variants
            .iter()
            .map(|v| {
                let pct = if total > 0.0 {
                    100.0 * v.weight / total
                } else {
                    0.0
                };
                let (mean, samples) = scores
                    .get(&v.agent)
                    .map(|(m, n)| (Some(format!("{m:.2}")), Some(*n)))
                    .unwrap_or((None, None));
                VariantRow {
                    agent: v.agent.clone(),
                    is_primary: Some(v.agent.as_str()) == primary,
                    mean,
                    samples,
                    share: format!("{pct:.0}%"),
                    weight: v.weight,
                }
            })
            .collect();
        Self {
            epsilon: exp.epsilon,
            leader,
            metric: exp.metric.clone(),
            min_samples: exp.min_samples,
            name: exp.name.clone(),
            purpose: exp.purpose.clone(),
            sampling_rate: exp.sampling_rate,
            show_scores,
            sticky_by_user: exp.sticky_by_user,
            strategy: match exp.strategy {
                Strategy::Bandit => "bandit",
                Strategy::Shadow => "shadow",
                Strategy::Split => "split",
            },
            variants,
        }
    }
}

pub struct UserRow {
    pub last_activity_at: String,
    pub memory_count: u32,
    pub message_count: u32,
    pub tool_call_count: u32,
    pub user_id: String,
}

impl From<UserSummary> for UserRow {
    fn from(s: UserSummary) -> Self {
        Self {
            last_activity_at: relative_time(s.last_activity_at),
            memory_count: s.memory_count,
            message_count: s.message_count,
            tool_call_count: s.tool_call_count,
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
    pub tool_calls: Vec<ToolCallRow>,
}

pub struct ToolCallRow {
    pub args: String,
    pub error: Option<String>,
    pub kind_class: &'static str,
    pub kind_label: &'static str,
    pub outcome_label: &'static str,
    pub result: Option<String>,
    pub tool_name: String,
}

impl From<StoredToolCall> for ToolCallRow {
    fn from(t: StoredToolCall) -> Self {
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

pub fn message_rows(
    messages: Vec<StoredMessage>,
    tool_calls: Vec<StoredToolCall>,
) -> Vec<MessageRow> {
    let mut by_message: HashMap<String, Vec<(u32, ToolCallRow)>> = HashMap::new();
    for tc in tool_calls {
        let key = tc.message_id.0.to_string();
        let ord = tc.ordinal;
        by_message.entry(key).or_default().push((ord, tc.into()));
    }
    for calls in by_message.values_mut() {
        calls.sort_by_key(|(ord, _)| *ord);
    }
    messages
        .into_iter()
        .map(|m| {
            let id = m.id.0.to_string();
            let tool_calls = by_message
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                .map(|(_, t)| t)
                .collect();
            let (role_label, role_tone) = match m.role {
                Role::Assistant => ("assistant", "bg-indigo-950/40 border-indigo-900/60"),
                Role::System => ("system", "bg-slate-950/60 border-slate-800"),
                Role::User => ("user", "bg-slate-900 border-slate-800"),
            };
            MessageRow {
                content: m.content,
                created_at: relative_time(m.created_at),
                id,
                is_assistant: matches!(m.role, Role::Assistant),
                role_label,
                role_tone,
                token_count: m.token_count.0,
                tool_calls,
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

pub struct ScoreRow {
    pub created_at: String,
    pub criterion: String,
    pub judge_name: String,
    pub reasoning: String,
    pub score: String,
}

pub struct CriterionAverageRow {
    pub average: String,
    pub count: u32,
    pub criterion: String,
    pub judge_name: String,
}

pub struct ScoresPanel {
    pub averages: Vec<CriterionAverageRow>,
    pub recent: Vec<ScoreRow>,
}

impl ScoresPanel {
    pub fn build(scores: Vec<Score>) -> Self {
        let averages = average_by_criterion(&scores);
        // Most recent first, top 5 — same posture as the legacy SPA so
        // operators recognize the surface.
        let mut recent: Vec<ScoreRow> = scores
            .into_iter()
            .rev()
            .take(5)
            .map(|s| ScoreRow {
                created_at: relative_time(s.created_at),
                criterion: s.criterion,
                judge_name: s.judge_name,
                reasoning: s.reasoning,
                score: format!("{:.1}", s.score),
            })
            .collect();
        recent.shrink_to_fit();
        Self { averages, recent }
    }
}

fn average_by_criterion(scores: &[Score]) -> Vec<CriterionAverageRow> {
    let mut buckets: HashMap<(String, String), (f64, u32)> = HashMap::new();
    for s in scores {
        let entry = buckets
            .entry((s.judge_name.clone(), s.criterion.clone()))
            .or_insert((0.0, 0));
        entry.0 += s.score as f64;
        entry.1 += 1;
    }
    let mut out: Vec<CriterionAverageRow> = buckets
        .into_iter()
        .map(
            |((judge_name, criterion), (sum, count))| CriterionAverageRow {
                average: format!("{:.1}", sum / count as f64),
                count,
                criterion,
                judge_name,
            },
        )
        .collect();
    out.sort_by(|a, b| {
        a.judge_name
            .cmp(&b.judge_name)
            .then_with(|| a.criterion.cmp(&b.criterion))
    });
    out
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
    let mut children_of: HashMap<Option<telemetry::EventId>, Vec<Event>> = HashMap::new();
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
    parent: Option<telemetry::EventId>,
    depth: usize,
    children_of: &mut HashMap<Option<telemetry::EventId>, Vec<Event>>,
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

/// Format a unix timestamp (seconds) as a coarse "5m ago" / "2h ago"
/// string. The original SPA did this client-side; doing it server-side is
/// fine because the page is reloaded on navigation.
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
