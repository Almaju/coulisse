#![allow(unsafe_code)]

//! `tracing_subscriber::Layer` that captures `turn`, `tool_call`, and
//! `llm_call` spans and persists them to the same `events` and `tool_calls`
//! tables that the studio UI reads. The bridge is the only thing that knows
//! about `SQLite` — feature crates remain agnostic and just emit `tracing!`.
//!
//! Spans are written on close. Layer callbacks are sync; the actual `SQLite`
//! INSERT runs on a background tokio task fed by an unbounded mpsc channel,
//! so `on_close` never blocks the request hot path.
//!
//! Field extraction expects:
//!   - `turn`      span: `agent`, `experiment`, `turn_id`, `user_id`, `user_message`
//!   - `tool_call` span: `args`, `error`, `kind` (`mcp`|`subagent`), `result`, `tool_name`
//!   - `llm_call`  span: `cost_usd`, `error`, `model`, `provider`, `prompt`, `response`, `usage`
//!
//! Parent linkage in the `events` table comes from the tracing span tree:
//! a child span's `parent_id` points at the closest ancestor that this
//! layer also recorded.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use coulisse_core::{ToolCallKind, TurnId, UserId, u64_to_i64};
use sqlx::SqlitePool;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Subscriber, error};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;
use uuid::Uuid;

/// One persisted row, dispatched from `on_close`. `Flush` is a sentinel
/// for `SqliteLayerGuard::flush`: senders are still alive (the layer is
/// installed in the subscriber), so we can't rely on channel close to
/// know everything before this point has drained.
#[derive(Debug)]
enum WriteJob {
    Event(EventRow),
    Flush(oneshot::Sender<()>),
    ToolCall(ToolCallRow),
}

#[derive(Debug)]
struct EventRow {
    correlation_id: String,
    created_at_ms: u64,
    duration_ms: u64,
    id: String,
    kind: &'static str,
    parent_id: Option<String>,
    payload: String,
    user_id: String,
}

#[derive(Debug)]
struct ToolCallRow {
    args: String,
    created_at_secs: u64,
    error: Option<String>,
    id: String,
    kind: &'static str,
    ordinal: u32,
    result: Option<String>,
    tool_name: String,
    turn_id: String,
    user_id: String,
}

/// Per-span state: fields recorded so far plus the start instant. Stored in
/// the span's extension map so children can look it up via the registry.
struct SpanExt {
    event_id: Uuid,
    fields: HashMap<&'static str, String>,
    started_at: Instant,
    started_at_ms: u64,
}

/// State attached only to `turn` spans: lets descendant `tool_call` spans
/// pull a stable `(user_id, turn_id)` plus a per-turn ordinal counter.
struct TurnExt {
    ordinal: AtomicU32,
    turn_id: TurnId,
    user_id: UserId,
}

/// Returned alongside the layer. Held for the lifetime of the process; the
/// background writer drains until all senders drop (the layer's clone
/// installed in the subscriber, plus this guard's). Tests call `flush()`
/// to round-trip the writer before reading rows back.
pub struct SqliteLayerGuard {
    tx: UnboundedSender<WriteJob>,
}

impl SqliteLayerGuard {
    /// Round-trip the writer: enqueue a sentinel and wait until the writer
    /// processes it. Earlier rows are guaranteed to be on disk by the time
    /// this returns. Cheap enough to use in tests.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(WriteJob::Flush(tx)).is_err() {
            return;
        }
        let _ = rx.await;
    }
}

/// `tracing_subscriber::Layer` that mirrors selected spans into `SQLite`.
/// Cheap to clone (`tx` is `Arc`-backed inside `mpsc`), so it can be added
/// to any subscriber stack.
#[derive(Clone)]
pub struct SqliteLayer {
    tx: UnboundedSender<WriteJob>,
}

