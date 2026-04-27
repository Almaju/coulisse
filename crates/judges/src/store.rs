use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{
    AgentScoreSummary, MessageId, ScoreLookup, ScoreLookupError, UserId, i64_to_u32, i64_to_u64,
    now_secs, u64_to_i64,
};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::{Executor, SqliteConnection, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::config::JudgeList;
use crate::merge::{MergeReport, merge};
use crate::{JudgeConfig, Score, ScoreId};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "judges";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0", "0.2.0"];

    async fn upgrade_from(
        &self,
        from_version: &str,
        conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        match from_version {
            "0.1.0" => {
                conn.execute(
                    "CREATE TABLE IF NOT EXISTS dynamic_judges (\
                        config_json TEXT,\
                        created_at  INTEGER NOT NULL,\
                        disabled    INTEGER NOT NULL DEFAULT 0,\
                        name        TEXT    NOT NULL PRIMARY KEY,\
                        updated_at  INTEGER NOT NULL\
                    )",
                )
                .await?;
                Ok(())
            }
            _ => unreachable!("unknown judges schema version: {from_version}"),
        }
    }
}

/// One row in `dynamic_judges`. `config` is `Some` for active rows
/// (overrides and DB-only judges) and `None` for tombstones, paired with
/// `disabled = true`.
#[derive(Clone, Debug)]
pub struct DynamicJudgeRow {
    pub config: Option<JudgeConfig>,
    pub created_at: i64,
    pub disabled: bool,
    pub name: String,
    pub updated_at: i64,
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
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn open(pool: SqlitePool) -> Result<Self, JudgeStoreError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Persist one judge score row. Called from background tasks spawned
    /// off the response path so the client is never blocked.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn append_score(&self, score: Score) -> Result<ScoreId, JudgeStoreError> {
        sqlx::query(
            "INSERT INTO scores (agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&score.agent_name)
        .bind(u64_to_i64(score.created_at))
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

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(u64_to_i64(since))
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
                #[allow(clippy::cast_possible_truncation)] // score means are bounded 0..10
                mean: mean as f32,
                samples: i64_to_u32(samples),
            });
        }
        Ok(out)
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(u64_to_i64(since))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_score).collect()
    }

    /// All judge scores recorded for `user_id`, chronological.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn scores(&self, user_id: UserId) -> Result<Vec<Score>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id \
             FROM scores WHERE user_id = ? ORDER BY rowid ASC",
        )
        .bind(user_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_score).collect()
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn score_count(&self, user_id: UserId) -> Result<usize, JudgeStoreError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM scores WHERE user_id = ?")
            .bind(user_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(usize::try_from(row.0.max(0)).unwrap_or(0))
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn score_volume(&self, since: u64) -> Result<Vec<JudgeVolume>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT judge_name, COUNT(*) AS count \
             FROM scores \
             WHERE created_at >= ? \
             GROUP BY judge_name \
             ORDER BY judge_name",
        )
        .bind(u64_to_i64(since))
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let judge_name: String = row.try_get("judge_name")?;
            let count: i64 = row.try_get("count")?;
            out.push(JudgeVolume {
                count: i64_to_u32(count),
                judge_name,
            });
        }
        Ok(out)
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn scores_for_agent(&self, agent_name: &str) -> Result<Vec<Score>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT agent_name, created_at, criterion, id, judge_model, judge_name, \
             message_id, reasoning, score, user_id \
             FROM scores WHERE agent_name = ? ORDER BY created_at DESC LIMIT 50",
        )
        .bind(agent_name)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_score).collect()
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_score).collect()
    }

    /// Mean and sample count of scores grouped by `agent_name`, scoped to
    /// `(judge, criterion)` and to scores recorded after `since`. Used by
    /// the bandit strategy. Aggregates across all users (the experiment
    /// is global, not per-user). Empty when no scores match — callers
    /// fall back to exploration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
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
        .bind(u64_to_i64(since))
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let agent_name: String = row.try_get("agent_name")?;
            let mean: f64 = row.try_get("mean")?;
            let samples: i64 = row.try_get("samples")?;
            out.push(AgentScoreSummary {
                agent_name,
                #[allow(clippy::cast_possible_truncation)] // score means are bounded 0..10
                mean: mean as f32,
                samples: i64_to_u32(samples),
            });
        }
        Ok(out)
    }
}

