//! Foreground server. The body of what `coulisse start --foreground`
//! (and the bare `coulisse` invocation) executes — this is also the
//! process the detached `start` re-spawns into.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agents::{Agents, BootConfig, RigAgents};
use arc_swap::ArcSwap;
use auth::Auth;
use axum::Router;
use axum::middleware::from_fn;
use axum::response::Redirect;
use axum::routing::get;
use coulisse_core::{AgentResolver, ScoreLookup};
use experiments::{ExperimentResolver, ExperimentRouter};
use judges::{Judge, JudgeConfig, Judges};
use limits::Tracker;
use mcp::McpServers;
use memory::{BackendConfig, EmbedderConfig, Extractor, Store, UserId};
use providers::ProviderKind;
use smoke::{RunDispatcher, SmokeStore};
use telemetry::Sink as TelemetrySink;
use tokio::net::TcpListener;

use crate::admin::shell as admin_shell;
use crate::banner::Banner;
use crate::config::Config;
use crate::config_store::ConfigStore;
use crate::server::{self, AppState};
use crate::smoke_runner::SmokeRunner;

pub async fn run(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_path(config_path)?;
    let auth = Auth::from_config(config.auth.clone()).await?;
    let default_user_id = config.default_user_id.as_deref().map(UserId::from_string);

    // Warm the vendored LiteLLM pricing table so the first chat
    // completion doesn't pay for ~9k JSON entries on the request
    // path. Off the request path; one-shot at boot.
    providers::warm_pricing();

    let embedder_fallback_key = embedder_fallback_key(&config);
    let extractor_config = config.memory.extractor.clone();
    let memory_summary = memory_summary(&config.memory);

    // Hot-reloadable view of every editable section. The same handle
    // is held by feature crates (admin routers, runtime where wired)
    // and by the ConfigStore reload pipeline — file changes (admin
    // save or hand-edit) atomically swap in a fresh snapshot.
    let agents_list = agents::agent_list(config.agents.clone());
    let judges_list = judges::judge_list(config.judges.clone());
    let experiments_list = experiments::experiment_list(config.experiments.clone());
    let smoke_list = smoke::smoke_list(config.smoke_tests.clone());
    let settings_view = Arc::new(ArcSwap::from_pointee(
        crate::admin::SettingsView::from_config(&config),
    ));
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

    let judges = build_judges(&config.judges)?;

    // Apply telemetry schema (creates `events`/`tool_calls` tables) and
    // wire the tracing subscriber: fmt + SqliteLayer (always on by
    // default) plus an optional OTLP exporter when `telemetry.otlp` is
    // set in YAML. The guard keeps the background writer + OTLP
    // provider alive for the process lifetime.
    let telemetry = Arc::new(TelemetrySink::open(pool.clone()).await?);
    let _telemetry_guard = telemetry::init_subscriber(pool.clone(), &config.telemetry)?;
    let judge_store = Arc::new(Judges::open(pool.clone()).await?);
    let smoke_store = Arc::new(SmokeStore::open(pool.clone()).await?);
    let mcp = Arc::new(McpServers::connect(config.mcp.clone()).await?);
    let experiments = Arc::new(ExperimentRouter::new(config.experiments.clone()));
    let resolver: Arc<dyn AgentResolver> = Arc::new(ExperimentResolver::new(
        Arc::clone(&experiments),
        Some(Arc::clone(&judge_store) as Arc<dyn ScoreLookup>),
    ));
    let prompter = Arc::new(RigAgents::new(BootConfig {
        agents: Arc::clone(&agents_list),
        mcp,
        providers: config.providers.clone(),
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
    {
        let agent_snapshot = proxy_state.agents.agents();
        let judges_snapshot = judges_list.load();
        let experiments_snapshot = experiments_list.load();
        Banner {
            addr,
            agents: &agent_snapshot,
            auth: &auth,
            experiments: &experiments_snapshot,
            extractor: extractor_config.as_ref(),
            judges: &judges_snapshot,
            memory_summary: &memory_summary,
        }
        .print();
    }

    // ConfigStore is the single point all YAML edits flow through —
    // admin POSTs, the `PUT /admin/config` handler, hand-edits picked
    // up by the file watcher. The `on_reload` closure is the seam
    // back into the in-memory hot state held by feature crates.
    let on_reload = {
        let agents_list = Arc::clone(&agents_list);
        let judges_list = Arc::clone(&judges_list);
        let experiments_list = Arc::clone(&experiments_list);
        let settings_view = Arc::clone(&settings_view);
        let smoke_list = Arc::clone(&smoke_list);
        Arc::new(move |cfg: &Config| {
            agents_list.store(Arc::new(cfg.agents.clone()));
            judges_list.store(Arc::new(cfg.judges.clone()));
            experiments_list.store(Arc::new(cfg.experiments.clone()));
            settings_view.store(Arc::new(crate::admin::SettingsView::from_config(cfg)));
            smoke_list.store(Arc::new(cfg.smoke_tests.clone()));
        }) as Arc<dyn Fn(&Config) + Send + Sync>
    };
    let config_path_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| PathBuf::from(config_path));
    let config_store = Arc::new(ConfigStore::new(config_path_abs, config.clone(), on_reload));
    let _watcher_guard = config_store.spawn_watcher()?;

    // The admin surface is composed by merging each feature crate's
    // admin router. Cross-feature views (e.g. tool calls inside a
    // conversation page) are filled in via htmx fragments — feature
    // crates remain decoupled and the cli only owns the layout shell
    // and the auth wrapping.
    let persister: Arc<dyn coulisse_core::ConfigPersister> = Arc::clone(&config_store) as _;
    let smoke_runner: Arc<dyn RunDispatcher> = Arc::new(SmokeRunner {
        configs: Arc::clone(&smoke_list),
        state: Arc::clone(&proxy_state),
        store: Arc::clone(&smoke_store),
    });
    let admin_inner = Router::new()
        .merge(agents::admin::router(
            Arc::clone(&agents_list),
            Arc::clone(&persister),
        ))
        .merge(crate::admin::config_router(Arc::clone(&config_store)))
        .merge(crate::admin_extras::router(Arc::clone(&config_store)))
        .merge(crate::openapi::router(Arc::clone(&config_store)))
        .merge(experiments::admin::router(
            Arc::clone(&experiments_list),
            Arc::clone(&persister),
        ))
        .merge(judges::admin::router(
            Arc::clone(&judge_store),
            Arc::clone(&judges_list),
            Arc::clone(&persister),
        ))
        .merge(memory::admin::router(Arc::clone(&memory)))
        .merge(smoke::admin::router(
            Arc::clone(&smoke_list),
            Arc::clone(&smoke_store),
            smoke_runner,
        ))
        .merge(telemetry::admin::router(Arc::clone(&telemetry)))
        .merge(
            Router::new()
                .route("/settings", get(crate::admin::settings))
                .with_state(settings_view),
        )
        .route("/overview", get(crate::admin::overview))
        .route(
            "/",
            get(|| async { Redirect::permanent("/admin/overview") }),
        )
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
