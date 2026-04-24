use std::net::SocketAddr;
use std::sync::Arc;

use limits::Tracker;
use memory::{BackendConfig, EmbedderConfig, Store, UserId};
use prompter::{Config, Prompter, ProviderKind, RigPrompter};
use server::{AppState, Extractor, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path =
        std::env::var("COULISSE_CONFIG").unwrap_or_else(|_| "coulisse.yaml".to_string());
    let config = Config::from_path(&config_path)?;
    let default_user_id = config.default_user_id.as_deref().map(UserId::from_string);

    let embedder_fallback_key = embedder_fallback_key(&config);
    let extractor_config = config.memory.extractor.clone();
    let memory_summary = memory_summary(&config.memory);
    let store = Store::open(config.memory.clone(), embedder_fallback_key.as_deref()).await?;
    let memory = Arc::new(store);

    let extractor = match extractor_config {
        Some(ref cfg) => Some(Arc::new(Extractor::from_config(cfg)?)),
        None => None,
    };

    let prompter = Arc::new(RigPrompter::new(config).await?);
    let tracker = Tracker::new();
    let state = Arc::new(AppState {
        default_user_id,
        extractor,
        memory,
        prompter,
        tracker,
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], 8421));
    println!("coulisse listening on http://{addr}");
    println!("  memory: {memory_summary}");
    if let Some(cfg) = &extractor_config {
        println!(
            "  extractor: {} / {} (dedup_threshold={}, max_facts_per_turn={})",
            cfg.provider, cfg.model, cfg.dedup_threshold, cfg.max_facts_per_turn,
        );
    } else {
        println!("  extractor: disabled (memory only grows via explicit API calls)");
    }
    for agent in state.prompter.agents() {
        println!(
            "  agent: {} (provider={}, model={})",
            agent.name,
            agent.provider.as_str(),
            agent.model,
        );
    }
    Server::new(addr, state).run().await?;
    Ok(())
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
