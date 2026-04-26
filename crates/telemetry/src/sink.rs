use coulisse_core::{ToolCallKind, TurnId, UserId};
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use crate::error::TelemetryError;
use crate::event::{Event, EventKind};
use crate::id::EventId;
use crate::tool_call::{ToolCall, ToolCallId};

pub struct ActivityCounts {
    pub turn_count: u32,
    pub user_count: u32,
}

pub struct ToolCallStats {
    pub call_count: u32,
    pub error_count: u32,
    pub kind: ToolCallKind,
    pub tool_name: String,
    pub user_count: u32,
}

const SCHEMA_SQL: &str = include_str!("../migrations/schema.sql");
const MIGRATE_SQL: &str = include_str!("../migrations/migrate.sql");

/// Read-only handle onto the telemetry tables. Writes flow exclusively
/// through `SqliteLayer`, which mirrors `tracing` spans into the same
/// `events` and `tool_calls` tables that this struct reads back for the
/// studio UI. `Sink::open` is still the entry point that applies the
/// schema migrations, so cli runs it once at startup before the layer
/// starts emitting rows.
///
/// Cheap to clone via the wrapping `Arc` callers are expected to hold.
pub struct Sink {
    pool: SqlitePool,
}

impl Sink {
    /// Apply the telemetry schema and return a ready-to-use sink. Schema
    /// statements use `CREATE IF NOT EXISTS` so it's safe to call against a
    /// pool already used by other crates.
    pub async fn open(pool: SqlitePool) -> Result<Self, TelemetryError> {
        for stmt in split_sql(SCHEMA_SQL) {
            sqlx::query(&stmt).execute(&pool).await?;
        }
        // Migrate steps may run on a fresh database where the target shape
        // is already in place from schema.sql. ALTER TABLE RENAME COLUMN
        // can't be made idempotent in pure SQL, so swallow the "no such
        // column" / "duplicate column" errors that signal "already
        // applied" and let everything else surface.
        for stmt in split_sql(MIGRATE_SQL) {
            if let Err(err) = sqlx::query(&stmt).execute(&pool).await
                && !is_already_applied(&err)
            {
                return Err(err.into());
            }
        }
        Ok(Self { pool })
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

    /// All tool calls for one user, chronological. Studio uses this to
    /// render the per-message tool-call panel.
    pub async fn tool_calls_for_user(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ToolCall>, TelemetryError> {
        let rows = sqlx::query(
            "SELECT args, created_at, error, id, kind, ordinal, result, tool_name, \
             turn_id, user_id FROM tool_calls WHERE user_id = ? ORDER BY rowid ASC",
        )
        .bind(user_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_tool_call).collect()
    }

    pub async fn recent_activity_counts(
        &self,
        since: u64,
    ) -> Result<ActivityCounts, TelemetryError> {
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT user_id) AS user_count, \
             COUNT(DISTINCT correlation_id) AS turn_count \
             FROM events \
             WHERE created_at >= ?",
        )
        .bind(since as i64)
        .fetch_one(&self.pool)
        .await?;
        let turn_count: i64 = row.try_get("turn_count")?;
        let user_count: i64 = row.try_get("user_count")?;
        Ok(ActivityCounts {
            turn_count: turn_count.max(0) as u32,
            user_count: user_count.max(0) as u32,
        })
    }

    pub async fn tool_call_stats(&self, since: u64) -> Result<Vec<ToolCallStats>, TelemetryError> {
        let rows = sqlx::query(
            "SELECT tool_name, kind, \
             COUNT(*) AS call_count, \
             SUM(CASE WHEN error IS NOT NULL THEN 1 ELSE 0 END) AS error_count, \
             COUNT(DISTINCT user_id) AS user_count \
             FROM tool_calls \
             WHERE created_at >= ? \
             GROUP BY tool_name, kind \
             ORDER BY call_count DESC",
        )
        .bind(since as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let call_count: i64 = row.try_get("call_count")?;
            let error_count: i64 = row.try_get("error_count")?;
            let kind: String = row.try_get("kind")?;
            let tool_name: String = row.try_get("tool_name")?;
            let user_count: i64 = row.try_get("user_count")?;
            out.push(ToolCallStats {
                call_count: call_count.max(0) as u32,
                error_count: error_count.max(0) as u32,
                kind: parse_tool_call_kind(&kind)?,
                tool_name,
                user_count: user_count.max(0) as u32,
            });
        }
        Ok(out)
    }

    pub async fn tool_calls_for_tool(
        &self,
        tool_name: &str,
        limit: u32,
    ) -> Result<Vec<ToolCall>, TelemetryError> {
        let rows = sqlx::query(
            "SELECT args, created_at, error, id, kind, ordinal, result, tool_name, \
             turn_id, user_id FROM tool_calls \
             WHERE tool_name = ? \
             ORDER BY created_at DESC \
             LIMIT ?",
        )
        .bind(tool_name)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_tool_call).collect()
    }

    /// Tool calls for one turn, in insertion order. Used by the studio
    /// UI for per-turn detail views.
    pub async fn tool_calls_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<Vec<ToolCall>, TelemetryError> {
        let rows = sqlx::query(
            "SELECT args, created_at, error, id, kind, ordinal, result, tool_name, \
             turn_id, user_id FROM tool_calls WHERE turn_id = ? ORDER BY ordinal ASC",
        )
        .bind(turn_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_tool_call).collect()
    }

    pub async fn tool_call_count(&self, user_id: UserId) -> Result<usize, TelemetryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tool_calls WHERE user_id = ?")
            .bind(user_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 as usize)
    }
}

fn row_to_tool_call(row: SqliteRow) -> Result<ToolCall, TelemetryError> {
    let args: String = row.try_get("args")?;
    let created_at: i64 = row.try_get("created_at")?;
    let error: Option<String> = row.try_get("error")?;
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let ordinal: i64 = row.try_get("ordinal")?;
    let result: Option<String> = row.try_get("result")?;
    let tool_name: String = row.try_get("tool_name")?;
    let turn_id: String = row.try_get("turn_id")?;
    let user_id: String = row.try_get("user_id")?;

    let parse_uuid = |field: &str, s: &str| {
        Uuid::parse_str(s).map_err(|_| {
            TelemetryError::Database(sqlx::Error::Decode(
                format!("invalid uuid in {field}: {s}").into(),
            ))
        })
    };

    Ok(ToolCall {
        args,
        created_at: created_at.max(0) as u64,
        error,
        id: ToolCallId(parse_uuid("id", &id)?),
        kind: parse_tool_call_kind(&kind)?,
        ordinal: ordinal.max(0) as u32,
        result,
        tool_name,
        turn_id: TurnId(parse_uuid("turn_id", &turn_id)?),
        user_id: UserId(parse_uuid("user_id", &user_id)?),
    })
}

fn parse_tool_call_kind(s: &str) -> Result<ToolCallKind, TelemetryError> {
    match s {
        "mcp" => Ok(ToolCallKind::Mcp),
        "subagent" => Ok(ToolCallKind::Subagent),
        other => Err(TelemetryError::Database(sqlx::Error::Decode(
            format!("unknown tool_call kind: {other}").into(),
        ))),
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

fn is_already_applied(err: &sqlx::Error) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("duplicate column") || msg.contains("no such column")
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
