// WHY: aes-gcm 0.10 (current stable) uses generic-array 0.x, whose
// `GenericArray` type is deprecated upstream in favor of generic-array
// 1.x. Upgrading would require aes-gcm 0.11 (still a release candidate
// at the time of writing). Suppress at use-sites; revisit when 0.11
// stabilizes.
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use coulisse_core::migrate::SchemaMigrator;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::McpError;

pub const SCHEMA: &str = include_str!("../migrations/schema.sql");

/// Encrypted token pair stored per `(server_name, user_id)`.
#[derive(Debug)]
pub struct StoredToken {
    pub access_token: String,
    pub expires_at: Option<i64>,
    pub refresh_token: Option<String>,
}

/// Token vault backed by the shared SQLite pool. Tokens are stored
/// AES-256-GCM encrypted with a nonce prepended (12 bytes || ciphertext).
pub struct TokenVault {
    cipher: Aes256Gcm,
    pool: SqlitePool,
}

pub struct VaultMigrator;

/// Type alias so callers can reference the migrator by the spec-mandated name.
pub type McpMigrator = VaultMigrator;

impl SchemaMigrator for VaultMigrator {
    const NAME: &'static str = "mcp";
    const SCHEMA: &'static str = SCHEMA;
    const VERSIONS: &'static [&'static str] = &["0.1.0"];

    async fn upgrade_from(
        &self,
        _from_version: &str,
        _conn: &mut SqliteConnection,
    ) -> sqlx::Result<()> {
        unreachable!("only one version exists")
    }
}

impl TokenVault {
    /// Build from the shared pool and a base64-encoded 32-byte key.
    ///
    /// # Errors
    ///
    /// Returns `McpError::VaultKeyInvalid` if the key is not valid base64
    /// or not exactly 32 bytes after decoding.
    pub fn new(pool: SqlitePool, key_b64: &str) -> Result<Self, McpError> {
        let key_bytes = B64
            .decode(key_b64.trim())
            .map_err(|_| McpError::VaultKeyInvalid)?;
        let cipher =
            Aes256Gcm::new_from_slice(&key_bytes).map_err(|_| McpError::VaultKeyInvalid)?;
        Ok(Self { cipher, pool })
    }

