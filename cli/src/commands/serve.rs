//! Foreground server. The body of what `coulisse start --foreground`
//! (and the bare `coulisse` invocation) executes — this is also the
//! process the detached `start` re-spawns into.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agents::{Agents, BootConfig, DynamicAgents, RigAgents};
use arc_swap::ArcSwap;
use auth::Auth;
use axum::Router;
use axum::middleware::from_fn;
use axum::response::Redirect;
use axum::routing::get;
use coulisse_core::{AgentResolver, ScoreLookup};
use experiments::{ExperimentResolver, ExperimentRouter, Experiments};
use judges::{Judge, JudgeConfig, Judges};
use limits::Tracker;
use mcp::McpServers;
use memory::{BackendConfig, EmbedderConfig, Extractor, Store};
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

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub async fn run(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_path(config_path)?;
    let auth = Auth::from_config(config.auth.clone()).await?;

    // Warm the vendored LiteLLM pricing table so the first chat
    // completion doesn't pay for ~9k JSON entries on the request
    // path. Off the request path; one-shot at boot.
    providers::warm_pricing();

    let memory_summary = memory_summary(&config.memory);
    let stores = boot_stores(&config).await?;
    let _telemetry_guard = telemetry::init_subscriber(stores.pool.clone(), &config.telemetry)?;
    let runtime = build_runtime(&config, &stores).await?;
    let proxy_state = build_proxy_state(config.users, &stores, runtime);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8421));
    print_banner(addr, &auth, &config, &stores, &proxy_state, &memory_summary);

    // Lift names that the wiring blocks below still reference.
    let Stores {
        agents_list,
        dynamic_agents,
        experiments_list,
        experiments_store,
        judge_store,
        judges_list,
        memory,
        settings_view,
        smoke_list,
        smoke_store,
        telemetry,
        yaml_agents,
        yaml_experiments,
        yaml_judges,
        yaml_smoke,
        ..
    } = stores;

    // ConfigStore is the single point all YAML edits flow through —
    // admin POSTs, the `PUT /admin/config` handler, hand-edits picked
    // up by the file watcher. The `on_reload` closure is the seam
    // back into the in-memory hot state held by feature crates.
    let on_reload = make_on_reload(ReloadHandles {
        agents_list: Arc::clone(&agents_list),
        dynamic_agents: Arc::clone(&dynamic_agents),
        experiments_list: Arc::clone(&experiments_list),
        experiments_store: Arc::clone(&experiments_store),
        judge_store: Arc::clone(&judge_store),
        judges_list: Arc::clone(&judges_list),
        settings_view: Arc::clone(&settings_view),
        smoke_list: Arc::clone(&smoke_list),
        smoke_store: Arc::clone(&smoke_store),
        yaml_agents: Arc::clone(&yaml_agents),
        yaml_experiments: Arc::clone(&yaml_experiments),
        yaml_judges: Arc::clone(&yaml_judges),
        yaml_smoke: Arc::clone(&yaml_smoke),
    });
    let config_path_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| PathBuf::from(config_path));
    let config_store = Arc::new(ConfigStore::new(config_path_abs, config.clone(), on_reload));
    let _watcher_guard = config_store.spawn_watcher()?;

    let admin_router = auth.wrap_admin(build_admin_router(AdminWiring {
        agents_list: Arc::clone(&agents_list),
        config_store: Arc::clone(&config_store),
        dynamic_agents: Arc::clone(&dynamic_agents),
        experiments_list: Arc::clone(&experiments_list),
        experiments_store: Arc::clone(&experiments_store),
        judge_store: Arc::clone(&judge_store),
        judges_list: Arc::clone(&judges_list),
        memory: Arc::clone(&memory),
        proxy_state: Arc::clone(&proxy_state),
        settings_view,
        smoke_list: Arc::clone(&smoke_list),
        smoke_store: Arc::clone(&smoke_store),
        telemetry,
        yaml_agents,
        yaml_experiments,
        yaml_judges,
        yaml_smoke,
    }));
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

/// Persistent stores opened against the shared `SQLite` pool, plus the
/// hot-reloadable arc-swap lists each feature crate watches.
struct Stores {
    agents_list: agents::AgentList,
    dynamic_agents: Arc<DynamicAgents>,
    experiments_list: experiments::ExperimentList,
    experiments_store: Arc<Experiments>,
    judge_store: Arc<Judges>,
    judges_list: judges::JudgeList,
    memory: Arc<Store>,
    pool: memory::SqlitePool,
    settings_view: crate::admin::SettingsHandle,
    smoke_list: smoke::SmokeList,
    smoke_store: Arc<SmokeStore>,
    telemetry: Arc<TelemetrySink>,
    yaml_agents: agents::AgentList,
    yaml_experiments: experiments::ExperimentList,
    yaml_judges: judges::JudgeList,
    yaml_smoke: smoke::SmokeList,
}

