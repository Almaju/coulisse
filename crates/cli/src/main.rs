use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use judge::Judge;
use limits::Tracker;
use memory::{BackendConfig, EmbedderConfig, Store, UserId};
use prompter::{AdminConfig, Config, JudgeConfig, Prompter, ProviderKind, RigPrompter};
use server::{AdminAuth, AdminCredentials, AppState, Extractor, OidcRuntime, Server};
use telemetry::Sink as TelemetrySink;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path =
        std::env::var("COULISSE_CONFIG").unwrap_or_else(|_| "coulisse.yaml".to_string());
    let config = Config::from_path(&config_path)?;
    let admin_auth = build_admin_auth(config.admin.as_ref()).await?;
    let default_user_id = config.default_user_id.as_deref().map(UserId::from_string);

    let embedder_fallback_key = embedder_fallback_key(&config);
    let extractor_config = config.memory.extractor.clone();
    let judge_configs = config.judges.clone();
    let memory_summary = memory_summary(&config.memory);
    let store = Store::open(config.memory.clone(), embedder_fallback_key.as_deref()).await?;
    let memory = Arc::new(store);

    let extractor = match extractor_config {
        Some(ref cfg) => Some(Arc::new(Extractor::from_config(cfg)?)),
        None => None,
    };

    let judges = build_judges(&judge_configs)?;

    let telemetry = Arc::new(TelemetrySink::open(memory.pool().clone()).await?);
    let prompter = Arc::new(RigPrompter::new(config, Some(Arc::clone(&telemetry))).await?);
    let tracker = Tracker::open(memory.pool().clone()).await?;
    let state = Arc::new(AppState {
        admin_auth,
        default_user_id,
        extractor,
        judges: Arc::new(judges),
        memory,
        prompter,
        telemetry,
        tracker,
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], 8421));
    println!("coulisse listening on http://{addr}");
    println!("  memory: {memory_summary}");
    match state.admin_auth.as_ref() {
        Some(AdminAuth::Basic(_)) => println!("  admin: basic auth enabled"),
        Some(AdminAuth::Oidc(_)) => println!("  admin: OIDC login enabled"),
        None => println!("  admin: unauthenticated (set `admin.basic` or `admin.oidc`)"),
    }
    if let Some(cfg) = &extractor_config {
        println!(
            "  extractor: {} / {} (dedup_threshold={}, max_facts_per_turn={})",
            cfg.provider, cfg.model, cfg.dedup_threshold, cfg.max_facts_per_turn,
        );
    } else {
        println!("  extractor: disabled (memory only grows via explicit API calls)");
    }
    if judge_configs.is_empty() {
        println!("  judges: none configured");
    } else {
        for cfg in &judge_configs {
            let criteria: Vec<&str> = cfg.rubrics.keys().map(String::as_str).collect();
            println!(
                "  judge: {} ({} / {}, sampling_rate={}, criteria=[{}])",
                cfg.name,
                cfg.provider,
                cfg.model,
                cfg.sampling_rate,
                criteria.join(", "),
            );
        }
    }
    for agent in state.prompter.agents() {
        let judges = if agent.judges.is_empty() {
            String::new()
        } else {
            format!(", judges=[{}]", agent.judges.join(", "))
        };
        println!(
            "  agent: {} (provider={}, model={}{})",
            agent.name,
            agent.provider.as_str(),
            agent.model,
            judges,
        );
    }
    Server::new(addr, state).run().await?;
    Ok(())
}

/// Resolve the YAML `admin` block into a runtime `AdminAuth`. Validation
/// at config load already guarantees that at most one of `basic`/`oidc`
/// is set when the block is present — so this function only needs to
/// pick the branch that exists. OIDC builds an issuer-discovered client;
/// any failure there surfaces as a fatal startup error.
async fn build_admin_auth(
    config: Option<&AdminConfig>,
) -> Result<Option<AdminAuth>, Box<dyn std::error::Error>> {
    let Some(cfg) = config else { return Ok(None) };
    if let Some(basic) = &cfg.basic {
        return Ok(Some(AdminAuth::Basic(AdminCredentials::new(
            basic.username.clone(),
            basic.password.clone(),
        ))));
    }
    if let Some(oidc) = &cfg.oidc {
        let runtime = OidcRuntime::discover(oidc).await?;
        return Ok(Some(AdminAuth::Oidc(Box::new(runtime))));
    }
    Ok(None)
}

fn build_judges(
    configs: &[JudgeConfig],
) -> Result<HashMap<String, Arc<Judge>>, judge::JudgeBuildError> {
    let mut out = HashMap::with_capacity(configs.len());
    for cfg in configs {
        let judge = Judge::from_config(cfg)?;
        out.insert(cfg.name.clone(), Arc::new(judge));
    }
    Ok(out)
}

/// Derive an API key to use when the memory embedder config doesn't carry
/// its own. Looks up the matching top-level provider entry so users who
/// already configured OpenAI for completions don't have to repeat the key.
fn embedder_fallback_key(config: &Config) -> Option<String> {
    let kind = match &config.memory.embedder {
        EmbedderConfig::Hash { .. } => return None,
        EmbedderConfig::Openai { .. } => ProviderKind::Openai,
        // Voyage is not a completion provider, so no fallback is possible;
        // the user must set memory.embedder.api_key explicitly.
        EmbedderConfig::Voyage { .. } => return None,
    };
    config.providers.get(&kind).map(|p| p.api_key.clone())
}

fn memory_summary(config: &memory::MemoryConfig) -> String {
    let backend = match &config.backend {
        BackendConfig::InMemory => "in-memory (ephemeral)".to_string(),
        BackendConfig::Sqlite { path } => format!("sqlite at {}", path.display()),
    };
    let embedder = match &config.embedder {
        EmbedderConfig::Hash { dims } => {
            format!("hash (dims={dims}, OFFLINE — no semantic understanding)")
        }
        EmbedderConfig::Openai { model, .. } => format!("openai / {model}"),
        EmbedderConfig::Voyage { model, .. } => format!("voyage / {model}"),
    };
    format!("{backend}; embedder={embedder}")
}