impl Judges {
    /// Every dynamic-judge row, in name order. Used by the merge step.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn list_dynamic(&self) -> Result<Vec<DynamicJudgeRow>, JudgeStoreError> {
        let rows = sqlx::query(
            "SELECT config_json, created_at, disabled, name, updated_at \
             FROM dynamic_judges ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_dynamic_judge).collect()
    }

    /// Upsert an active row (override or dynamic). `created_at` is preserved
    /// across updates; `updated_at` is bumped to now.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn put_active_dynamic(
        &self,
        name: &str,
        config: &JudgeConfig,
    ) -> Result<(), JudgeStoreError> {
        let now = u64_to_i64(now_secs());
        let json = serde_json::to_string(config)
            .map_err(|e| JudgeStoreError::RowDecode(format!("serialize: {e}")))?;
        sqlx::query(
            "INSERT INTO dynamic_judges (config_json, created_at, disabled, name, updated_at) \
             VALUES (?, ?, 0, ?, ?) \
             ON CONFLICT(name) DO UPDATE SET \
                 config_json = excluded.config_json, \
                 disabled    = 0, \
                 updated_at  = excluded.updated_at",
        )
        .bind(json)
        .bind(now)
        .bind(name)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upsert a tombstone row. Use this to disable a YAML-declared judge at
    /// runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn put_tombstone_dynamic(&self, name: &str) -> Result<(), JudgeStoreError> {
        let now = u64_to_i64(now_secs());
        sqlx::query(
            "INSERT INTO dynamic_judges (config_json, created_at, disabled, name, updated_at) \
             VALUES (NULL, ?, 1, ?, ?) \
             ON CONFLICT(name) DO UPDATE SET \
                 config_json = NULL, \
                 disabled    = 1, \
                 updated_at  = excluded.updated_at",
        )
        .bind(now)
        .bind(name)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Physically remove the row. Returns true if a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn delete_dynamic(&self, name: &str) -> Result<bool, JudgeStoreError> {
        let result = sqlx::query("DELETE FROM dynamic_judges WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Read every dynamic row, merge against `yaml_judges`, and atomically
    /// swap the effective list into `list`. Called once at boot, after every
    /// YAML reload, and after every admin write.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn rebuild_judges(
        &self,
        list: &JudgeList,
        yaml_judges: &[JudgeConfig],
    ) -> Result<MergeReport, JudgeStoreError> {
        let db = self.list_dynamic().await?;
        let (merged, report) = merge(yaml_judges, &db);
        let configs: Vec<JudgeConfig> = merged.into_iter().map(|m| m.config).collect();
        list.store(Arc::new(configs));
        Ok(report)
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

fn row_to_score(row: &SqliteRow) -> Result<Score, JudgeStoreError> {
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
        created_at: i64_to_u64(created_at),
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

fn row_to_dynamic_judge(row: &SqliteRow) -> Result<DynamicJudgeRow, JudgeStoreError> {
    let config_json: Option<String> = row.try_get("config_json")?;
    let created_at: i64 = row.try_get("created_at")?;
    let disabled: i64 = row.try_get("disabled")?;
    let name: String = row.try_get("name")?;
    let updated_at: i64 = row.try_get("updated_at")?;
    let config = match config_json {
        Some(s) => Some(
            serde_json::from_str::<JudgeConfig>(&s)
                .map_err(|e| JudgeStoreError::RowDecode(format!("config_json: {e}")))?,
        ),
        None => None,
    };
    Ok(DynamicJudgeRow {
        config,
        created_at,
        disabled: disabled != 0,
        name,
        updated_at,
    })
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