/// Open one `SQLite` pool, every per-feature store, and reconcile each
/// store with the YAML it was given. Each crate runs its own schema
/// migrations against the shared pool — table ownership is per-crate,
/// the connection is shared so operators back up one file.
async fn boot_stores(config: &Config) -> Result<Stores, Box<dyn std::error::Error>> {
    // The merged effective list (`agents_list`) is what the runtime
    // resolves against; `yaml_agents` is the raw YAML view, kept
    // alongside so the admin layer can compute source labels and the
    // smart DELETE handler can decide tombstone-vs-physical-delete.
    let agents_list = agents::agent_list(config.agents.clone());
    let yaml_agents = agents::agent_list(config.agents.clone());
    let judges_list = judges::judge_list(config.judges.clone());
    let yaml_judges = judges::judge_list(config.judges.clone());
    let experiments_list = experiments::experiment_list(config.experiments.clone());
    let yaml_experiments = experiments::experiment_list(config.experiments.clone());
    let smoke_list = smoke::smoke_list(config.smoke_tests.clone());
    let yaml_smoke = smoke::smoke_list(config.smoke_tests.clone());
    let settings_view = Arc::new(ArcSwap::from_pointee(
        crate::admin::SettingsView::from_config(config),
    ));

    let pool = memory::open_pool(&config.memory.backend).await?;
    let dynamic_agents = Arc::new(DynamicAgents::open(pool.clone()).await?);
    let report = dynamic_agents.rebuild(&agents_list, &config.agents).await?;
    log_agents_merge(&report);

    let memory = Arc::new(
        Store::open(
            pool.clone(),
            config.memory.clone(),
            embedder_fallback_key(config).as_deref(),
        )
        .await?,
    );

    let telemetry = Arc::new(TelemetrySink::open(pool.clone()).await?);
    let judge_store = Arc::new(Judges::open(pool.clone()).await?);
    let report = judge_store
        .rebuild_judges(&judges_list, &config.judges)
        .await?;
    log_judges_merge(&report);

    let smoke_store = Arc::new(SmokeStore::open(pool.clone()).await?);
    let report = smoke_store
        .rebuild_smoke(&smoke_list, &config.smoke_tests)
        .await?;
    log_smoke_merge(&report);

    let experiments_store = Arc::new(Experiments::open(pool.clone()).await?);
    let report = experiments_store
        .rebuild(&experiments_list, &config.experiments)
        .await?;
    log_experiments_merge(&report);

    Ok(Stores {
        agents_list,
        dynamic_agents,
        experiments_list,
        experiments_store,
        judge_store,
        judges_list,
        memory,
        pool,
        settings_view,
        smoke_list,
        smoke_store,
        telemetry,
        yaml_agents,
        yaml_experiments,
        yaml_judges,
        yaml_smoke,
    })
}

fn log_agents_merge(report: &agents::MergeReport) {
    tracing::info!(
        yaml = report.yaml_count,
        overrides = report.override_count,
        dynamic = report.dynamic_count,
        tombstones = report.tombstone_count,
        "agents merged",
    );
}

fn log_judges_merge(report: &judges::MergeReport) {
    tracing::info!(
        yaml = report.yaml_count,
        overrides = report.override_count,
        dynamic = report.dynamic_count,
        tombstones = report.tombstone_count,
        "judges merged",
    );
}

fn log_smoke_merge(report: &smoke::MergeReport) {
    tracing::info!(
        yaml = report.yaml_count,
        overrides = report.override_count,
        dynamic = report.dynamic_count,
        tombstones = report.tombstone_count,
        "smoke tests merged",
    );
}

fn log_experiments_merge(report: &experiments::MergeReport) {
    tracing::info!(
        yaml = report.yaml_count,
        overrides = report.override_count,
        dynamic = report.dynamic_count,
        tombstones = report.tombstone_count,
        "experiments merged",
    );
}

/// Long-lived runtime objects derived from the configured stores.
struct Runtime {
    experiments: Arc<ExperimentRouter>,
    extractor: Option<Arc<Extractor>>,
    judges: HashMap<String, Arc<Judge>>,
    prompter: Arc<RigAgents>,
    tracker: Tracker,
}

