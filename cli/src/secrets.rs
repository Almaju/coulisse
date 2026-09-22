//! Auto-generated, persisted infrastructure secrets for MCP OAuth.
//!
//! Coulisse needs two long-lived 32-byte secrets when an OAuth-enabled MCP
//! server is configured:
//!
//! - **`vault_key`** — encrypts user OAuth tokens at rest in the vault.
//! - **`hmac_key`** — signs the per-user connect links surfaced to LLMs
//!   and the OAuth `state` parameter that round-trips through the
//!   provider.
//!
//! Resolution priority:
//!
//! 1. **Environment variables** (`COULISSE_VAULT_KEY`, `COULISSE_HMAC_KEY`)
//!    — for deployments that inject secrets from a vault/CI/k8s.
//! 2. **`<state_dir>/secrets.env` file** — auto-generated on first boot
//!    and reused on every subsequent start. The state dir is already
//!    `.gitignore`d, and the file is written `0600` so other OS users
//!    can't read it.
//! 3. **Fresh generation** — first run with no env vars and no secrets
//!    file. We mint two 32-byte random values, write the file, and
//!    proceed. Coulisse logs the path so users know where the encryption
//!    material lives. Losing the file bricks every stored OAuth token —
//!    users have to re-authorize each MCP server.
//!
//! Why not just env vars: the MCP docs ask people to copy-paste a YAML
//! snippet. Asking them to *also* export two random base64 strings before
//! `coulisse start` works is exactly the kind of friction the project is
//! trying to eliminate.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const VAULT_KEY_VAR: &str = "COULISSE_VAULT_KEY";
const HMAC_KEY_VAR: &str = "COULISSE_HMAC_KEY";
const SECRETS_FILENAME: &str = "secrets.env";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error(
        "{path} is malformed: expected `KEY=value` lines for COULISSE_VAULT_KEY and COULISSE_HMAC_KEY. Delete the file to regenerate, but note that this will invalidate every stored OAuth token."
    )]
    Malformed { path: String },
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Debug)]
pub struct Secrets {
    pub hmac_key: String,
    pub vault_key: String,
}

/// The two keys as the environment supplies them. Both must be present
/// for the environment to win; a lone key falls through to the file.
#[derive(Clone, Debug, Default)]
pub struct EnvKeys {
    pub hmac_key: Option<String>,
    pub vault_key: Option<String>,
}

impl EnvKeys {
    /// Read `COULISSE_VAULT_KEY` / `COULISSE_HMAC_KEY` from the process
    /// environment. Called once, at boot, before any worker threads exist.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            hmac_key: std::env::var(HMAC_KEY_VAR).ok(),
            vault_key: std::env::var(VAULT_KEY_VAR).ok(),
        }
    }
}

impl Secrets {
    // rabot: allow(ambient-randomness) cryptographic secret: a replayable generator would be a vulnerability
    fn generate() -> Self {
        let mut vault = [0u8; 32];
        rand::rng().fill_bytes(&mut vault);
        let mut hmac = [0u8; 32];
        rand::rng().fill_bytes(&mut hmac);
        Self {
            hmac_key: B64.encode(hmac),
            vault_key: B64.encode(vault),
        }
    }

    fn parse_file(path: &Path) -> Result<Self, SecretsError> {
        let contents = fs::read_to_string(path).map_err(|source| SecretsError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let mut vault_key: Option<String> = None;
        let mut hmac_key: Option<String> = None;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim().trim_matches('"').to_string();
            match key {
                VAULT_KEY_VAR => vault_key = Some(value),
                HMAC_KEY_VAR => hmac_key = Some(value),
                _ => {}
            }
        }
        match (vault_key, hmac_key) {
            (Some(vault_key), Some(hmac_key)) => Ok(Self {
                hmac_key,
                vault_key,
            }),
            _ => Err(SecretsError::Malformed {
                path: path.display().to_string(),
            }),
        }
    }

    /// Resolve secrets for this Coulisse instance with the env > file >
    /// generate priority. `env` is read once at boot by
    /// [`EnvKeys::from_env`]; tests pass injected values, so no test ever
    /// mutates the process environment.
    ///
    /// # Errors
    ///
    /// Returns `SecretsError` if the secrets file exists but is
    /// unreadable / malformed, or if writing a freshly generated file
    /// fails.
    pub fn resolve(state_dir: &Path, env: EnvKeys) -> Result<Self, SecretsError> {
        if let (Some(vault_key), Some(hmac_key)) = (env.vault_key, env.hmac_key) {
            return Ok(Self {
                hmac_key,
                vault_key,
            });
        }

        let path = state_dir.join(SECRETS_FILENAME);
        if path.exists() {
            return Self::parse_file(&path);
        }

        let secrets = Self::generate();
        fs::create_dir_all(state_dir).map_err(|source| SecretsError::Write {
            path: state_dir.display().to_string(),
            source,
        })?;
        secrets.write_file(&path)?;
        tracing::info!(
            path = %path.display(),
            "generated MCP OAuth encryption keys at first boot — \
             back this file up; losing it invalidates every stored token"
        );
        Ok(secrets)
    }

