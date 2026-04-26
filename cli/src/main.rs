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
use coulisse::banner::Banner;
use coulisse::config::Config;
use coulisse::server::{self, AppState};
use coulisse_core::{AgentResolver, ScoreLookup};
use experiments::{ExperimentResolver, ExperimentRouter};
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
    Banner {
        addr,
        agents: proxy_state.agents.agents(),
        auth: &auth,
        experiments: &experiment_configs,
        extractor: extractor_config.as_ref(),
        judges: &judge_configs,
        memory_summary: &memory_summary,
    }
    .print();

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
