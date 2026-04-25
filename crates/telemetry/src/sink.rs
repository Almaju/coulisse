use coulisse_core::TurnId;
use memory::UserId;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use crate::error::TelemetryError;
use crate::event::{Event, EventKind};
use crate::id::EventId;

const SCHEMA_SQL: &str = include_str!("../migrations/schema.sql");
const MIGRATE_SQL: &str = include_str!("../migrations/migrate.sql");

/// SQLite-backed event sink. Owns only telemetry tables — shares a pool
/// with `memory::Store` (same file on disk) so operators back up one thing,
/// but writes never touch tables that feed the prompt.
///
/// Clone cheaply via the wrapping `Arc` callers are expected to hold.
pub struct Sink {
    pool: SqlitePool,
}

impl Sink {
    /// Apply the telemetry schema and return a ready-to-use sink. Schema
    /// statements use `CREATE IF NOT EXISTS` so it's safe to call against a
    /// pool already used by other crates.
    pub async fn open(pool: SqlitePool) -> Result<Self, TelemetryError> {
        for stmt in split_sql(SCHEMA_SQL)
            .into_iter()
            .chain(split_sql(MIGRATE_SQL))
        {
            sqlx::query(&stmt).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    /// Persist a single event. Called on span completion; callers are free
    /// to `tokio::spawn` this to keep it off the request's critical path,
    /// but SQLite writes are fast enough that awaiting is usually fine.
    pub async fn emit(&self, event: Event) -> Result<(), TelemetryError> {
        let payload = serde_json::to_string(&event.payload)?;
        sqlx::query(
            "INSERT INTO events (correlation_id, created_at, duration_ms, id, kind, \
             parent_id, payload, user_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.correlation_id.0.to_string())
        .bind(event.created_at as i64)
        .bind(event.duration_ms.map(|d| d as i64))
        .bind(event.id.0.to_string())
        .bind(kind_as_str(event.kind))
        .bind(event.parent_id.map(|p| p.0.to_string()))
        .bind(payload)
        .bind(event.user_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every event for one turn, oldest first. Used by the studio UI to
    /// rebuild the call tree rooted at the `TurnStart`.
    pub async fn fetch_turn(
        &self,
        user_id: UserId,
        correlation_id: TurnId,
    ) -> Result<Vec<Event>, TelemetryError> {
        let rows = sqlx::query(
            "SELECT correlation_id, created_at, duration_ms, id, kind, parent_id, \
             payload, user_id FROM events \
             WHERE user_id = ? AND correlation_id = ? \
             ORDER BY created_at ASC, id ASC",
        )
        .bind(user_id.0.to_string())
        .bind(correlation_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_event).collect()
    }

    /// Turn ids for `user_id`, most recently active first, capped at `limit`.
    /// Used by the studio UI to list a user's recent turns without loading
    /// the full event stream.
    pub async fn recent_turns(
        &self,
        user_id: UserId,
        limit: u32,
    ) -> Result<Vec<TurnId>, TelemetryError> {
        let rows = sqlx::query(
            "SELECT correlation_id, MAX(created_at) AS last_seen FROM events \
             WHERE user_id = ? \
             GROUP BY correlation_id \
             ORDER BY last_seen DESC \
             LIMIT ?",
        )
        .bind(user_id.0.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let s: String = row.try_get("correlation_id")?;
                let uuid = Uuid::parse_str(&s).map_err(|_| {
                    TelemetryError::Database(sqlx::Error::Decode(
                        format!("invalid correlation_id uuid: {s}").into(),
                    ))
                })?;
                Ok(TurnId(uuid))
            })
            .collect()
    }
}

fn row_to_event(row: SqliteRow) -> Result<Event, TelemetryError> {
    let correlation_id: String = row.try_get("correlation_id")?;
    let created_at: i64 = row.try_get("created_at")?;
    let duration_ms: Option<i64> = row.try_get("duration_ms")?;
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let parent_id: Option<String> = row.try_get("parent_id")?;
    let payload: String = row.try_get("payload")?;
    let user_id: String = row.try_get("user_id")?;

    let parse_uuid = |field: &str, s: &str| {
        Uuid::parse_str(s).map_err(|_| {
            TelemetryError::Database(sqlx::Error::Decode(
                format!("invalid uuid in {field}: {s}").into(),
            ))
        })
    };

    Ok(Event {
        correlation_id: TurnId(parse_uuid("correlation_id", &correlation_id)?),
        created_at: created_at.max(0) as u64,
        duration_ms: duration_ms.map(|d| d.max(0) as u64),
        id: EventId(parse_uuid("id", &id)?),
        kind: kind_from_str(&kind)?,
        parent_id: parent_id
            .as_deref()
            .map(|s| parse_uuid("parent_id", s).map(EventId))
            .transpose()?,
        payload: serde_json::from_str(&payload)?,
        user_id: UserId(parse_uuid("user_id", &user_id)?),
    })
}

fn kind_as_str(kind: EventKind) -> &'static str {
    match kind {
        EventKind::LlmCall => "llm_call",
        EventKind::ToolCall => "tool_call",
        EventKind::TurnFinish => "turn_finish",
        EventKind::TurnStart => "turn_start",
    }
}

fn kind_from_str(s: &str) -> Result<EventKind, TelemetryError> {
    match s {
        "llm_call" => Ok(EventKind::LlmCall),
        "tool_call" => Ok(EventKind::ToolCall),
        "turn_finish" => Ok(EventKind::TurnFinish),
        "turn_start" => Ok(EventKind::TurnStart),
        other => Err(TelemetryError::Database(sqlx::Error::Decode(
            format!("unknown event kind: {other}").into(),
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;

    async fn sink() -> Sink {
        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(options).await.unwrap();
        Sink::open(pool).await.unwrap()
    }

    #[tokio::test]
    async fn round_trip_one_event() {
        let sink = sink().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let parent = EventId::new();

        let event = Event::new(
            turn,
            user,
            Some(parent),
            EventKind::ToolCall,
            json!({ "tool_name": "search_jobs", "kind": "mcp", "args": "{}" }),
        )
        .with_duration_ms(42);
        let event_id = event.id;
        sink.emit(event).await.unwrap();

        let events = sink.fetch_turn(user, turn).await.unwrap();
        assert_eq!(events.len(), 1);
        let got = &events[0];
        assert_eq!(got.id, event_id);
        assert_eq!(got.parent_id, Some(parent));
        assert_eq!(got.duration_ms, Some(42));
        assert_eq!(got.kind, EventKind::ToolCall);
        assert_eq!(got.payload["tool_name"], "search_jobs");
    }

    #[tokio::test]
    async fn nested_tree_preserves_parent_links() {
        let sink = sink().await;
        let user = UserId::new();
        let turn = TurnId::new();

        // turn_start → subagent tool_call → nested mcp tool_call
        let turn_evt = Event::new(turn, user, None, EventKind::TurnStart, json!({}));
        let turn_evt_id = turn_evt.id;
        sink.emit(turn_evt).await.unwrap();

        let sub = Event::new(
            turn,
            user,
            Some(turn_evt_id),
            EventKind::ToolCall,
            json!({ "tool_name": "job-matcher", "kind": "subagent" }),
        );
        let sub_id = sub.id;
        sink.emit(sub).await.unwrap();

        let mcp = Event::new(
            turn,
            user,
            Some(sub_id),
            EventKind::ToolCall,
            json!({ "tool_name": "search_jobs", "kind": "mcp" }),
        );
        let mcp_id = mcp.id;
        sink.emit(mcp).await.unwrap();

        let events = sink.fetch_turn(user, turn).await.unwrap();
        assert_eq!(events.len(), 3);
        let by_id: std::collections::HashMap<_, _> = events.iter().map(|e| (e.id, e)).collect();
        assert_eq!(by_id[&turn_evt_id].kind, EventKind::TurnStart);
        assert_eq!(by_id[&turn_evt_id].parent_id, None);
        assert_eq!(by_id[&sub_id].parent_id, Some(turn_evt_id));
        assert_eq!(by_id[&mcp_id].parent_id, Some(sub_id));
    }

    #[tokio::test]
    async fn isolated_by_user_and_turn() {
        let sink = sink().await;
        let alice = UserId::new();
        let bob = UserId::new();
        let turn_a = TurnId::new();
        let turn_b = TurnId::new();

        for (user, turn) in [(alice, turn_a), (alice, turn_b), (bob, turn_a)] {
            sink.emit(Event::new(
                turn,
                user,
                None,
                EventKind::TurnStart,
                json!({}),
            ))
            .await
            .unwrap();
        }

        assert_eq!(sink.fetch_turn(alice, turn_a).await.unwrap().len(), 1);
        assert_eq!(sink.fetch_turn(alice, turn_b).await.unwrap().len(), 1);
        assert_eq!(sink.fetch_turn(bob, turn_a).await.unwrap().len(), 1);
        assert_eq!(sink.recent_turns(alice, 10).await.unwrap().len(), 2);
        assert_eq!(sink.recent_turns(bob, 10).await.unwrap().len(), 1);
    }
}
