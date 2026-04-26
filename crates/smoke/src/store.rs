use std::time::{SystemTime, UNIX_EPOCH};

use coulisse_core::MessageId;
use coulisse_core::migrate::{self, SchemaMigrator};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::types::{RunId, RunStatus, StoredMessage, StoredRun, TurnRole};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "smoke";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("smoke has only one schema version")
    }
}

/// Persistent storage for smoke-test runs. One row per run in
/// `smoke_runs`, plus the alternating persona/assistant turns in
/// `smoke_messages`. The runner (cli) inserts via `start_run`,
/// `record_persona_turn`, `record_assistant_turn`, and finalises with
/// `finish_run`. Reads (admin UI) go through `list_runs` / `run_detail`.
pub struct SmokeStore {
    pool: SqlitePool,
}

impl SmokeStore {
    pub async fn open(pool: SqlitePool) -> Result<Self, SmokeStoreError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Insert a fresh `running` run. Returns the freshly-minted id so
    /// the caller can persist messages and finalise it later.
    pub async fn start_run(&self, test_name: &str) -> Result<RunId, SmokeStoreError> {
        let id = RunId::new();
        sqlx::query(
            "INSERT INTO smoke_runs (id, started_at, status, test_name, total_turns) \
             VALUES (?, ?, ?, ?, 0)",
        )
        .bind(id.0.to_string())
        .bind(now_secs() as i64)
        .bind(RunStatus::Running.as_str())
        .bind(test_name)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Record which agent name the experiment router resolved to, plus
    /// the experiment name (when applicable). Called once per run after
    /// the first agent turn.
    pub async fn set_resolution(
        &self,
        run_id: RunId,
        agent_resolved: &str,
        experiment: Option<&str>,
    ) -> Result<(), SmokeStoreError> {
        sqlx::query("UPDATE smoke_runs SET agent_resolved = ?, experiment = ? WHERE id = ?")
            .bind(agent_resolved)
            .bind(experiment)
            .bind(run_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_persona_turn(
        &self,
        run_id: RunId,
        turn_index: u32,
        content: &str,
    ) -> Result<(), SmokeStoreError> {
        sqlx::query(
            "INSERT INTO smoke_messages (content, message_id, role, run_id, turn_index) \
             VALUES (?, NULL, ?, ?, ?)",
        )
        .bind(content)
        .bind(TurnRole::Persona.as_str())
        .bind(run_id.0.to_string())
        .bind(turn_index as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_assistant_turn(
        &self,
        run_id: RunId,
        turn_index: u32,
        message_id: MessageId,
        content: &str,
    ) -> Result<(), SmokeStoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO smoke_messages (content, message_id, role, run_id, turn_index) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(content)
        .bind(message_id.0.to_string())
        .bind(TurnRole::Assistant.as_str())
        .bind(run_id.0.to_string())
        .bind(turn_index as i64)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE smoke_runs SET total_turns = ? WHERE id = ?")
            .bind((turn_index as i64) + 1)
            .bind(run_id.0.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn finish_run(
        &self,
        run_id: RunId,
        status: RunStatus,
        error: Option<&str>,
    ) -> Result<(), SmokeStoreError> {
        sqlx::query("UPDATE smoke_runs SET ended_at = ?, error = ?, status = ? WHERE id = ?")
            .bind(now_secs() as i64)
            .bind(error)
            .bind(status.as_str())
            .bind(run_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Most recent runs, newest first. `limit` caps the result set so
    /// the admin list page stays bounded on noisy databases.
    pub async fn list_runs(&self, limit: u32) -> Result<Vec<StoredRun>, SmokeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_resolved, ended_at, error, experiment, id, started_at, status, \
             test_name, total_turns \
             FROM smoke_runs ORDER BY started_at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_run).collect()
    }

    pub async fn list_runs_for_test(
        &self,
        test_name: &str,
        limit: u32,
    ) -> Result<Vec<StoredRun>, SmokeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_resolved, ended_at, error, experiment, id, started_at, status, \
             test_name, total_turns \
             FROM smoke_runs WHERE test_name = ? ORDER BY started_at DESC LIMIT ?",
        )
        .bind(test_name)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_run).collect()
    }

    pub async fn get_run(&self, run_id: RunId) -> Result<Option<StoredRun>, SmokeStoreError> {
        let row = sqlx::query(
            "SELECT agent_resolved, ended_at, error, experiment, id, started_at, status, \
             test_name, total_turns \
             FROM smoke_runs WHERE id = ?",
        )
        .bind(run_id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_run).transpose()
    }

    pub async fn messages_for_run(
        &self,
        run_id: RunId,
    ) -> Result<Vec<StoredMessage>, SmokeStoreError> {
        let rows = sqlx::query(
            "SELECT content, message_id, role, run_id, turn_index \
             FROM smoke_messages WHERE run_id = ? ORDER BY turn_index ASC, role ASC",
        )
        .bind(run_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_message).collect()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn row_to_run(row: SqliteRow) -> Result<StoredRun, SmokeStoreError> {
    let agent_resolved: Option<String> = row.try_get("agent_resolved")?;
    let ended_at: Option<i64> = row.try_get("ended_at")?;
    let error: Option<String> = row.try_get("error")?;
    let experiment: Option<String> = row.try_get("experiment")?;
    let id: String = row.try_get("id")?;
    let started_at: i64 = row.try_get("started_at")?;
    let status: String = row.try_get("status")?;
    let test_name: String = row.try_get("test_name")?;
    let total_turns: i64 = row.try_get("total_turns")?;
    let status = RunStatus::parse(&status)
        .ok_or_else(|| SmokeStoreError::RowDecode(format!("invalid status: {status}")))?;
    Ok(StoredRun {
        agent_resolved,
        ended_at: ended_at.map(|v| v as u64),
        error,
        experiment,
        id: RunId(parse_uuid(&id, "run id")?),
        started_at: started_at as u64,
        status,
        test_name,
        total_turns: total_turns as u32,
    })
}

fn row_to_message(row: SqliteRow) -> Result<StoredMessage, SmokeStoreError> {
    let content: String = row.try_get("content")?;
    let message_id: Option<String> = row.try_get("message_id")?;
    let role: String = row.try_get("role")?;
    let run_id: String = row.try_get("run_id")?;
    let turn_index: i64 = row.try_get("turn_index")?;
    let role = TurnRole::parse(&role)
        .ok_or_else(|| SmokeStoreError::RowDecode(format!("invalid role: {role}")))?;
    let message_id = match message_id {
        Some(s) => Some(MessageId(parse_uuid(&s, "message id")?)),
        None => None,
    };
    Ok(StoredMessage {
        content,
        message_id,
        role,
        run_id: RunId(parse_uuid(&run_id, "run id")?),
        turn_index: turn_index as u32,
    })
}

fn parse_uuid(s: &str, label: &str) -> Result<Uuid, SmokeStoreError> {
    Uuid::parse_str(s).map_err(|e| SmokeStoreError::RowDecode(format!("invalid {label}: {e}")))
}

#[derive(Debug, Error)]
pub enum SmokeStoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("schema migration failed: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
    #[error("failed to decode row: {0}")]
    RowDecode(String),
}
