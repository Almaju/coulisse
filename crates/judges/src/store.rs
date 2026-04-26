use std::future::Future;
use std::pin::Pin;

use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{AgentScoreSummary, MessageId, ScoreLookup, ScoreLookupError, UserId};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::{Score, ScoreId};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "judges";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("judges has only one schema version")
    }
}

pub struct AgentCriterionCell {
    pub agent_name: String,
    pub criterion: String,
    pub mean: f32,
    pub samples: u32,
}

pub struct JudgeVolume {
    pub count: u32,
    pub judge_name: String,
}

/// Persistent storage for LLM-judge scores. One row per criterion per
/// scored turn. Reads are exposed both directly (`scores`,
/// `mean_scores_by_agent`) and via the `ScoreLookup` trait so feature
/// crates that need to consume scores (e.g. `agents` for bandit
/// experiments) can depend on the trait in `coulisse-core` rather than
/// on `judges` itself.
pub struct Judges {
    pool: SqlitePool,
}

impl Judges {
    pub async fn open(pool: SqlitePool) -> Result<Self, JudgeStoreError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Persist one judge score row. Called from background tasks spawned
    /// off the response path so the client is never blocked.
    pub async fn append_score(&self, score: Score) -> Result<ScoreId, JudgeStoreError> {
        sqlx::query(
            "INSERT INTO scores (agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&score.agent_name)
        .bind(score.created_at as i64)
        .bind(&score.criterion)
        .bind(score.id.0.to_string())
        .bind(&score.judge_model)
        .bind(&score.judge_name)
        .bind(score.message_id.0.to_string())
        .bind(&score.reasoning)
        .bind(score.score)
        .bind(score.user_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(score.id)
    }

    pub async fn agent_criterion_matrix(
        &self,
        judge: &str,
        since: u64,
    ) -> Result<Vec<AgentCriterionCell>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, criterion, AVG(score) AS mean, COUNT(*) AS samples \
             FROM scores \
             WHERE judge_name = ? AND created_at >= ? \
             GROUP BY agent_name, criterion \
             ORDER BY agent_name, criterion",
        )
        .bind(judge)
        .bind(since as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let agent_name: String = row.try_get("agent_name")?;
            let criterion: String = row.try_get("criterion")?;
            let mean: f64 = row.try_get("mean")?;
            let samples: i64 = row.try_get("samples")?;
            out.push(AgentCriterionCell {
                agent_name,
                criterion,
                mean: mean as f32,
                samples: samples as u32,
            });
        }
        Ok(out)
    }

    pub async fn all_scores_since(
        &self,
        since: u64,
        limit: u32,
    ) -> Result<Vec<Score>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id \
             FROM scores \
             WHERE created_at >= ? \
             ORDER BY created_at DESC \
             LIMIT ?",
        )
        .bind(since as i64)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_score).collect()
    }

    /// All judge scores recorded for `user_id`, chronological.
    pub async fn scores(&self, user_id: UserId) -> Result<Vec<Score>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id \
             FROM scores WHERE user_id = ? ORDER BY rowid ASC",
        )
        .bind(user_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_score).collect()
    }

    pub async fn score_count(&self, user_id: UserId) -> Result<usize, JudgeStoreError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM scores WHERE user_id = ?")
            .bind(user_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 as usize)
    }

    pub async fn score_volume(&self, since: u64) -> Result<Vec<JudgeVolume>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT judge_name, COUNT(*) AS count \
             FROM scores \
             WHERE created_at >= ? \
             GROUP BY judge_name \
             ORDER BY judge_name",
        )
        .bind(since as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let judge_name: String = row.try_get("judge_name")?;
            let count: i64 = row.try_get("count")?;
            out.push(JudgeVolume {
                count: count as u32,
                judge_name,
            });
        }
        Ok(out)
    }

    pub async fn scores_for_agent(&self, agent_name: &str) -> Result<Vec<Score>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id \
             FROM scores WHERE agent_name = ? ORDER BY created_at DESC LIMIT 50",
        )
        .bind(agent_name)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_score).collect()
    }

    pub async fn scores_for_judge(
        &self,
        judge: &str,
        limit: u32,
    ) -> Result<Vec<Score>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id \
             FROM scores \
             WHERE judge_name = ? \
             ORDER BY created_at DESC \
             LIMIT ?",
        )
        .bind(judge)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_score).collect()
    }

    /// Mean and sample count of scores grouped by `agent_name`, scoped to
    /// `(judge, criterion)` and to scores recorded after `since`. Used by
    /// the bandit strategy. Aggregates across all users (the experiment
    /// is global, not per-user). Empty when no scores match — callers
    /// fall back to exploration.
    pub async fn mean_scores_by_agent(
        &self,
        judge: &str,
        criterion: &str,
        since: u64,
    ) -> Result<Vec<AgentScoreSummary>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, AVG(score) AS mean, COUNT(*) AS samples \
             FROM scores \
             WHERE judge_name = ? AND criterion = ? AND created_at >= ? AND agent_name <> '' \
             GROUP BY agent_name",
        )
        .bind(judge)
        .bind(criterion)
        .bind(since as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let agent_name: String = row.try_get("agent_name")?;
            let mean: f64 = row.try_get("mean")?;
            let samples: i64 = row.try_get("samples")?;
            out.push(AgentScoreSummary {
                agent_name,
                mean: mean as f32,
                samples: samples as u32,
            });
        }
        Ok(out)
    }
}

impl ScoreLookup for Judges {
    fn mean_scores_by_agent<'a>(
        &'a self,
        judge: &'a str,
        criterion: &'a str,
        since: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<AgentScoreSummary>, ScoreLookupError>> + Send + 'a>>
    {
        Box::pin(async move {
            Judges::mean_scores_by_agent(self, judge, criterion, since)
                .await
                .map_err(|e| ScoreLookupError(e.to_string()))
        })
    }
}

fn row_to_score(row: SqliteRow) -> Result<Score, JudgeStoreError> {
    let agent_name: String = row.try_get("agent_name")?;
    let created_at: i64 = row.try_get("created_at")?;
    let criterion: String = row.try_get("criterion")?;
    let id: String = row.try_get("id")?;
    let judge_model: String = row.try_get("judge_model")?;
    let judge_name: String = row.try_get("judge_name")?;
    let message_id: String = row.try_get("message_id")?;
    let reasoning: String = row.try_get("reasoning")?;
    let score: f32 = row.try_get("score")?;
    let user_id: String = row.try_get("user_id")?;
    Ok(Score {
        agent_name,
        created_at: created_at as u64,
        criterion,
        id: ScoreId(parse_uuid(&id, "score id")?),
        judge_model,
        judge_name,
        message_id: MessageId(parse_uuid(&message_id, "message id")?),
        reasoning,
        score,
        user_id: UserId(parse_uuid(&user_id, "user id")?),
    })
}

fn parse_uuid(s: &str, label: &str) -> Result<Uuid, JudgeStoreError> {
    Uuid::parse_str(s).map_err(|e| JudgeStoreError::RowDecode(format!("invalid {label}: {e}")))
}

#[derive(Debug, Error)]
pub enum JudgeStoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("schema migration failed: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
    #[error("failed to decode row: {0}")]
    RowDecode(String),
}
