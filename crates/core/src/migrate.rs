//! Schema migration runner shared by every persistent feature crate.
//!
//! Each crate declares a `SchemaMigrator` and calls [`run`] on startup against
//! the shared pool. Versions are `SemVer` strings — the crate versions in which
//! the schema actually changed, ascending. A fresh database receives
//! `SCHEMA` and is recorded at the latest version. An older database walks
//! `upgrade_from(v)` forward through the version list until it reaches the
//! latest.
//!
//! Versions are stored in a single shared `coulisse_schema_versions` table
//! (one row per feature, keyed by `NAME`). Never `PRAGMA user_version` —
//! that's one int per database, but Coulisse shares one database across
//! crates.

use sqlx::{Executor, SqliteConnection, SqlitePool};
use std::future::Future;

/// One step of a schema upgrade: the version we're stepping *from* and an
/// open transaction to run the upgrade SQL inside. The runner commits the
/// transaction on success and rolls back on error.
pub struct MigrationStep<'a> {
    pub conn: &'a mut SqliteConnection,
    pub from_version: &'a str,
}

/// Migration contract for a feature crate's slice of the database.
///
/// Implementations are typically zero-sized marker structs. The associated
/// constants drive the runner; the only method, `upgrade_from`, runs the
/// step from `from_version` to the next entry in `VERSIONS`.
pub trait SchemaMigrator {
    /// Feature name. Used as the primary key in `coulisse_schema_versions`.
    /// Stable forever — renaming this strands prior versions.
    const NAME: &'static str;

