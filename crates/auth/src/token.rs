//! Self-issued API tokens for the `/v1/*` proxy.
//!
//! Coulisse mints `sk-coulisse-…` bearer tokens, stores only their SHA-256
//! digest, and gates the proxy on them when `auth.proxy.tokens` is set. Each
//! token binds to a principal (the user identity that partitions memory and
//! rate limits) and carries a spend budget — unlimited, a lifetime cap, or a
//! per-calendar-month cap. The [`TokenStore`] owns both tables and exposes a
//! `check`/`record` pair mirroring `limits::Tracker`, so the request-flow
//! handler reads the same top-to-bottom: check budget before the call, record
//! spend after.

use base64::Engine;
use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{now_secs, u64_to_i64};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{SqliteConnection, SqlitePool};
use thiserror::Error;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "auth";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("auth has only one schema version")
    }
}

/// Public, stable identity for a minted token. Distinct from the secret: the
/// id is safe to log, surface in the studio, and pass around; the secret is
/// shown once and never persisted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TokenId(pub Uuid);

impl TokenId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parse a token id from its string form (a UUID).
    ///
    /// # Errors
    ///
    /// Returns an error if `s` is not a valid UUID.
    pub fn parse(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

impl Default for TokenId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Spend cap attached to a token. `Unlimited` never blocks; `Total` caps
/// lifetime spend; `Monthly` caps spend within the current calendar month
/// (UTC) and resets on the first of each month.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Budget {
    Monthly { limit_micro_usd: i64 },
    Total { limit_micro_usd: i64 },
    Unlimited,
}

impl Budget {
    /// Construct from an admin form: a kind string plus an optional dollar
    /// amount. `unlimited` ignores the amount; `total`/`monthly` require a
    /// positive amount.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetParseError`] for an unknown kind or a missing/
    /// non-positive amount on a capped kind.
    pub fn from_parts(kind: &str, limit_usd: Option<f64>) -> Result<Self, BudgetParseError> {
        match kind {
            "monthly" => Ok(Self::Monthly {
                limit_micro_usd: require_positive(limit_usd)?,
            }),
            "total" => Ok(Self::Total {
                limit_micro_usd: require_positive(limit_usd)?,
            }),
            "unlimited" => Ok(Self::Unlimited),
            other => Err(BudgetParseError::UnknownKind(other.to_string())),
        }
    }

    fn to_db(self) -> (&'static str, Option<i64>) {
        match self {
            Self::Monthly { limit_micro_usd } => ("monthly", Some(limit_micro_usd)),
            Self::Total { limit_micro_usd } => ("total", Some(limit_micro_usd)),
            Self::Unlimited => ("unlimited", None),
        }
    }

    fn from_db(kind: &str, limit_micro_usd: Option<i64>) -> Self {
        match (kind, limit_micro_usd) {
            ("monthly", Some(limit_micro_usd)) => Self::Monthly { limit_micro_usd },
            ("total", Some(limit_micro_usd)) => Self::Total { limit_micro_usd },
            // WHY: an unrecognised kind or a capped kind with a NULL limit is
            // a corrupt row; fail open to `Unlimited` rather than blocking a
            // user behind unreadable budget state.
            _ => Self::Unlimited,
        }
    }

    /// Human-readable summary for the studio (`"unlimited"`, `"$5.00 total"`,
    /// `"$20.00 / month"`).
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Monthly { limit_micro_usd } => {
                format!("${:.2} / month", micro_to_usd(limit_micro_usd))
            }
            Self::Total { limit_micro_usd } => {
                format!("${:.2} total", micro_to_usd(limit_micro_usd))
            }
            Self::Unlimited => "unlimited".to_string(),
        }
    }
}

/// A freshly minted token. The `secret` is returned exactly once — there is
/// no way to recover it later, so the caller must surface it to the user
/// immediately.
#[derive(Clone, Debug)]
pub struct MintedToken {
    pub id: TokenId,
    pub secret: String,
}

/// The identity a presented secret resolved to. Returned by
/// [`TokenStore::verify`] and lifted into request extensions.
#[derive(Clone, Debug)]
pub struct VerifiedToken {
    pub id: TokenId,
    pub principal: String,
}

/// One token's row plus its computed spend, for the studio list/detail.
/// `spend_micro_usd` is lifetime spend; `period_spend_micro_usd` is spend
/// counted against the current budget window (all-time for `Total`, the
/// current month for `Monthly`, and lifetime for display when `Unlimited`).
#[derive(Clone, Debug)]
pub struct TokenRecord {
    pub budget: Budget,
    pub created_at: u64,
    pub id: TokenId,
    pub label: String,
    pub last_used_at: Option<u64>,
    pub period_spend_micro_usd: i64,
    pub principal: String,
    pub revoked_at: Option<u64>,
    pub spend_micro_usd: i64,
}

