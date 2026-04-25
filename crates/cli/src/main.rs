use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use agents::{Agents, BootConfig, RigAgents};
use axum::Router;
use axum::response::Redirect;
use axum::routing::get;
use backends::ProviderKind;
use coulisse::config::Config;
use coulisse::server::{self, AppState};
use coulisse_core::ScoreLookup;
use experiments::Strategy;
use judge::{Judge, JudgeConfig, Judges};
use limits::Tracker;
use memory::{BackendConfig, EmbedderConfig, Extractor, Store, UserId};
use studio::{OidcRuntime, StudioAuth, StudioConfig, StudioCredentials, StudioState};
use telemetry::Sink as TelemetrySink;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path =
        std::env::var("COULISSE_CONFIG").unwrap_or_else(|_| "coulisse.yaml".to_string());
    let config = Config::from_path(&config_path)?;
    let studio_auth = build_studio_auth(config.studio.as_ref()).await?;
    let default_user_id = config.default_user_id.as_deref().map(UserId::from_string);

    let embedder_fallback_key = embedder_fallback_key(&config);
    let experiment_configs = config.experiments.clone();
    let extractor_config = config.memory.extractor.clone();
    let judge_configs = config.judges.clone();
    let memory_summary = memory_summary(&config.memory);
    let store = Store::open(config.memory.clone(), embedder_fallback_key.as_deref()).await?;
    let memory = Arc::new(store);

    let judges = build_judges(&judge_configs)?;

    let telemetry = Arc::new(TelemetrySink::open(memory.pool().clone()).await?);
    let judge_store = Arc::new(Judges::open(memory.pool().clone()).await?);
    let prompter = Arc::new(
        RigAgents::new(
            BootConfig {
                agents: config.agents,
                experiments: config.experiments,
                mcp: config.mcp,
                providers: config.providers,
            },
            Some(Arc::clone(&telemetry)),
            Some(Arc::clone(&judge_store) as Arc<dyn ScoreLookup>),
        )
        .await?,
    );

    let extractor = extractor_config
        .as_ref()
        .map(|cfg| Arc::new(Extractor::new(cfg.clone(), Arc::clone(&prompter) as _)));

    let tracker = Tracker::open(memory.pool().clone()).await?;
    let proxy_state = Arc::new(AppState {
        agents: Arc::clone(&prompter),
        default_user_id,
        extractor,
        judges: Arc::new(judges),
        judge_store: Arc::clone(&judge_store),
        memory: Arc::clone(&memory),
        telemetry: Arc::clone(&telemetry),
        tracker,
    });
    let studio_state = Arc::new(StudioState {
        auth: studio_auth,
        experiments: experiment_configs.clone(),
        judges: judge_store,
        memory,
        telemetry,
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], 8421));
    println!("coulisse listening on http://{addr}");
    println!("  memory: {memory_summary}");
    match studio_state.auth.as_ref() {
        Some(StudioAuth::Basic(_)) => println!("  studio: basic auth enabled"),
        Some(StudioAuth::Oidc(_)) => println!("  studio: OIDC login enabled"),
        None => println!("  studio: unauthenticated (set `studio.basic` or `studio.oidc`)"),
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
    for exp in &experiment_configs {
        let variants: Vec<String> = exp
            .variants
            .iter()
            .map(|v| format!("{}@{}", v.agent, v.weight))
            .collect();
        println!(
            "  experiment: {} (strategy={}, sticky_by_user={}, variants=[{}])",
            exp.name,
            match exp.strategy {
                Strategy::Bandit => "bandit",
                Strategy::Shadow => "shadow",
                Strategy::Split => "split",
            },
            exp.sticky_by_user,
            variants.join(", "),
        );
    }
    for agent in proxy_state.agents.agents() {
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

    // axum 0.8 nests asymmetrically: `nest("/studio", ...)` matches the
    // inner `/` route at `/studio`, but a request to `/studio/` returns
    // 404. Redirect the trailing-slash form so bookmarks don't break.
    let app = Router::new()
        .merge(server::router(proxy_state))
        .route("/studio/", get(|| async { Redirect::permanent("/studio") }))
        .nest("/studio", studio::router(studio_state));
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Resolve the YAML `studio` block into a runtime `StudioAuth`. Validation
/// at config load already guarantees that at most one of `basic`/`oidc` is
/// set when the block is present, so this function only needs to pick the
/// branch that exists. OIDC builds an issuer-discovered client; any
/// failure there surfaces as a fatal startup error.
async fn build_studio_auth(
    config: Option<&StudioConfig>,
) -> Result<Option<StudioAuth>, Box<dyn std::error::Error>> {
    let Some(cfg) = config else { return Ok(None) };
    if let Some(basic) = &cfg.basic {
        return Ok(Some(StudioAuth::Basic(StudioCredentials::new(
            basic.username.clone(),
            basic.password.clone(),
        ))));
    }
    if let Some(oidc) = &cfg.oidc {
        let runtime = OidcRuntime::discover(oidc).await?;
        return Ok(Some(StudioAuth::Oidc(Box::new(runtime))));
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
