use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use agents::{Agents, BootConfig, RigAgents};
use auth::Auth;
use axum::Router;
use axum::middleware::from_fn;
use axum::response::Redirect;
use axum::routing::get;
use coulisse::admin::shell as admin_shell;
use coulisse::config::Config;
use coulisse::server::{self, AppState};
use coulisse_core::{AgentResolver, ScoreLookup};
use experiments::{ExperimentResolver, ExperimentRouter, Strategy};
use judges::{Judge, JudgeConfig, Judges};
use limits::Tracker;
use mcp::McpServers;
use memory::{BackendConfig, EmbedderConfig, Extractor, Store, UserId};
use providers::ProviderKind;
use telemetry::Sink as TelemetrySink;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path =
        std::env::var("COULISSE_CONFIG").unwrap_or_else(|_| "coulisse.yaml".to_string());
    let config = Config::from_path(&config_path)?;
    let auth = Auth::from_config(config.auth.clone()).await?;
    let default_user_id = config.default_user_id.as_deref().map(UserId::from_string);

    let embedder_fallback_key = embedder_fallback_key(&config);
    let experiment_configs = config.experiments.clone();
    let extractor_config = config.memory.extractor.clone();
    let judge_configs = config.judges.clone();
    let memory_summary = memory_summary(&config.memory);
    // Open one SQLite pool and hand clones to every persistent crate.
    // Each crate runs its own schema migrations against the shared
    // pool — table ownership is per-crate, but the connection is
    // shared so operators back up one file.
    let pool = memory::open_pool(&config.memory.backend).await?;
    let store = Store::open(
        pool.clone(),
        config.memory.clone(),
        embedder_fallback_key.as_deref(),
    )
    .await?;
    let memory = Arc::new(store);

    let judges = build_judges(&judge_configs)?;

    // Apply telemetry schema (creates `events`/`tool_calls` tables) and
    // wire the tracing subscriber: fmt + SqliteLayer (always on by
    // default) plus an optional OTLP exporter when `telemetry.otlp` is
    // set in YAML. The guard keeps the background writer + OTLP
    // provider alive for the process lifetime.
    let telemetry = Arc::new(TelemetrySink::open(pool.clone()).await?);
    let _telemetry_guard = telemetry::init_subscriber(pool.clone(), &config.telemetry)?;
    let judge_store = Arc::new(Judges::open(pool.clone()).await?);
    let mcp = Arc::new(McpServers::connect(config.mcp).await?);
    let experiments = Arc::new(ExperimentRouter::new(config.experiments));
    let resolver: Arc<dyn AgentResolver> = Arc::new(ExperimentResolver::new(
        Arc::clone(&experiments),
        Some(Arc::clone(&judge_store) as Arc<dyn ScoreLookup>),
    ));
    let prompter = Arc::new(RigAgents::new(BootConfig {
        agents: config.agents,
        mcp,
        providers: config.providers,
        resolver,
    })?);

    let extractor = extractor_config
        .as_ref()
        .map(|cfg| Arc::new(Extractor::new(cfg.clone(), Arc::clone(&prompter) as _)));

    let tracker = Tracker::open(pool.clone()).await?;
    let proxy_state = Arc::new(AppState {
        agents: Arc::clone(&prompter),
        default_user_id,
        experiments,
        extractor,
        judges: Arc::new(judges),
        judge_store: Arc::clone(&judge_store),
        memory: Arc::clone(&memory),
        tracker,
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], 8421));
    println!("coulisse listening on http://{addr}");
    println!("  memory: {memory_summary}");
    println!("  proxy auth: {}", auth.proxy_summary());
    println!("  admin auth: {}", auth.admin_summary());
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

    // The admin surface is composed by merging each feature crate's
    // admin router. Cross-feature views (e.g. tool calls inside a
    // conversation page) are filled in via htmx fragments — feature
    // crates remain decoupled and the cli only owns the layout shell
    // and the auth wrapping.
    let experiments_for_admin = Arc::new(experiment_configs.clone());
    let admin_inner = Router::new()
        .merge(memory::admin::router(Arc::clone(&memory)))
        .merge(telemetry::admin::router(Arc::clone(&telemetry)))
        .merge(judges::admin::router(Arc::clone(&judge_store)))
        .merge(experiments::admin::router(experiments_for_admin))
        .route("/", get(|| async { Redirect::permanent("/admin/users") }))
        .layer(from_fn(admin_shell));
    let admin_router = auth.wrap_admin(admin_inner);
    let proxy_router = auth.wrap_proxy(server::router(proxy_state));

    // axum 0.8 nests asymmetrically: `nest("/admin", ...)` matches the
    // inner `/` route at `/admin`, but a request to `/admin/` returns
    // 404. Redirect the trailing-slash form so bookmarks don't break.
    let app = Router::new()
        .merge(proxy_router)
        .route("/admin/", get(|| async { Redirect::permanent("/admin") }))
        .nest("/admin", admin_router);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_judges(
    configs: &[JudgeConfig],
) -> Result<HashMap<String, Arc<Judge>>, judges::JudgeBuildError> {
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