impl TokenRecord {
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// Lifetime spend in dollars, for display.
    #[must_use]
    pub fn spend_usd(&self) -> f64 {
        micro_to_usd(self.spend_micro_usd)
    }

    /// Spend in dollars counted against the current budget window.
    #[must_use]
    pub fn period_spend_usd(&self) -> f64 {
        micro_to_usd(self.period_spend_micro_usd)
    }
}

/// Owns the `api_tokens` and `token_usage` tables. Cloneable handle around the
/// shared `SQLite` pool.
#[derive(Clone)]
pub struct TokenStore {
    pool: SqlitePool,
}

impl TokenStore {
    /// Apply the auth schema to `pool` and return a store over it.
    ///
    /// # Errors
    ///
    /// Returns an error if the migration fails.
    pub async fn open(pool: SqlitePool) -> Result<Self, StoreError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Mint a new token: generate a high-entropy secret, store its hash with
    /// the given label/principal/budget, and return the id plus the one-time
    /// plaintext secret.
    ///
    /// # Errors
    ///
    /// Returns an error if the row cannot be written.
    pub async fn mint(
        &self,
        label: &str,
        principal: &str,
        budget: Budget,
    ) -> Result<MintedToken, StoreError> {
        let id = TokenId::new();
        let secret = generate_secret();
        let (kind, limit) = budget.to_db();
        sqlx::query(
            "INSERT INTO api_tokens \
                (budget_kind, budget_micro_usd, created_at, id, label, principal, secret_hash) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(kind)
        .bind(limit)
        .bind(u64_to_i64(now_secs()))
        .bind(id.0.to_string())
        .bind(label)
        .bind(principal)
        .bind(hash_secret(&secret))
        .execute(&self.pool)
        .await?;
        Ok(MintedToken { id, secret })
    }

    /// Resolve a presented bearer secret to its bound principal, or `None`
    /// when the secret is unknown or its token has been revoked. Updates
    /// `last_used_at` on a hit.
    ///
    /// # Errors
    ///
    /// Returns an error if the lookup fails.
    pub async fn verify(&self, presented: &str) -> Result<Option<VerifiedToken>, StoreError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT id, principal FROM api_tokens \
             WHERE secret_hash = ? AND revoked_at IS NULL",
        )
        .bind(hash_secret(presented))
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, principal)) = row else {
            return Ok(None);
        };
        sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
            .bind(u64_to_i64(now_secs()))
            .bind(&id)
            .execute(&self.pool)
            .await?;
        let id = TokenId(Uuid::parse_str(&id).map_err(|e| StoreError::CorruptId(e.to_string()))?);
        Ok(Some(VerifiedToken { id, principal }))
    }

    /// Revoke a token by id. Returns `true` if a row was updated, `false` if
    /// no such token exists. Idempotent: revoking an already-revoked token
    /// leaves its original `revoked_at` untouched and returns `false`.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    pub async fn revoke(&self, id: TokenId) -> Result<bool, StoreError> {
        let affected =
            sqlx::query("UPDATE api_tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
                .bind(u64_to_i64(now_secs()))
                .bind(id.0.to_string())
                .execute(&self.pool)
                .await?
                .rows_affected();
        Ok(affected > 0)
    }

    /// Every token, newest first, with computed spend. Drives the studio
    /// list. Reads two indexed sums per token — fine for the token counts a
    /// single instance issues.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn list(&self) -> Result<Vec<TokenRecord>, StoreError> {
        self.list_at(now_secs()).await
    }

    async fn list_at(&self, now: u64) -> Result<Vec<TokenRecord>, StoreError> {
        let rows: Vec<TokenRow> = sqlx::query_as::<_, TokenRow>(
            "SELECT budget_kind, budget_micro_usd, created_at, id, label, last_used_at, \
                    principal, revoked_at \
             FROM api_tokens ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let month_start = month_start_secs(now);
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let id = TokenId(
                Uuid::parse_str(&row.id).map_err(|e| StoreError::CorruptId(e.to_string()))?,
            );
            let budget = Budget::from_db(&row.budget_kind, row.budget_micro_usd);
            let spend_micro_usd = self.spend_since(id, 0).await?;
            let period_spend_micro_usd = match budget {
                Budget::Monthly { .. } => self.spend_since(id, month_start).await?,
                Budget::Total { .. } | Budget::Unlimited => spend_micro_usd,
            };
            records.push(TokenRecord {
                budget,
                created_at: coulisse_core::i64_to_u64(row.created_at),
                id,
                label: row.label,
                last_used_at: row.last_used_at.map(coulisse_core::i64_to_u64),
                period_spend_micro_usd,
                principal: row.principal,
                revoked_at: row.revoked_at.map(coulisse_core::i64_to_u64),
                spend_micro_usd,
            });
        }
        Ok(records)
    }

    /// Charge `micro_usd` to a token's spend ledger. No-op for non-positive
    /// amounts (a free model, or a pricing miss). Called after each LLM
    /// round-trip on the request-flow's finalize path.
    ///
    /// # Errors
    ///
    /// Returns an error if the insert fails.
    pub async fn record_spend(&self, id: TokenId, micro_usd: i64) -> Result<(), StoreError> {
        self.record_spend_at(id, micro_usd, now_secs()).await
    }

    async fn record_spend_at(
        &self,
        id: TokenId,
        micro_usd: i64,
        at: u64,
    ) -> Result<(), StoreError> {
        if micro_usd <= 0 {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO token_usage (cost_micro_usd, created_at, id, token_id) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(micro_usd)
        .bind(u64_to_i64(at))
        .bind(Uuid::new_v4().to_string())
        .bind(id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reject the request if the token has met or exceeded its budget. Returns
    /// `Ok(())` for unlimited tokens and for capped tokens still under their
    /// limit. Called before the LLM round-trip, alongside the rate-limit
    /// check.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError::Exceeded`] when the cap is reached, or
    /// [`BudgetError::Store`] if the spend lookup fails.
    pub async fn check_budget(&self, id: TokenId) -> Result<(), BudgetError> {
        self.check_budget_at(id, now_secs()).await
    }

    async fn check_budget_at(&self, id: TokenId, now: u64) -> Result<(), BudgetError> {
        let row: Option<(String, Option<i64>)> = sqlx::query_as(
            "SELECT budget_kind, budget_micro_usd FROM api_tokens \
             WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(id.0.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Sqlx)?;
        // No row (unknown or revoked) is not this check's job to reject — the
        // verify step already gates that. Treat as no budget.
        let Some((kind, limit)) = row else {
            return Ok(());
        };
        let (since, limit_micro_usd) = match Budget::from_db(&kind, limit) {
            Budget::Monthly { limit_micro_usd } => (month_start_secs(now), limit_micro_usd),
            Budget::Total { limit_micro_usd } => (0, limit_micro_usd),
            Budget::Unlimited => return Ok(()),
        };
        let spent = self.spend_since(id, since).await?;
        if spent >= limit_micro_usd {
            return Err(BudgetError::Exceeded {
                limit_micro_usd,
                spent_micro_usd: spent,
            });
        }
        Ok(())
    }

    async fn spend_since(&self, id: TokenId, since: u64) -> Result<i64, StoreError> {
        let total: Option<i64> = sqlx::query_scalar(
            "SELECT SUM(cost_micro_usd) FROM token_usage \
             WHERE token_id = ? AND created_at >= ?",
        )
        .bind(id.0.to_string())
        .bind(u64_to_i64(since))
        .fetch_one(&self.pool)
        .await?;
        Ok(total.unwrap_or(0))
    }
}

/// Raw `api_tokens` row, before spend is computed.
#[derive(sqlx::FromRow)]
struct TokenRow {
    budget_kind: String,
    budget_micro_usd: Option<i64>,
    created_at: i64,
    id: String,
    label: String,
    last_used_at: Option<i64>,
    principal: String,
    revoked_at: Option<i64>,
}

/// First instant (UTC) of the calendar month containing `now`.
fn month_start_secs(now: u64) -> u64 {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(u64_to_i64(now)) else {
        return 0;
    };
    let Ok(first) = Date::from_calendar_date(dt.year(), dt.month(), 1) else {
        return 0;
    };
    coulisse_core::i64_to_u64(first.midnight().assume_utc().unix_timestamp())
}

/// `sk-coulisse-<43-char base64url>` — 256 bits of entropy, URL-safe so it
/// pastes cleanly into env files and headers.
fn generate_secret() -> String {
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    format!("sk-coulisse-{encoded}")
}

/// SHA-256 hex digest of a secret. The secret is high-entropy, so a fast
/// cryptographic hash is sufficient — no need for a password-style KDF.
fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    hex::encode(digest)
}

fn require_positive(limit_usd: Option<f64>) -> Result<i64, BudgetParseError> {
    match limit_usd {
        Some(usd) if usd > 0.0 && usd.is_finite() => Ok(usd_to_micro(usd)),
        _ => Err(BudgetParseError::NonPositiveLimit),
    }
}

// Budget amounts are dollar values an operator types; `* 1e6` stays far
// inside i64 range, so the truncation/precision lints don't apply in practice.
#[allow(clippy::cast_possible_truncation)]
fn usd_to_micro(usd: f64) -> i64 {
    (usd * 1_000_000.0).round() as i64
}

#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn micro_to_usd(micro: i64) -> f64 {
    micro as f64 / 1_000_000.0
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("stored token id is not a valid uuid: {0}")]
    CorruptId(String),
    #[error("auth schema migration failed: {0}")]
    Migrate(#[from] migrate::MigrateError),
    #[error("auth database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Debug, Error)]
pub enum BudgetError {
    #[error(
        "token budget exceeded: spent ${spent:.2} of ${limit:.2}",
        spent = micro_to_usd(*spent_micro_usd),
        limit = micro_to_usd(*limit_micro_usd),
    )]
    Exceeded {
        limit_micro_usd: i64,
        spent_micro_usd: i64,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Error)]