    fn encrypt(&self, server: &str, plaintext: &str) -> Result<Vec<u8>, McpError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|err| McpError::Encrypt {
                server: server.to_string(),
                err,
            })?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    #[allow(deprecated)]
    fn decrypt(&self, server: &str, blob: &[u8]) -> Result<String, McpError> {
        let nonce_arr: [u8; 12] =
            blob.get(..12)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| McpError::Decrypt {
                    server: server.to_string(),
                    err: aes_gcm::Error,
                })?;
        let ciphertext = &blob[12..];
        let nonce = aes_gcm::aead::generic_array::GenericArray::from(nonce_arr);
        let plaintext =
            self.cipher
                .decrypt(&nonce, ciphertext)
                .map_err(|err| McpError::Decrypt {
                    server: server.to_string(),
                    err,
                })?;
        String::from_utf8(plaintext).map_err(|_| McpError::Decrypt {
            server: server.to_string(),
            err: aes_gcm::Error,
        })
    }

    /// Upsert a token pair for `(server_name, user_id)`.
    ///
    /// # Errors
    ///
    /// Returns an error if encryption or the database write fails.
    pub async fn upsert_token(
        &self,
        server_name: &str,
        user_id: &str,
        access_token: &str,
        expires_at: Option<i64>,
        refresh_token: Option<&str>,
    ) -> Result<(), McpError> {
        let now = coulisse_core::now_secs() as i64;
        let access_enc = self.encrypt(server_name, access_token)?;
        let refresh_enc = refresh_token
            .map(|rt| self.encrypt(server_name, rt))
            .transpose()?;

        sqlx::query(
            "INSERT INTO mcp_oauth_tokens \
             (access_token_enc, created_at, expires_at, refresh_token_enc, server_name, updated_at, user_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(server_name, user_id) DO UPDATE SET \
               access_token_enc = excluded.access_token_enc, \
               expires_at = excluded.expires_at, \
               refresh_token_enc = excluded.refresh_token_enc, \
               updated_at = excluded.updated_at",
        )
        .bind(access_enc)
        .bind(now)
        .bind(expires_at)
        .bind(refresh_enc)
        .bind(server_name)
        .bind(now)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Retrieve and decrypt the stored token for `(server_name, user_id)`.
    /// Returns `None` if no token has been stored yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read or decryption fails.
    pub async fn get_token(
        &self,
        server_name: &str,
        user_id: &str,
    ) -> Result<Option<StoredToken>, McpError> {
        let row: Option<(Vec<u8>, Option<i64>, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT access_token_enc, expires_at, refresh_token_enc \
             FROM mcp_oauth_tokens \
             WHERE server_name = ? AND user_id = ?",
        )
        .bind(server_name)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some((access_enc, expires_at, refresh_enc)) = row else {
            return Ok(None);
        };

        let access_token = self.decrypt(server_name, &access_enc)?;
        let refresh_token = refresh_enc
            .map(|r| self.decrypt(server_name, &r))
            .transpose()?;

        Ok(Some(StoredToken {
            access_token,
            expires_at,
            refresh_token,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn make_vault() -> TokenVault {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE mcp_oauth_tokens (\
                access_token_enc  BLOB    NOT NULL, \
                created_at        INTEGER NOT NULL, \
                expires_at        INTEGER, \
                refresh_token_enc BLOB, \
                server_name       TEXT    NOT NULL, \
                updated_at        INTEGER NOT NULL, \
                user_id           TEXT    NOT NULL, \
                PRIMARY KEY (server_name, user_id) \
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        // 32 bytes of zeros base64-encoded
        let key = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        TokenVault::new(pool, &key).unwrap()
    }

    #[tokio::test]
    async fn encrypt_decrypt_round_trip() {
        let vault = make_vault().await;
        vault
            .upsert_token(
                "github",
                "user-1",
                "access-abc",
                Some(9999),
                Some("refresh-xyz"),
            )
            .await
            .unwrap();

        let token = vault.get_token("github", "user-1").await.unwrap().unwrap();
        assert_eq!(token.access_token, "access-abc");
        assert_eq!(token.expires_at, Some(9999));
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-xyz"));
    }

    #[tokio::test]
    async fn missing_token_returns_none() {
        let vault = make_vault().await;
        let result = vault.get_token("github", "nobody").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn upsert_updates_existing_token() {
        let vault = make_vault().await;
        vault
            .upsert_token("github", "user-1", "old-access", None, None)
            .await
            .unwrap();
        vault
            .upsert_token(
                "github",
                "user-1",
                "new-access",
                Some(42),
                Some("new-refresh"),
            )
            .await
            .unwrap();

        let token = vault.get_token("github", "user-1").await.unwrap().unwrap();
        assert_eq!(token.access_token, "new-access");
        assert_eq!(token.expires_at, Some(42));
        assert_eq!(token.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[tokio::test]
    async fn invalid_key_length_rejected() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        // Only 16 bytes, not 32
        let short_key = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        let result = TokenVault::new(pool, &short_key);
        assert!(matches!(result, Err(McpError::VaultKeyInvalid)));
    }

    #[tokio::test]
    async fn invalid_base64_rejected() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let result = TokenVault::new(pool, "!!!not-base64!!!");
        assert!(matches!(result, Err(McpError::VaultKeyInvalid)));
    }

    #[tokio::test]
    async fn user_cannot_read_another_users_token() {
        let vault = make_vault().await;
        vault
            .upsert_token("github", "user-1", "secret-token-1", None, None)
            .await
            .unwrap();
        vault
            .upsert_token("github", "user-2", "secret-token-2", None, None)
            .await
            .unwrap();

        let token1 = vault.get_token("github", "user-1").await.unwrap().unwrap();
        let token2 = vault.get_token("github", "user-2").await.unwrap().unwrap();

        assert_eq!(token1.access_token, "secret-token-1");
        assert_eq!(token2.access_token, "secret-token-2");
        assert_ne!(token1.access_token, token2.access_token);
    }
}