impl SqliteLayer {
    /// Spawn the background writer on the current tokio runtime and return
    /// the layer paired with a guard. Caller installs the layer with
    /// `tracing_subscriber::registry().with(layer)` and keeps the guard
    /// alive for the process lifetime (or `flush()`es it in tests).
    #[must_use]
    pub fn spawn(pool: SqlitePool) -> (Self, SqliteLayerGuard) {
        let (tx, mut rx) = mpsc::unbounded_channel::<WriteJob>();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                match job {
                    WriteJob::Flush(notify) => {
                        let _ = notify.send(());
                    }
                    other => {
                        if let Err(err) = write_job(&pool, &other).await {
                            error!(error = %err, "telemetry sqlite write failed");
                        }
                    }
                }
            }
        });
        (Self { tx: tx.clone() }, SqliteLayerGuard { tx })
    }
}

impl<S> Layer<S> for SqliteLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !is_recorded_span(attrs.metadata().name()) {
            return;
        }
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        span.extensions_mut().insert(SpanExt {
            event_id: Uuid::new_v4(),
            fields: visitor.fields,
            started_at: Instant::now(),
            started_at_ms: now_millis(),
        });

        if span.name() == "turn" {
            let extensions = span.extensions();
            let Some(span_ext) = extensions.get::<SpanExt>() else {
                return;
            };
            let user_id = span_ext
                .fields
                .get("user_id")
                .and_then(|s| Uuid::parse_str(s).ok().map(UserId::from));
            let turn_id = span_ext
                .fields
                .get("turn_id")
                .and_then(|s| Uuid::parse_str(s).ok().map(TurnId));
            drop(extensions);
            if let (Some(user_id), Some(turn_id)) = (user_id, turn_id) {
                span.extensions_mut().insert(TurnExt {
                    ordinal: AtomicU32::new(0),
                    turn_id,
                    user_id,
                });
            }
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(span_ext) = extensions.get_mut::<SpanExt>() else {
            return;
        };
        let mut visitor = FieldVisitor {
            fields: std::mem::take(&mut span_ext.fields),
        };
        values.record(&mut visitor);
        span_ext.fields = visitor.fields;
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let name = span.name();
        if !is_recorded_span(name) {
            return;
        }
        let extensions = span.extensions();
        let Some(span_ext) = extensions.get::<SpanExt>() else {
            return;
        };
        let duration_ms =
            u64::try_from(span_ext.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let event_id = span_ext.event_id;
        let started_at_ms = span_ext.started_at_ms;
        let fields = &span_ext.fields;

        // Walk to the parent that this layer also recorded; tracing already
        // skips spans we don't care about.
        let parent_event_id = span
            .scope()
            .skip(1)
            .find_map(|s| s.extensions().get::<SpanExt>().map(|e| e.event_id));

        // Walk to the root `turn` span to inherit user/turn ids and bump the
        // shared ordinal counter for tool_calls.
        let turn_ctx = span.scope().find_map(|s| {
            s.extensions()
                .get::<TurnExt>()
                .map(|t| (t.user_id, t.turn_id, &raw const t.ordinal))
        });

        let (user_id, turn_id) = match (turn_ctx, name) {
            (Some((u, t, _)), _) => (u, t),
            // A `turn` span itself carries its ids in fields, but TurnExt
            // wasn't installed because parsing failed. Recover from fields.
            (None, "turn") => {
                let user_id = fields
                    .get("user_id")
                    .and_then(|s| Uuid::parse_str(s).ok().map(UserId::from));
                let turn_id = fields
                    .get("turn_id")
                    .and_then(|s| Uuid::parse_str(s).ok().map(TurnId));
                match (user_id, turn_id) {
                    (Some(u), Some(t)) => (u, t),
                    _ => return,
                }
            }
            (None, _) => return,
        };

        let payload = build_payload(name, fields);
        let kind = match name {
            "turn" => "turn_start",
            other => other,
        };
        let job = WriteJob::Event(EventRow {
            correlation_id: turn_id.0.to_string(),
            created_at_ms: started_at_ms,
            duration_ms,
            id: event_id.to_string(),
            kind,
            parent_id: parent_event_id.map(|id| id.to_string()),
            payload: payload.to_string(),
            user_id: user_id.0.to_string(),
        });
        if self.tx.send(job).is_err() {
            return;
        }

        if name == "tool_call"
            && let Some((_, _, counter_ptr)) = turn_ctx
        {
            // SAFETY: counter lives in the `turn` span's extension map; the
            // span is kept alive while children exist, and the closing
            // child still holds a strong ref via `scope()`. The pointer is
            // valid for the remainder of this function.
            let ordinal = unsafe { (*counter_ptr).fetch_add(1, Ordering::Relaxed) };
            let kind = fields
                .get("kind")
                .and_then(|s| s.parse::<ToolCallKind>().ok())
                .unwrap_or(ToolCallKind::Mcp);
            let row = ToolCallRow {
                args: fields.get("args").cloned().unwrap_or_default(),
                created_at_secs: started_at_ms / 1000,
                error: fields.get("error").cloned().filter(|s| !s.is_empty()),
                id: Uuid::new_v4().to_string(),
                kind: kind.as_str(),
                ordinal,
                result: fields.get("result").cloned().filter(|s| !s.is_empty()),
                tool_name: fields.get("tool_name").cloned().unwrap_or_default(),
                turn_id: turn_id.0.to_string(),
                user_id: user_id.0.to_string(),
            };
            let _ = self.tx.send(WriteJob::ToolCall(row));
        }
    }
}