    #[cfg(unix)]
    fn write_file(&self, path: &Path) -> Result<(), SecretsError> {
        use std::io::Write;

        let body = format!(
            "# Auto-generated by coulisse on first boot. Encryption material for\n\
             # MCP per-user OAuth tokens — back this file up. Losing it makes every\n\
             # token in `mcp_oauth_tokens` unrecoverable (users have to re-authorize\n\
             # each connected MCP server).\n\
             #\n\
             # You can override these by exporting COULISSE_VAULT_KEY and\n\
             # COULISSE_HMAC_KEY as environment variables; env wins over this file.\n\
             {VAULT_KEY_VAR}={}\n\
             {HMAC_KEY_VAR}={}\n",
            self.vault_key, self.hmac_key,
        );

        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|source| SecretsError::Write {
                path: path.display().to_string(),
                source,
            })?;
        file.write_all(body.as_bytes())
            .map_err(|source| SecretsError::Write {
                path: path.display().to_string(),
                source,
            })?;
        // Belt and suspenders: if the file pre-existed (race), re-assert perms.
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        Ok(())
    }

    #[cfg(not(unix))]
    fn write_file(&self, path: &Path) -> Result<(), SecretsError> {
        let body = format!(
            "{VAULT_KEY_VAR}={}\n{HMAC_KEY_VAR}={}\n",
            self.vault_key, self.hmac_key,
        );
        fs::write(path, body).map_err(|source| SecretsError::Write {
            path: path.display().to_string(),
            source,
        })
    }
}

#[must_use]
pub fn state_dir_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join(".coulisse")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn env_values_win_over_file() {
        let dir = TempDir::new().unwrap();
        // Even with a file present, injected env values should win.
        std::fs::write(
            dir.path().join(SECRETS_FILENAME),
            format!("{VAULT_KEY_VAR}=file-vault\n{HMAC_KEY_VAR}=file-hmac\n"),
        )
        .unwrap();
        let env = EnvKeys {
            hmac_key: Some("env-hmac".into()),
            vault_key: Some("env-vault".into()),
        };
        let secrets = Secrets::resolve(dir.path(), env).unwrap();
        assert_eq!(secrets.vault_key, "env-vault");
        assert_eq!(secrets.hmac_key, "env-hmac");
    }

    #[test]
    fn partial_env_falls_through_to_file_or_generation() {
        // Only the vault env set (not the hmac one) → fall through to
        // file or generation, don't pick up just the vault half.
        let dir = TempDir::new().unwrap();
        let env = EnvKeys {
            hmac_key: None,
            vault_key: Some("env-vault".into()),
        };
        let secrets = Secrets::resolve(dir.path(), env).unwrap();
        assert_ne!(secrets.vault_key, "env-vault");
    }

    #[test]
    fn first_boot_generates_and_persists() {
        let dir = TempDir::new().unwrap();
        let first = Secrets::resolve(dir.path(), EnvKeys::default()).unwrap();
        // Same dir, second call — should read the file, not regenerate.
        let second = Secrets::resolve(dir.path(), EnvKeys::default()).unwrap();
        assert_eq!(first.vault_key, second.vault_key);
        assert_eq!(first.hmac_key, second.hmac_key);
        // Both are valid base64 of 32 bytes.
        let vault_bytes = B64.decode(&first.vault_key).expect("base64");
        let hmac_bytes = B64.decode(&first.hmac_key).expect("base64");
        assert_eq!(vault_bytes.len(), 32);
        assert_eq!(hmac_bytes.len(), 32);
    }

    #[test]
    fn generated_secrets_are_distinct() {
        // The two keys must be independent random material; a bug that
        // accidentally uses the same RNG draw for both would still pass
        // size checks, so guard against it explicitly.
        let dir = TempDir::new().unwrap();
        let secrets = Secrets::resolve(dir.path(), EnvKeys::default()).unwrap();
        assert_ne!(secrets.vault_key, secrets.hmac_key);
    }

    #[test]
    fn malformed_file_surfaces_clear_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(SECRETS_FILENAME), "this is not a key file").unwrap();
        let err = Secrets::resolve(dir.path(), EnvKeys::default()).unwrap_err();
        assert!(matches!(err, SecretsError::Malformed { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn generated_file_is_0600() {
        use std::os::unix::fs::MetadataExt;
        let dir = TempDir::new().unwrap();
        Secrets::resolve(dir.path(), EnvKeys::default()).unwrap();
        let meta = std::fs::metadata(dir.path().join(SECRETS_FILENAME)).unwrap();
        // mode() returns the full st_mode; mask to permission bits.
        assert_eq!(meta.mode() & 0o777, 0o600);
    }
}
