use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use coulisse_core::migrate::{self, SchemaMigrator};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};
use thiserror::Error;

use crate::merge::{MergeReport, merge};
use crate::{ExperimentConfig, ExperimentList};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "experiments";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("experiments has only one schema version")
    }
}

/// One row in `dynamic_experiments`. `config` is `Some` for active rows
/// and `None` for tombstones, paired with `disabled = true`.
#[derive(Clone, Debug)]
pub struct DynamicExperimentRow {
    pub config: Option<ExperimentConfig>,
    pub created_at: i64,
    pub disabled: bool,
    pub name: String,
    pub updated_at: i64,
}

/// Persistent storage for runtime-mutable experiment configs.
pub struct Experiments {
    pool: SqlitePool,
}

impl Experiments {
    pub async fn open(pool: SqlitePool) -> Result<Self, ExperimentsError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    pub async fn list_dynamic(&self) -> Result<Vec<DynamicExperimentRow>, ExperimentsError> {
        let rows = sqlx::query(
            "SELECT config_json, created_at, disabled, name, updated_at \
             FROM dynamic_experiments ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_dynamic_experiment).collect()
    }

    pub async fn put_active_dynamic(
        &self,
        name: &str,
        config: &ExperimentConfig,
    ) -> Result<(), ExperimentsError> {
        let now = now_secs();
        let json = serde_json::to_string(config)
            .map_err(|e| ExperimentsError::Serialize(e.to_string()))?;
        sqlx::query(
            "INSERT INTO dynamic_experiments (config_json, created_at, disabled, name, updated_at) \
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

    pub async fn put_tombstone_dynamic(&self, name: &str) -> Result<(), ExperimentsError> {
        let now = now_secs();
        sqlx::query(
            "INSERT INTO dynamic_experiments (config_json, created_at, disabled, name, updated_at) \
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

    pub async fn delete_dynamic(&self, name: &str) -> Result<bool, ExperimentsError> {
        let result = sqlx::query("DELETE FROM dynamic_experiments WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Read every dynamic row, merge against `yaml_experiments`, and atomically
    /// swap the effective list into `list`.
    pub async fn rebuild(
        &self,
        list: &ExperimentList,
        yaml_experiments: &[ExperimentConfig],
    ) -> Result<MergeReport, ExperimentsError> {
        let db = self.list_dynamic().await?;
        let (merged, report) = merge(yaml_experiments, &db);
        let configs: Vec<ExperimentConfig> = merged.into_iter().map(|m| m.config).collect();
        list.store(Arc::new(configs));
        Ok(report)
    }
}

fn row_to_dynamic_experiment(row: SqliteRow) -> Result<DynamicExperimentRow, ExperimentsError> {
    let config_json: Option<String> = row.try_get("config_json")?;
    let created_at: i64 = row.try_get("created_at")?;
    let disabled: i64 = row.try_get("disabled")?;
    let name: String = row.try_get("name")?;
    let updated_at: i64 = row.try_get("updated_at")?;
    let config = match config_json {
        Some(s) => Some(
            serde_json::from_str::<ExperimentConfig>(&s)
                .map_err(|e| ExperimentsError::RowDecode(format!("config_json: {e}")))?,
        ),
        None => None,
    };
    Ok(DynamicExperimentRow {
        config,
        created_at,
        disabled: disabled != 0,
        name,
        updated_at,
    })
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Error)]
pub enum ExperimentsError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("schema migration failed: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
    #[error("failed to decode row: {0}")]
    RowDecode(String),
    #[error("failed to serialize config: {0}")]
    Serialize(String),
}
