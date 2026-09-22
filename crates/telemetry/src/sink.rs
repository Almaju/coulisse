use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{ToolCallKind, TurnId, UserId, i64_to_u32, i64_to_u64, u64_to_i64};
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use crate::error::TelemetryError;
use crate::event::{Event, EventKind};
use crate::id::EventId;
use crate::tool_call::{ToolCall, ToolCallId};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "telemetry";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];
}

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
    /// Apply the telemetry schema and return a ready-to-use sink.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn open(pool: SqlitePool) -> Result<Self, TelemetryError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Every event for one turn, oldest first. Used by the studio UI to
    /// rebuild the call tree rooted at the `TurnStart`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        rows.iter().map(Event::from_row).collect()
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(u64_to_i64(since))
        .fetch_one(&self.pool)
        .await?;
        let turn_count: i64 = row.try_get("turn_count")?;
        let user_count: i64 = row.try_get("user_count")?;
        Ok(ActivityCounts {
            turn_count: i64_to_u32(turn_count),
            user_count: i64_to_u32(user_count),
        })
    }

    /// Most recent tool calls across all users, newest first. Used by the
    /// `/admin/live` activity feed.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn recent_tool_calls(&self, limit: u32) -> Result<Vec<ToolCall>, TelemetryError> {
        let rows = sqlx::query(
            "SELECT args, created_at, error, id, kind, ordinal, result, tool_name, \
             turn_id, user_id FROM tool_calls \
             ORDER BY created_at DESC \
             LIMIT ?",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(ToolCall::from_row).collect()
    }

    /// Turn ids for `user_id`, most recently active first, capped at `limit`.
    /// Used by the studio UI to list a user's recent turns without loading
    /// the full event stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| uuid_column(row, "correlation_id").map(TurnId))
            .collect()
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn tool_call_count(&self, user_id: UserId) -> Result<usize, TelemetryError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tool_calls WHERE user_id = ?")
            .bind(user_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(usize::try_from(row.0.max(0)).unwrap_or(0))
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(u64_to_i64(since))
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
                call_count: i64_to_u32(call_count),
                error_count: i64_to_u32(error_count),
                kind: kind.parse::<ToolCallKind>()?,
                tool_name,
                user_count: i64_to_u32(user_count),
            });
        }
        Ok(out)
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(ToolCall::from_row).collect()
    }

    /// Tool calls for one turn, in insertion order. Used by the studio
    /// UI for per-turn detail views.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        rows.iter().map(ToolCall::from_row).collect()
    }

    /// All tool calls for one user, chronological. Studio uses this to
    /// render the per-message tool-call panel.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        rows.iter().map(ToolCall::from_row).collect()
    }
}

/// Read `column` as a UUID stored in its hyphenated text form.
fn uuid_column(row: &SqliteRow, column: &'static str) -> Result<Uuid, TelemetryError> {
    let value: String = row.try_get(column)?;
    Uuid::parse_str(&value).map_err(|source| TelemetryError::InvalidUuid {
        column,
        source,
        value,
    })
}

impl ToolCall {
    fn from_row(row: &SqliteRow) -> Result<Self, TelemetryError> {
        let args: String = row.try_get("args")?;
        let created_at: i64 = row.try_get("created_at")?;
        let error: Option<String> = row.try_get("error")?;
        let kind: String = row.try_get("kind")?;
        let ordinal: i64 = row.try_get("ordinal")?;
        let result: Option<String> = row.try_get("result")?;
        let tool_name: String = row.try_get("tool_name")?;
        Ok(Self {
            args,
            created_at: i64_to_u64(created_at),
            error,
            id: ToolCallId(uuid_column(row, "id")?),
            kind: kind.parse::<ToolCallKind>()?,
            ordinal: i64_to_u32(ordinal),
            result,
            tool_name,
            turn_id: TurnId(uuid_column(row, "turn_id")?),
            user_id: UserId(uuid_column(row, "user_id")?),
        })
    }
}

impl Event {
    fn from_row(row: &SqliteRow) -> Result<Self, TelemetryError> {
        let created_at: i64 = row.try_get("created_at")?;
        let duration_ms: Option<i64> = row.try_get("duration_ms")?;
        let kind: String = row.try_get("kind")?;
        let has_parent: Option<String> = row.try_get("parent_id")?;
        let payload: String = row.try_get("payload")?;
        let parent_id = match has_parent {
            None => None,
            Some(_) => Some(EventId(uuid_column(row, "parent_id")?)),
        };
        Ok(Self {
            correlation_id: TurnId(uuid_column(row, "correlation_id")?),
            created_at: i64_to_u64(created_at),
            duration_ms: duration_ms.map(i64_to_u64),
            id: EventId(uuid_column(row, "id")?),
            kind: kind.parse::<EventKind>()?,
            parent_id,
            payload: serde_json::from_str(&payload)?,
            user_id: UserId(uuid_column(row, "user_id")?),
        })
    }
}