    /// Crate versions in which the schema changed, ascending. The last entry
    /// is the version this code targets. Versions whose releases didn't
    /// touch the schema are absent.
    ///
    /// Must be non-empty, valid `SemVer`, and strictly ascending.
    const VERSIONS: &'static [&'static str];

    /// Full current schema. Applied verbatim on a fresh database; the runner
    /// splits it into individual statements on `;`. Use `CREATE TABLE IF NOT
    /// EXISTS` so callers can no-op apply against pre-existing schemas
    /// during local dev (the version table prevents duplicate runs in
    /// production).
    const SCHEMA: &'static str;

    /// Upgrade the database from `step.from_version` to the next entry in
    /// `VERSIONS`. Called once per gap between the stored version and the
    /// latest. `step.conn` is inside a transaction; the runner commits on
    /// success and rolls back on error.
    ///
    /// `step.from_version` is always one of `VERSIONS[..len()-1]`.
    /// Match-arm on it; an unknown value should return
    /// `sqlx::Error::Protocol` describing the stranded version.
    ///
    /// The default implementation returns a protocol error and is correct
    /// for crates with a single schema version: the runner never reaches
    /// `upgrade_from` for them, so the body would otherwise be
    /// `unreachable!()`.
    fn upgrade_from(
        &self,
        step: MigrationStep<'_>,
    ) -> impl Future<Output = sqlx::Result<()>> + Send {
        async move {
            Err(sqlx::Error::Protocol(format!(
                "{}: upgrade_from('{}') called but no migration path defined",
                Self::NAME,
                step.from_version,
            )))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("schema migrator '{0}' declares an empty VERSIONS list")]
    EmptyVersions(&'static str),
    #[error("schema migrator '{0}' has invalid SemVer in VERSIONS: '{1}'")]
    InvalidVersion(&'static str, String),
    #[error("sqlx: {0}")]
    Sqlx(sqlx::Error),
    #[error(
        "schema migrator '{name}' stored version '{stored}' is not in VERSIONS — downgrade across a schema bump?"
    )]
    UnknownStoredVersion { name: &'static str, stored: String },
    #[error("schema migrator '{0}' has unsorted or duplicate VERSIONS")]
    UnsortedVersions(&'static str),
}

impl From<sqlx::Error> for MigrateError {
    fn from(error: sqlx::Error) -> Self {
        Self::Sqlx(error)
    }
}

/// Bring the slice of `pool` owned by `M` up to its latest version.
///
/// # Errors
///
/// Returns an error if the underlying operation fails.
///
/// Idempotent: a process restart with an unchanged `M::VERSIONS` does
/// nothing. Each upgrade step is its own transaction, so a crash mid-walk
/// resumes from the last committed step on next startup.
pub async fn run<M: SchemaMigrator>(
    sqlite_pool: &SqlitePool,
    schema_migrator: &M,
) -> Result<(), MigrateError> {
    let target = validate::<M>()?;

    sqlite_pool
        .execute(
            "CREATE TABLE IF NOT EXISTS coulisse_schema_versions (\
                name    TEXT NOT NULL PRIMARY KEY,\
                version TEXT NOT NULL\
            )",
        )
        .await?;

    let stored: Option<String> =
        sqlx::query_scalar("SELECT version FROM coulisse_schema_versions WHERE name = ?")
            .bind(M::NAME)
            .fetch_optional(sqlite_pool)
            .await?;

    let Some(stored) = stored else {
        initialize::<M>(sqlite_pool, target).await?;
        tracing::info!(feature = M::NAME, version = target, "schema initialized");
        return Ok(());
    };

    let start = M::VERSIONS
        .iter()
        .position(|v| *v == stored.as_str())
        .ok_or(MigrateError::UnknownStoredVersion {
            name: M::NAME,
            stored: stored.clone(),
        })?;

    for i in start..M::VERSIONS.len() - 1 {
        let from = M::VERSIONS[i];
        let to = M::VERSIONS[i + 1];
        let mut tx = sqlite_pool.begin().await?;
        schema_migrator
            .upgrade_from(MigrationStep {
                conn: &mut tx,
                from_version: from,
            })
            .await?;
        sqlx::query("UPDATE coulisse_schema_versions SET version = ? WHERE name = ?")
            .bind(to)
            .bind(M::NAME)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(feature = M::NAME, from, to, "schema upgraded");
    }

    Ok(())
}

async fn initialize<M: SchemaMigrator>(
    sqlite_pool: &SqlitePool,
    target: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = sqlite_pool.begin().await?;
    for stmt in split_sql(M::SCHEMA) {
        sqlx::query(&stmt).execute(&mut *tx).await?;
    }
    sqlx::query(
        "INSERT INTO coulisse_schema_versions (name, version) VALUES (?, ?)\
         ON CONFLICT(name) DO UPDATE SET version = excluded.version",
    )
    .bind(M::NAME)
    .bind(target)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

fn validate<M: SchemaMigrator>() -> Result<&'static str, MigrateError> {
    let target = M::VERSIONS
        .last()
        .ok_or(MigrateError::EmptyVersions(M::NAME))?;
    let mut parsed: Vec<semver::Version> = Vec::with_capacity(M::VERSIONS.len());
    for v in M::VERSIONS {
        let parsed_v = semver::Version::parse(v)
            .map_err(|_| MigrateError::InvalidVersion(M::NAME, (*v).to_string()))?;
        parsed.push(parsed_v);
    }
    if parsed.windows(2).any(|w| w[0] >= w[1]) {
        return Err(MigrateError::UnsortedVersions(M::NAME));
    }
    Ok(*target)
}

fn split_sql(sql: &str) -> Vec<String> {
    let stripped: String = sql
        .lines()
        .map(|line| match line.find("--") {
            None => line,
            Some(i) => &line[..i],
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
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    struct Fresh;

    impl SchemaMigrator for Fresh {
        const NAME: &'static str = "fresh";
        const SCHEMA: &'static str =
            "CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL)";
        const VERSIONS: &'static [&'static str] = &["0.1.0"];
    }

    #[tokio::test]
    async fn fresh_database_applies_schema_and_records_version() {
        let pool = pool().await;
        run(&pool, &Fresh).await.unwrap();

        let v: String =
            sqlx::query_scalar("SELECT version FROM coulisse_schema_versions WHERE name = ?")
                .bind("fresh")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(v, "0.1.0");

        sqlx::query("INSERT INTO widgets (name) VALUES ('a')")
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn idempotent_on_restart() {
        let pool = pool().await;
        run(&pool, &Fresh).await.unwrap();
        run(&pool, &Fresh).await.unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM coulisse_schema_versions WHERE name = 'fresh'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    struct Upgrader;

    impl SchemaMigrator for Upgrader {
        const NAME: &'static str = "upgrader";
        const SCHEMA: &'static str =
            "CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL, color TEXT)";
        const VERSIONS: &'static [&'static str] = &["0.1.0", "0.3.0"];

        async fn upgrade_from(&self, step: MigrationStep<'_>) -> sqlx::Result<()> {
            match step.from_version {
                "0.1.0" => {
                    sqlx::query("ALTER TABLE widgets ADD COLUMN color TEXT")
                        .execute(step.conn)
                        .await?;
                    Ok(())
                },
                other => Err(sqlx::Error::Protocol(format!(
                    "upgrader: no upgrade path from '{other}'",
                ))),
            }
        }
    }

    #[tokio::test]
    async fn walks_upgrade_chain_from_old_version() {
        let pool = pool().await;
        sqlx::query("CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE coulisse_schema_versions (\
                name    TEXT NOT NULL PRIMARY KEY,\
                version TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO coulisse_schema_versions (name, version) VALUES ('upgrader', '0.1.0')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run(&pool, &Upgrader).await.unwrap();

        let v: String =
            sqlx::query_scalar("SELECT version FROM coulisse_schema_versions WHERE name = ?")
                .bind("upgrader")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(v, "0.3.0");

        sqlx::query("INSERT INTO widgets (name, color) VALUES ('a', 'red')")
            .execute(&pool)
            .await
            .unwrap();
    }

    struct EmptyVersions;

    impl SchemaMigrator for EmptyVersions {
        const NAME: &'static str = "empty";
        const SCHEMA: &'static str = "";
        const VERSIONS: &'static [&'static str] = &[];
    }

    #[tokio::test]
    async fn empty_versions_is_rejected() {
        let pool = pool().await;
        let err = run(&pool, &EmptyVersions).await.unwrap_err();
        assert!(matches!(err, MigrateError::EmptyVersions("empty")));
    }

    struct Unsorted;

    impl SchemaMigrator for Unsorted {
        const NAME: &'static str = "unsorted";
        const SCHEMA: &'static str = "";
        const VERSIONS: &'static [&'static str] = &["0.2.0", "0.1.0"];
    }

    #[tokio::test]
    async fn unsorted_versions_is_rejected() {
        let pool = pool().await;
        let err = run(&pool, &Unsorted).await.unwrap_err();
        assert!(matches!(err, MigrateError::UnsortedVersions("unsorted")));
    }

    struct InvalidSemver;

    impl SchemaMigrator for InvalidSemver {
        const NAME: &'static str = "invalid";
        const SCHEMA: &'static str = "";
        const VERSIONS: &'static [&'static str] = &["v1"];
    }

    #[tokio::test]
    async fn invalid_semver_is_rejected() {
        let pool = pool().await;
        let err = run(&pool, &InvalidSemver).await.unwrap_err();
        assert!(matches!(err, MigrateError::InvalidVersion("invalid", _)));
    }
}