async fn build_runtime(
    config: &Config,
    stores: &Stores,
) -> Result<Runtime, Box<dyn std::error::Error>> {
    // Build runtime Judge objects from the merged list (DB shadows + YAML)
    // so DB-only judges are usable from the moment they're created. The
    // HashMap itself is rebuilt only at boot — runtime hot-reload of the
    // Judge instances is a follow-up.
    let judges = build_judges(&stores.judges_list.load())?;
    let mcp = Arc::new(McpServers::connect(config.mcp.clone()).await?);
    let experiments = Arc::new(ExperimentRouter::new(
        stores.experiments_list.load().to_vec(),
    ));
    let resolver: Arc<dyn AgentResolver> = Arc::new(ExperimentResolver::new(
        Arc::clone(&experiments),
        Some(Arc::clone(&stores.judge_store) as Arc<dyn ScoreLookup>),
    ));
    let prompter = Arc::new(RigAgents::new(BootConfig {
        agents: Arc::clone(&stores.agents_list),
        mcp,
        providers: config.providers.clone(),
        resolver,
    })?);
    let extractor = config
        .memory
        .extractor
        .as_ref()
        .map(|cfg| Arc::new(Extractor::new(cfg.clone(), Arc::clone(&prompter) as _)));
    let tracker = Tracker::open(stores.pool.clone()).await?;
    Ok(Runtime {
        experiments,
        extractor,
        judges,
        prompter,
        tracker,
    })
}

fn build_proxy_state(
    users: crate::config::Users,
    stores: &Stores,
    runtime: Runtime,
) -> Arc<AppState<RigAgents>> {
    Arc::new(AppState {
        agents: runtime.prompter,
        experiments: runtime.experiments,
        extractor: runtime.extractor,
        judges: Arc::new(runtime.judges),
        judge_store: Arc::clone(&stores.judge_store),
        memory: Arc::clone(&stores.memory),
        tracker: runtime.tracker,
        users,
    })
}

fn print_banner(
    addr: SocketAddr,
    auth: &Auth,
    config: &Config,
    stores: &Stores,
    proxy_state: &AppState<RigAgents>,
    memory_summary: &str,
) {
    let agent_snapshot = proxy_state.agents.agents();
    let judges_snapshot = stores.judges_list.load();
    let experiments_snapshot = stores.experiments_list.load();
    Banner {
        addr,
        agents: &agent_snapshot,
        auth,
        experiments: &experiments_snapshot,
        extractor: config.memory.extractor.as_ref(),
        judges: &judges_snapshot,
        memory_summary,
        users: config.users,
    }
    .print();
}

/// The seam back into in-memory hot state for the `ConfigStore`. Every
/// YAML edit (admin POST, `PUT /admin/config`, hand-edit picked up by
/// the file watcher) flows through this closure.
type ReloadHook = Arc<
    dyn Fn(Config) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

/// Handles consumed by [`make_on_reload`]. A struct so the long argument
/// list stays self-documenting at the call site.
struct ReloadHandles {
    agents_list: agents::AgentList,
    dynamic_agents: Arc<DynamicAgents>,
    experiments_list: experiments::ExperimentList,
    experiments_store: Arc<Experiments>,
    judge_store: Arc<Judges>,
    judges_list: judges::JudgeList,
    settings_view: crate::admin::SettingsHandle,
    smoke_list: smoke::SmokeList,
    smoke_store: Arc<SmokeStore>,
    yaml_agents: agents::AgentList,
    yaml_experiments: experiments::ExperimentList,
    yaml_judges: judges::JudgeList,
    yaml_smoke: smoke::SmokeList,
}

