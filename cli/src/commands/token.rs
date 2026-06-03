//! `coulisse token` — mint, list, and revoke self-issued API tokens from the
//! command line. Operates directly on the same `SQLite` database the running
//! server uses (WAL mode allows concurrent access), so tokens minted here are
//! live immediately for a server that has `auth.proxy.tokens` enabled.

use std::path::Path;

use auth::{Budget, TokenId, TokenStore};

use crate::config::Config;

#[derive(clap::Subcommand)]
pub enum Action {
    /// Mint a token and print its secret to stdout (shown only once).
    Create {
        /// Human-readable label shown in the studio and `token list`.
        label: String,
        /// Budget kind: `unlimited`, `total` (lifetime cap), or `monthly`
        /// (per-calendar-month cap).
        #[arg(default_value = "unlimited", long)]
        budget: String,
        /// Spend limit in USD. Required for `total`/`monthly`, ignored for
        /// `unlimited`.
        #[arg(long)]
        limit: Option<f64>,
        /// Principal (user id) the token binds to — the identity that
        /// partitions memory, recall, and rate limits.
        #[arg(long)]
        principal: String,
    },
    /// List every token with its budget and spend.
    List,
    /// Revoke a token by id. Clients using it immediately get 401.
    Revoke {
        /// Token id (from `token list` or the studio).
        id: String,
    },
}

/// # Errors
///
/// Returns an error if the config can't be loaded, the database can't be
/// opened, or the requested operation fails.
pub fn run(config_path: &Path, action: &Action) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_async(config_path, action))
}

async fn run_async(config_path: &Path, action: &Action) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_path(config_path)?;
    if config
        .auth
        .proxy
        .as_ref()
        .is_none_or(|scope| scope.tokens.is_none())
    {
        eprintln!(
            "note: auth.proxy.tokens is not set — tokens minted here won't gate /v1 until you enable it"
        );
    }
    let state_dir = crate::secrets::state_dir_for(config_path);
    let memory_config =
        crate::memory_resolve::resolve_memory(&config.memory, &config.providers, &state_dir)?;
    let pool = memory::open_pool(&memory_config.backend).await?;
    let store = TokenStore::open(pool).await?;

    match action {
        Action::Create {
            budget,
            label,
            limit,
            principal,
        } => {
            let budget = Budget::from_parts(budget, *limit)?;
            let minted = store.mint(label, principal, budget).await?;
            // Secret to stdout (capturable), the id/context to stderr so a
            // pipe like `coulisse token create … > key.txt` keeps only the key.
            eprintln!(
                "created token {} for {principal} ({})",
                minted.id,
                budget.describe()
            );
            println!("{}", minted.secret);
        }
        Action::List => {
            let tokens = store.list().await?;
            if tokens.is_empty() {
                println!("no tokens");
                return Ok(());
            }
            for t in tokens {
                let status = if t.is_revoked() { "revoked" } else { "active" };
                println!(
                    "{id}  {status:7}  {label:20}  {budget:18}  spent ${spend:.2}  [{principal}]",
                    budget = t.budget.describe(),
                    id = t.id,
                    label = t.label,
                    principal = t.principal,
                    spend = t.spend_usd(),
                );
            }
        }
        Action::Revoke { id } => {
            let token_id = TokenId::parse(id)?;
            if store.revoke(token_id).await? {
                println!("revoked {id}");
            } else {
                println!("no active token with id {id}");
            }
        }
    }
    Ok(())
}
