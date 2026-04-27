use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{now_secs, u64_to_i64};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::WindowKind;
use crate::{LimitError, RequestLimits};

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "limits";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("limits has only one schema version")
    }
}

/// Persistent per-user token-usage tracker. Stores the current hour/day/month
/// counter for each user in `SQLite` so limits survive restarts. Shares a pool
/// with [`memory::Store`] — there is one database per Coulisse process, with
/// one table per crate that owns state.
pub struct Tracker {
    pool: SqlitePool,
}

impl Tracker {
    /// Apply the tracker schema to `pool` and return a tracker that reads and
    /// writes the `rate_limit_windows` table.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn open(pool: SqlitePool) -> Result<Self, LimitError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Reject the request if any of the caller-supplied caps have already been
    /// reached in the current window. Returns `Ok(())` when no limits apply or
    /// every relevant bucket is below its cap.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn check(&self, user: &str, limits: RequestLimits) -> Result<(), LimitError> {
        if limits.is_empty() {
            return Ok(());
        }
        let now = now_secs();
        for (cap, kind) in [
            (limits.tokens_per_hour, WindowKind::Hour),
            (limits.tokens_per_day, WindowKind::Day),
            (limits.tokens_per_month, WindowKind::Month),
        ] {
            let Some(cap) = cap else { continue };
            let size = kind.size_secs();
            let start = now - (now % size);
            let consumed = self.count(user, kind, start).await?;
            if consumed >= cap {
                return Err(LimitError::Exceeded {
                    limit: cap,
                    retry_after: (start + size).saturating_sub(now),
                    used: consumed,
                    window: kind,
                });
            }
        }
        Ok(())
    }

    /// Add `tokens` to every window-kind counter for `user`. If the stored
    /// window for a kind has rolled over (new hour/day/month), the row is
    /// replaced with a fresh `(start, tokens)` pair instead of accumulating
    /// onto the stale value.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn record(&self, user: &str, tokens: u64) -> Result<(), LimitError> {
        if tokens == 0 {
            return Ok(());
        }
        let now = now_secs();
        for kind in [WindowKind::Hour, WindowKind::Day, WindowKind::Month] {
            let size = kind.size_secs();
            let start = u64_to_i64(now - (now % size));
            sqlx::query(
                "INSERT INTO rate_limit_windows (count, kind, start, user_id) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT(user_id, kind) DO UPDATE SET \
                   count = CASE WHEN start = excluded.start \
                                THEN count + excluded.count \
                                ELSE excluded.count END, \
                   start = excluded.start",
            )
            .bind(u64_to_i64(tokens))
            .bind(kind.as_db_str())
            .bind(start)
            .bind(user)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn count(&self, user: &str, kind: WindowKind, start: u64) -> Result<u64, LimitError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT count FROM rate_limit_windows \
             WHERE user_id = ? AND kind = ? AND start = ?",
        )
        .bind(user)
        .bind(kind.as_db_str())
        .bind(u64_to_i64(start))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map_or(0, |(c,)| c.try_into().unwrap_or(0u64)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;

    async fn tracker() -> Tracker {
        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(options).await.unwrap();
        Tracker::open(pool).await.unwrap()
    }

    #[tokio::test]
    async fn rejects_when_over_limit() {
        let tracker = tracker().await;
        let limits = RequestLimits {
            tokens_per_hour: Some(100),
            ..Default::default()
        };
        tracker.record("alice", 100).await.unwrap();
        let err = tracker.check("alice", limits).await.unwrap_err();
        match err {
            LimitError::Exceeded {
                window,
                used,
                limit,
                ..
            } => {
                assert_eq!(window, WindowKind::Hour);
                assert_eq!(used, 100);
                assert_eq!(limit, 100);
            }
            _ => panic!("expected Exceeded, got {err:?}"),
        }
    }

    #[tokio::test]
    async fn allows_under_limit() {
        let tracker = tracker().await;
        let limits = RequestLimits {
            tokens_per_hour: Some(100),
            ..Default::default()
        };
        tracker.record("alice", 50).await.unwrap();
        tracker.check("alice", limits).await.unwrap();
    }

    #[tokio::test]
    async fn no_limits_always_passes() {
        let tracker = tracker().await;
        tracker.record("alice", 1_000_000_000).await.unwrap();
        tracker
            .check("alice", RequestLimits::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn isolated_users() {
        let tracker = tracker().await;
        let limits = RequestLimits {
            tokens_per_hour: Some(10),
            ..Default::default()
        };
        tracker.record("alice", 10).await.unwrap();
        tracker.check("alice", limits).await.unwrap_err();
        tracker.check("bob", limits).await.unwrap();
    }

    #[tokio::test]
    async fn survives_process_restart() {
        // Share one backing database file between two tracker instances, just
        // like Coulisse would across a restart. Verifies that the UPSERT path
        // and the count lookup both see persisted rows.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options.clone()).await.unwrap();
        let first = Tracker::open(pool).await.unwrap();
        first.record("alice", 42).await.unwrap();
        drop(first);

        let pool = SqlitePool::connect_with(options).await.unwrap();
        let second = Tracker::open(pool).await.unwrap();
        let limits = RequestLimits {
            tokens_per_hour: Some(42),
            ..Default::default()
        };
        let err = second.check("alice", limits).await.unwrap_err();
        match err {
            LimitError::Exceeded { used, .. } => assert_eq!(used, 42),
            _ => panic!("expected Exceeded"),
        }
    }
}