fn make_on_reload(handles: ReloadHandles) -> ReloadHook {
    let ReloadHandles {
        agents_list,
        dynamic_agents,
        experiments_list,
        experiments_store,
        judge_store,
        judges_list,
        settings_view,
        smoke_list,
        smoke_store,
        yaml_agents,
        yaml_experiments,
        yaml_judges,
        yaml_smoke,
    } = handles;
    Arc::new(move |cfg: Config| {
        let agents_list = Arc::clone(&agents_list);
        let dynamic_agents = Arc::clone(&dynamic_agents);
        let experiments_store = Arc::clone(&experiments_store);
        let experiments_list = Arc::clone(&experiments_list);
        let judge_store = Arc::clone(&judge_store);
        let judges_list = Arc::clone(&judges_list);
        let settings_view = Arc::clone(&settings_view);
        let smoke_list = Arc::clone(&smoke_list);
        let smoke_store = Arc::clone(&smoke_store);
        let yaml_agents = Arc::clone(&yaml_agents);
        let yaml_experiments = Arc::clone(&yaml_experiments);
        let yaml_judges = Arc::clone(&yaml_judges);
        let yaml_smoke = Arc::clone(&yaml_smoke);
        Box::pin(async move {
            yaml_agents.store(Arc::new(cfg.agents.clone()));
            yaml_judges.store(Arc::new(cfg.judges.clone()));
            yaml_experiments.store(Arc::new(cfg.experiments.clone()));
            yaml_smoke.store(Arc::new(cfg.smoke_tests.clone()));
            settings_view.store(Arc::new(crate::admin::SettingsView::from_config(&cfg)));
            log_rebuild_failure(
                "agents",
                dynamic_agents.rebuild(&agents_list, &cfg.agents).await,
            );
            log_rebuild_failure(
                "judges",
                judge_store.rebuild_judges(&judges_list, &cfg.judges).await,
            );
            log_rebuild_failure(
                "experiments",
                experiments_store
                    .rebuild(&experiments_list, &cfg.experiments)
                    .await,
            );
            log_rebuild_failure(
                "smoke",
                smoke_store
                    .rebuild_smoke(&smoke_list, &cfg.smoke_tests)
                    .await,
            );
        })
    })
}

fn log_rebuild_failure<T, E: std::fmt::Display>(kind: &str, result: Result<T, E>) {
    if let Err(err) = result {
        tracing::warn!(
            error = %err,
            "{kind} rebuild failed during reload; previous list kept",
        );
    }
}

/// Handles consumed by [`build_admin_router`]. As with `ReloadHandles`,
/// a struct so the call site stays readable.
struct AdminWiring {
    agents_list: agents::AgentList,
    config_store: Arc<ConfigStore>,
    dynamic_agents: Arc<DynamicAgents>,
    experiments_list: experiments::ExperimentList,
    experiments_store: Arc<Experiments>,
    judge_store: Arc<Judges>,
    judges_list: judges::JudgeList,
    memory: Arc<Store>,
    proxy_state: Arc<AppState<RigAgents>>,
    settings_view: crate::admin::SettingsHandle,
    smoke_list: smoke::SmokeList,
    smoke_store: Arc<SmokeStore>,
    telemetry: Arc<TelemetrySink>,
    yaml_agents: agents::AgentList,
    yaml_experiments: experiments::ExperimentList,
    yaml_judges: judges::JudgeList,
    yaml_smoke: smoke::SmokeList,
}

/// Compose the admin surface from each feature crate's router.
/// Cross-feature views (e.g. tool calls inside a conversation page) are
/// filled in via htmx fragments so feature crates remain decoupled.
fn build_admin_router(w: AdminWiring) -> Router {
    let smoke_runner: Arc<dyn RunDispatcher> = Arc::new(SmokeRunner {
        configs: Arc::clone(&w.smoke_list),
        state: w.proxy_state,
        store: Arc::clone(&w.smoke_store),
    });
    Router::new()
        .merge(agents::admin::router(
            w.agents_list,
            w.dynamic_agents,
            w.yaml_agents,
        ))
        .merge(crate::admin::config_router(Arc::clone(&w.config_store)))
        .merge(crate::admin_extras::router(Arc::clone(&w.config_store)))
        .merge(crate::openapi::router(w.config_store))
        .merge(experiments::admin::router(
            w.experiments_list,
            w.experiments_store,
            w.yaml_experiments,
        ))
        .merge(judges::admin::router(
            w.judge_store,
            w.judges_list,
            w.yaml_judges,
        ))
        .merge(memory::admin::router(w.memory))
        .merge(smoke::admin::router(
            w.smoke_list,
            w.smoke_store,
            smoke_runner,
            w.yaml_smoke,
        ))
        .merge(telemetry::admin::router(w.telemetry))
        .merge(
            Router::new()
                .route("/settings", get(crate::admin::settings))
                .with_state(w.settings_view),
        )
        .route("/overview", get(crate::admin::overview))
        .route(
            "/",
            get(|| async { Redirect::permanent("/admin/overview") }),
        )
        .layer(from_fn(admin_shell))
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
/// already configured `OpenAI` for completions don't have to repeat the key.
fn embedder_fallback_key(config: &Config) -> Option<String> {
    let kind = match &config.memory.embedder {
        EmbedderConfig::Openai { .. } => ProviderKind::Openai,
        // Hash and Voyage are not completion providers — no fallback applies.
        // For Voyage, the user must set memory.embedder.api_key explicitly.
        EmbedderConfig::Hash { .. } | EmbedderConfig::Voyage { .. } => return None,
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
