use std::sync::Arc;

use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{now_secs, u64_to_i64};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};
use thiserror::Error;

use crate::merge::{MergeReport, merge};
use crate::{AgentConfig, AgentList};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "agents";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("agents has only one schema version")
    }
}

/// One row in `dynamic_agents`. `config` is `Some` for active rows
/// (overrides and DB-only agents) and `None` for tombstones, paired with
/// `disabled = true`.
#[derive(Clone, Debug)]
pub struct DynamicRow {
    pub config: Option<AgentConfig>,
    pub created_at: i64,
    pub disabled: bool,
    pub name: String,
    pub updated_at: i64,
}

/// Persistent storage for runtime-mutable agents. Each row either overrides
/// a YAML-declared agent of the same name, stands alone as a DB-only agent,
/// or tombstones a YAML agent (`disabled = 1`). Resolution is "DB wins,
/// YAML fallback" — see the `merge` module for the precomputed list that
/// `RigAgents` actually reads.
pub struct DynamicAgents {
    pool: SqlitePool,
}

impl DynamicAgents {
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn open(pool: SqlitePool) -> Result<Self, DynamicAgentsError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Every row, in name order. Used by the merge step that produces the
    /// effective agent list.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn list(&self) -> Result<Vec<DynamicRow>, DynamicAgentsError> {
        let rows = sqlx::query(
            "SELECT config_json, created_at, disabled, name, updated_at \
             FROM dynamic_agents ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_dynamic).collect()
    }

    /// Upsert an active row (override or dynamic). `created_at` is preserved
    /// across updates; `updated_at` is bumped to now. Any existing tombstone
    /// of the same name is replaced.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn put_active(
        &self,
        name: &str,
        config: &AgentConfig,
    ) -> Result<(), DynamicAgentsError> {
        let now = u64_to_i64(now_secs());
        let json = serde_json::to_string(config)
            .map_err(|e| DynamicAgentsError::Serialize(e.to_string()))?;
        sqlx::query(
            "INSERT INTO dynamic_agents (config_json, created_at, disabled, name, updated_at) \
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

    /// Upsert a tombstone row. Replaces any existing override of the same
    /// name. Use this to disable a YAML-declared agent at runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn put_tombstone(&self, name: &str) -> Result<(), DynamicAgentsError> {
        let now = u64_to_i64(now_secs());
        sqlx::query(
            "INSERT INTO dynamic_agents (config_json, created_at, disabled, name, updated_at) \
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

    /// Physically remove the row. Returns true if a row was deleted. Used by
    /// "reset to YAML default" (drop an override so YAML reasserts) and by
    /// hard-delete of pure dynamic agents.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn delete(&self, name: &str) -> Result<bool, DynamicAgentsError> {
        let result = sqlx::query("DELETE FROM dynamic_agents WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Read every row, merge against `yaml_agents`, and atomically swap the
    /// effective list into `list`. Called once at boot, after every YAML
    /// reload, and after every admin write so all readers see one
    /// consistent view.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn rebuild(
        &self,
        list: &AgentList,
        yaml_agents: &[AgentConfig],
    ) -> Result<MergeReport, DynamicAgentsError> {
        let db = self.list().await?;
        let (merged, report) = merge(yaml_agents, &db);
        let configs: Vec<AgentConfig> = merged.into_iter().map(|m| m.config).collect();
        list.store(Arc::new(configs));
        Ok(report)
    }
}

fn row_to_dynamic(row: &SqliteRow) -> Result<DynamicRow, DynamicAgentsError> {
    let config_json: Option<String> = row.try_get("config_json")?;
    let created_at: i64 = row.try_get("created_at")?;
    let disabled: i64 = row.try_get("disabled")?;
    let name: String = row.try_get("name")?;
    let updated_at: i64 = row.try_get("updated_at")?;
    let config = match config_json {
        Some(s) => Some(
            serde_json::from_str::<AgentConfig>(&s)
                .map_err(|e| DynamicAgentsError::RowDecode(format!("config_json: {e}")))?,
        ),
        None => None,
    };
    Ok(DynamicRow {
        config,
        created_at,
        disabled: disabled != 0,
        name,
        updated_at,
    })
}

#[derive(Debug, Error)]
pub enum DynamicAgentsError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("schema migration failed: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
    #[error("failed to decode row: {0}")]
    RowDecode(String),
    #[error("failed to serialize config: {0}")]
    Serialize(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use providers::ProviderKind;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    fn sample_config(name: &str) -> AgentConfig {
        AgentConfig {
            judges: vec![],
            mcp_tools: vec![],
            model: "gpt-4o-mini".into(),
            name: name.into(),
            preamble: "be helpful".into(),
            provider: ProviderKind::Openai,
            purpose: None,
            subagents: vec![],
        }
    }

    #[tokio::test]
    async fn open_creates_schema_and_records_version() {
        let pool = pool().await;
        DynamicAgents::open(pool.clone()).await.unwrap();

        let v: String =
            sqlx::query_scalar("SELECT version FROM coulisse_schema_versions WHERE name = ?")
                .bind("agents")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(v, "0.1.0");
    }

    #[tokio::test]
    async fn put_active_round_trips() {
        let store = DynamicAgents::open(pool().await).await.unwrap();
        store
            .put_active("alice", &sample_config("alice"))
            .await
            .unwrap();

        let rows = store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "alice");
        assert!(!rows[0].disabled);
        assert_eq!(rows[0].config.as_ref().unwrap().model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn put_active_overwrites_existing_row() {
        let store = DynamicAgents::open(pool().await).await.unwrap();
        store
            .put_active("alice", &sample_config("alice"))
            .await
            .unwrap();

        let mut updated = sample_config("alice");
        updated.model = "gpt-5".into();
        store.put_active("alice", &updated).await.unwrap();

        let rows = store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].config.as_ref().unwrap().model, "gpt-5");
    }

    #[tokio::test]
    async fn put_tombstone_replaces_active_row() {
        let store = DynamicAgents::open(pool().await).await.unwrap();
        store
            .put_active("alice", &sample_config("alice"))
            .await
            .unwrap();
        store.put_tombstone("alice").await.unwrap();

        let rows = store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].disabled);
        assert!(rows[0].config.is_none());
    }

    #[tokio::test]
    async fn put_active_replaces_tombstone() {
        let store = DynamicAgents::open(pool().await).await.unwrap();
        store.put_tombstone("alice").await.unwrap();
        store
            .put_active("alice", &sample_config("alice"))
            .await
            .unwrap();

        let rows = store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].disabled);
        assert!(rows[0].config.is_some());
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let store = DynamicAgents::open(pool().await).await.unwrap();
        store
            .put_active("alice", &sample_config("alice"))
            .await
            .unwrap();

        assert!(store.delete("alice").await.unwrap());
        assert!(store.list().await.unwrap().is_empty());
        assert!(!store.delete("alice").await.unwrap());
    }

    #[tokio::test]
    async fn list_orders_by_name() {
        let store = DynamicAgents::open(pool().await).await.unwrap();
        store
            .put_active("charlie", &sample_config("charlie"))
            .await
            .unwrap();
        store
            .put_active("alice", &sample_config("alice"))
            .await
            .unwrap();
        store.put_tombstone("bob").await.unwrap();

        let names: Vec<String> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["alice", "bob", "charlie"]);
    }
}