fn is_recorded_span(name: &str) -> bool {
    matches!(name, "turn" | "tool_call" | "llm_call")
}

/// Build the JSON payload stored in `events.payload`. Shape mirrors the
/// pre-tracing `EventKind` payloads so studio rendering doesn't change.
fn build_payload(name: &str, fields: &HashMap<&'static str, String>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    let interesting: &[&str] = match name {
        "turn" => &["agent", "experiment", "user_message"],
        "tool_call" => &["args", "error", "kind", "result", "tool_name"],
        "llm_call" => &[
            "cost_usd", "error", "model", "prompt", "provider", "response", "usage",
        ],
        _ => &[],
    };
    for &key in interesting {
        if let Some(value) = fields.get(key).filter(|s| !s.is_empty()) {
            // Try to parse JSON-shaped values back to JSON; fall back to string.
            let parsed = serde_json::Value::from_str(value)
                .unwrap_or_else(|_| serde_json::Value::String(value.clone()));
            obj.insert(key.to_string(), parsed);
        }
    }
    serde_json::Value::Object(obj)
}

async fn write_job(pool: &SqlitePool, job: &WriteJob) -> Result<(), sqlx::Error> {
    match job {
        WriteJob::Flush(_) => {}
        WriteJob::Event(row) => {
            sqlx::query(
                "INSERT INTO events (correlation_id, created_at, duration_ms, id, kind, \
                 parent_id, payload, user_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.correlation_id)
            .bind(u64_to_i64(row.created_at_ms))
            .bind(u64_to_i64(row.duration_ms))
            .bind(&row.id)
            .bind(row.kind)
            .bind(row.parent_id.as_deref())
            .bind(&row.payload)
            .bind(&row.user_id)
            .execute(pool)
            .await?;
        }
        WriteJob::ToolCall(row) => {
            sqlx::query(
                "INSERT INTO tool_calls (args, created_at, error, id, kind, ordinal, result, \
                 tool_name, turn_id, user_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.args)
            .bind(u64_to_i64(row.created_at_secs))
            .bind(row.error.as_deref())
            .bind(&row.id)
            .bind(row.kind)
            .bind(i64::from(row.ordinal))
            .bind(row.result.as_deref())
            .bind(&row.tool_name)
            .bind(&row.turn_id)
            .bind(&row.user_id)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Visitor that flattens `tracing` field values into a string map. Captures
/// every primitive type the macros can emit so payloads keep their shape.
#[derive(Default)]
struct FieldVisitor {
    fields: HashMap<&'static str, String>,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields.insert(field.name(), format!("{value:?}"));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.fields.insert(field.name(), value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sink;
    use sqlx::sqlite::SqliteConnectOptions;
    use tracing::{Instrument, info_span};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    async fn fresh_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(options).await.unwrap();
        Sink::open(pool.clone()).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn turn_span_writes_one_event_row() {
        let pool = fresh_pool().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let (layer, guard) = SqliteLayer::spawn(pool.clone());
        let subscriber = tracing_subscriber::registry().with(layer);
        let _default = subscriber.set_default();

        {
            let _span = info_span!(
                "turn",
                agent = "hello-agent",
                turn_id = %turn.0,
                user_id = %user.0,
                user_message = "hi",
            )
            .entered();
        }

        guard.flush().await;
        let sink = Sink::open(pool).await.unwrap();
        let events = sink.fetch_turn(user, turn).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["agent"], "hello-agent");
        assert_eq!(events[0].payload["user_message"], "hi");
    }

    #[tokio::test]
    async fn nested_tool_call_inherits_turn_ids_and_links_parent() {
        let pool = fresh_pool().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let (layer, guard) = SqliteLayer::spawn(pool.clone());
        let subscriber = tracing_subscriber::registry().with(layer);
        let _default = subscriber.set_default();

        async {
            let tc_span = info_span!(
                "tool_call",
                args = "{}",
                error = tracing::field::Empty,
                kind = "mcp",
                result = tracing::field::Empty,
                tool_name = "search_jobs",
            );
            async {
                tc_span.record("result", "found");
            }
            .instrument(tc_span.clone())
            .await;
        }
        .instrument(info_span!(
            "turn",
            agent = "agent-a",
            turn_id = %turn.0,
            user_id = %user.0,
            user_message = "do it",
        ))
        .await;

        guard.flush().await;
        let sink = Sink::open(pool).await.unwrap();
        let events = sink.fetch_turn(user, turn).await.unwrap();
        assert_eq!(events.len(), 2, "expected one turn + one tool_call event");

        let turn_evt = events
            .iter()
            .find(|e| e.kind == crate::EventKind::TurnStart);
        let tool_evt = events.iter().find(|e| e.kind == crate::EventKind::ToolCall);
        let turn_evt = turn_evt.expect("turn event present");
        let tool_evt = tool_evt.expect("tool_call event present");

        assert_eq!(tool_evt.parent_id, Some(turn_evt.id));
        assert_eq!(tool_evt.payload["tool_name"], "search_jobs");
        assert_eq!(tool_evt.payload["result"], "found");

        let calls = sink.tool_calls_for_turn(turn).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "search_jobs");
        assert_eq!(calls[0].ordinal, 0);
        assert_eq!(calls[0].result.as_deref(), Some("found"));
    }

    #[tokio::test]
    async fn ordinals_increment_per_turn() {
        let pool = fresh_pool().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let (layer, guard) = SqliteLayer::spawn(pool.clone());
        let subscriber = tracing_subscriber::registry().with(layer);
        let _default = subscriber.set_default();

        {
            let turn_span = info_span!(
                "turn",
                agent = "agent-a",
                turn_id = %turn.0,
                user_id = %user.0,
                user_message = "x",
            );
            let _enter = turn_span.enter();
            for i in 0..3 {
                let _tc = info_span!(
                    "tool_call",
                    args = "{}",
                    kind = "mcp",
                    tool_name = format!("tool_{i}"),
                )
                .entered();
            }
        }

        guard.flush().await;
        let sink = Sink::open(pool).await.unwrap();
        let calls = sink.tool_calls_for_turn(turn).await.unwrap();
        assert_eq!(calls.len(), 3);
        let mut ords: Vec<u32> = calls.iter().map(|c| c.ordinal).collect();
        ords.sort_unstable();
        assert_eq!(ords, vec![0, 1, 2]);
    }
}