pub enum BudgetParseError {
    #[error("budget amount must be a positive dollar value")]
    NonPositiveLimit,
    #[error("unknown budget kind '{0}' (expected unlimited, total, or monthly)")]
    UnknownKind(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;

    async fn store() -> TokenStore {
        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(options).await.unwrap();
        TokenStore::open(pool).await.unwrap()
    }

    #[tokio::test]
    async fn mint_then_verify_roundtrips_principal() {
        let store = store().await;
        let minted = store
            .mint("laptop", "alice", Budget::Unlimited)
            .await
            .unwrap();
        assert!(minted.secret.starts_with("sk-coulisse-"));
        let verified = store.verify(&minted.secret).await.unwrap().unwrap();
        assert_eq!(verified.id, minted.id);
        assert_eq!(verified.principal, "alice");
    }

    #[tokio::test]
    async fn unknown_secret_does_not_verify() {
        let store = store().await;
        store
            .mint("laptop", "alice", Budget::Unlimited)
            .await
            .unwrap();
        assert!(store.verify("sk-coulisse-nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn revoked_token_stops_verifying() {
        let store = store().await;
        let minted = store
            .mint("laptop", "alice", Budget::Unlimited)
            .await
            .unwrap();
        assert!(store.revoke(minted.id).await.unwrap());
        assert!(store.verify(&minted.secret).await.unwrap().is_none());
        // Second revoke is a no-op.
        assert!(!store.revoke(minted.id).await.unwrap());
    }

    #[tokio::test]
    async fn unlimited_budget_never_blocks() {
        let store = store().await;
        let minted = store
            .mint("laptop", "alice", Budget::Unlimited)
            .await
            .unwrap();
        store.record_spend(minted.id, 999_000_000).await.unwrap();
        store.check_budget(minted.id).await.unwrap();
    }

    #[tokio::test]
    async fn total_budget_blocks_at_limit() {
        let store = store().await;
        let minted = store
            .mint(
                "laptop",
                "alice",
                Budget::Total {
                    limit_micro_usd: 5_000_000,
                },
            )
            .await
            .unwrap();
        store.record_spend(minted.id, 4_000_000).await.unwrap();
        store.check_budget(minted.id).await.unwrap();
        store.record_spend(minted.id, 1_000_000).await.unwrap();
        let err = store.check_budget(minted.id).await.unwrap_err();
        assert!(matches!(err, BudgetError::Exceeded { .. }));
    }

    #[tokio::test]
    async fn monthly_budget_only_counts_current_month() {
        let store = store().await;
        let minted = store
            .mint(
                "laptop",
                "alice",
                Budget::Monthly {
                    limit_micro_usd: 5_000_000,
                },
            )
            .await
            .unwrap();
        // 2024-06-15 12:00:00 UTC and a charge in the prior month.
        let now: u64 = 1_718_452_800;
        let last_month: u64 = 1_716_000_000; // mid-May 2024
        store
            .record_spend_at(minted.id, 9_000_000, last_month)
            .await
            .unwrap();
        // Prior-month spend doesn't count against this month's cap.
        store.check_budget_at(minted.id, now).await.unwrap();
        store
            .record_spend_at(minted.id, 6_000_000, now)
            .await
            .unwrap();
        let err = store.check_budget_at(minted.id, now).await.unwrap_err();
        assert!(matches!(err, BudgetError::Exceeded { .. }));
    }

    #[tokio::test]
    async fn list_reports_lifetime_spend_newest_first() {
        let store = store().await;
        let a = store.mint("a", "alice", Budget::Unlimited).await.unwrap();
        let b = store.mint("b", "bob", Budget::Unlimited).await.unwrap();
        store.record_spend(a.id, 2_500_000).await.unwrap();
        let records = store.list().await.unwrap();
        assert_eq!(records.len(), 2);
        let alice = records.iter().find(|r| r.id == a.id).unwrap();
        assert!((alice.spend_usd() - 2.5).abs() < 1e-9);
        let bob = records.iter().find(|r| r.id == b.id).unwrap();
        assert!(bob.spend_usd().abs() < 1e-9);
    }
}
