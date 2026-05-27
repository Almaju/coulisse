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
use coulisse_core::{AgentResolver, ScoreLookup, TaskQueue, TaskStatus};
use experiments::{ExperimentResolver, ExperimentRouter, Experiments};
use judges::{Judge, JudgeConfig, Judges};
use limits::Tracker;
use mcp::{McpServers, OAuthRouterState, TokenVault, VaultMigrator, oauth_router};
use memory::{BackendConfig, EmbedderConfig, Extractor, MemoryConfig, Store, UserId};
use providers::ProviderKind;
use smoke::{RunDispatcher, SmokeStore};
use storage::{BlobBackend, FsBackend, QuotaConfig, Store as FileStore, StorageYaml};
use tasks::Tasks;
use telemetry::Sink as TelemetrySink;
use tokio::net::TcpListener;

use crate::admin::shell as admin_shell;
use crate::banner::Banner;
use crate::config::Config;
use crate::config_store::ConfigStore;
use crate::memory_resolve;
use crate::server::{self, AppState};
use crate::smoke_runner::SmokeRunner;

/// # Errors
///
/// Returns an error if the underlying operation fails.
///
/// # Panics
///
/// Panics if the boot-time task reaper fails (the only way to reach
/// this state is filesystem permission errors on the `SQLite` WAL,
/// which would prevent the server from doing useful work anyway).
#[allow(clippy::too_many_lines)] // top-level wiring; readable as a flat sequence
pub async fn run(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_path(config_path)?;
    let auth = Auth::from_config(config.auth.clone()).await?;
    let default_user_id = config.default_user_id.as_deref().map(UserId::from_string);
    let memory_config = memory_resolve::resolve_memory(&config.memory, &config.providers)?;

    // WHY: warm the vendored LiteLLM pricing table so the first chat
    // completion doesn't pay for ~9k JSON entries on the request path.
    // Off the request path; one-shot at boot.
    providers::warm_pricing();

    let memory_summary = memory_summary(&memory_config);
    let stores = boot_stores(&config, &memory_config).await?;
    let _telemetry_guard = telemetry::init_subscriber(stores.pool.clone(), &config.telemetry)?;
    let runtime = build_runtime(&config, &memory_config, &stores).await?;
    let worker_tasks = Arc::clone(&runtime.tasks);
    let worker_agents = Arc::clone(&runtime.prompter);
    let proxy_state = build_proxy_state(default_user_id, &stores, runtime);
    // Reap `running` tasks left over from a previous process before workers
    // start, so PM sees them as `errored` on the next wakeup instead of
    // believing the work is still in flight. Cutoff = now: any task still
    // in `running` is by definition orphaned.
    let now = coulisse_core::now_secs();
    match TaskStatus::reap_stale_running(
        Arc::clone(&worker_tasks).as_ref(),
        now,
        "process restarted before task completed",
    )
    .await
    {
        Ok(0) => {}
        Ok(n) => tracing::info!(reaped = n, "stale running tasks marked errored"),
        Err(e) => tracing::warn!(%e, "task reap on boot failed; continuing"),
    }
    crate::workers::spawn(Arc::clone(&worker_tasks), worker_agents, 4);
    let trigger_user_id =
        default_user_id.unwrap_or_else(|| coulisse_core::UserId::from_string("cron"));
    triggers::spawn_cron(
        &config.triggers,
        Arc::clone(&worker_tasks) as Arc<dyn coulisse_core::TaskQueue>,
        trigger_user_id,
    );
    triggers::fire_boot(
        &config.triggers,
        Arc::clone(&worker_tasks) as Arc<dyn coulisse_core::TaskQueue>,
        trigger_user_id,
    )
    .await;
    sidecars::spawn_all(&config.sidecars);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port.unwrap_or(8421)));
    print_banner(
        addr,
        &auth,
        &config,
        &memory_config,
        &stores,
        &proxy_state,
        &memory_summary,
    );

    // NOTE: lift names that the wiring blocks below still reference.
    let Stores {
        agents_list,
        dynamic_agents,
        experiments_list,
        experiments_store,
        file_store,
        judge_store,
        judges_list,
        memory,
        mcp_vault,
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

    // NOTE: ConfigStore is the single point all YAML edits flow through —
    // admin POSTs, the `PUT /admin/config` handler, hand-edits picked up
    // by the file watcher. The `on_reload` closure is the seam back into
    // the in-memory hot state held by feature crates.
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
        tasks: Arc::clone(&worker_tasks),
        telemetry,
        yaml_agents,
        yaml_experiments,
        yaml_judges,
        yaml_smoke,
    }));
    let proxy_router = auth.wrap_proxy(server::router(proxy_state));
    let files_router = auth.wrap_proxy(crate::files::router(file_store));

    // Mount OAuth routes outside auth wrappers — they have their own
    // consumer-secret check via the Authorization: Bearer header.
    let oauth_routes = if let Some(vault) = mcp_vault {
        // COULISSE_HMAC_KEY is validated as present at startup (Config::validate);
        // base64-decode it here so the raw key bytes are passed to HMAC.
        let hmac_key = {
            use base64::Engine as _;
            let raw = std::env::var("COULISSE_HMAC_KEY")
                .expect("COULISSE_HMAC_KEY validated present at startup");
            base64::engine::general_purpose::STANDARD
                .decode(raw.trim())
                .expect("COULISSE_HMAC_KEY must be valid base64")
        };
        let consumer_secret = config.auth.mcp_consumer_secret.clone().unwrap_or_default();
        Some(oauth_router(OAuthRouterState {
            configs: config.mcp.clone(),
            consumer_secret,
            hmac_key,
            vault,
        }))
    } else {
        None
    };

    // WHY: axum 0.8 nests asymmetrically — `nest("/admin", ...)` matches
    // the inner `/` route at `/admin`, but a request to `/admin/` returns
    // 404. Redirect the trailing-slash form so bookmarks don't break.
    let mut app = Router::new()
        .merge(proxy_router)
        .merge(files_router)
        .route("/admin/", get(|| async { Redirect::permanent("/admin") }))
        .nest("/admin", admin_router);
    if let Some(oauth) = oauth_routes {
        app = app.merge(oauth);
    }
    // Webhook triggers — `/hooks/<name>` routes declared under `triggers:`.
    // Coulisse stays platform-agnostic: external bridges POST JSON here,
    // we substitute it into the configured prompt template and enqueue.
    app = app.merge(triggers::webhook_router(
        &config.triggers,
        Arc::clone(&worker_tasks) as Arc<dyn coulisse_core::TaskQueue>,
        trigger_user_id,
    ));
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
    file_store: Arc<FileStore>,
    judge_store: Arc<Judges>,
    judges_list: judges::JudgeList,
    memory: Arc<Store>,
    mcp_vault: Option<Arc<TokenVault>>,
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
async fn boot_stores(
    config: &Config,
    memory_config: &MemoryConfig,
) -> Result<Stores, Box<dyn std::error::Error>> {
    // NOTE: the merged effective list (`agents_list`) is what the runtime
    // resolves against; `yaml_agents` is the raw YAML view, kept alongside
    // so the admin layer can compute source labels and the smart DELETE
    // handler can decide tombstone-vs-physical-delete.
    let agents_list = agents::agent_list(config.agents.clone());
    let yaml_agents = agents::agent_list(config.agents.clone());
    let judges_list = judges::judge_list(config.judges.clone());
    let yaml_judges = judges::judge_list(config.judges.clone());
    let experiments_list = experiments::experiment_list(config.experiments.clone());
    let yaml_experiments = experiments::experiment_list(config.experiments.clone());
    let smoke_list = smoke::smoke_list(config.smoke_tests.clone());
    let yaml_smoke = smoke::smoke_list(config.smoke_tests.clone());
    let settings_view = Arc::new(ArcSwap::from_pointee(
        crate::admin::SettingsView::from_config(config, memory_config),
    ));

    let pool = memory::open_pool(&memory_config.backend).await?;

    // Open the MCP token vault if any server has an oauth block.
    let has_oauth = config.mcp.values().any(|c| c.oauth.is_some());
    let mcp_vault = if has_oauth {
        let vault_key = std::env::var("COULISSE_VAULT_KEY").map_err(|_| {
            Box::<dyn std::error::Error>::from(
                "COULISSE_VAULT_KEY env var is required when any MCP server has an oauth block",
            )
        })?;
        coulisse_core::migrate::run(&pool, &VaultMigrator)
            .await
            .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;
        Some(Arc::new(TokenVault::new(pool.clone(), &vault_key)?))
    } else {
        None
    };
    let dynamic_agents = Arc::new(DynamicAgents::open(pool.clone()).await?);
    let report = dynamic_agents.rebuild(&agents_list, &config.agents).await?;
    log_agents_merge(&report);

    let memory = Arc::new(
        Store::open(
            pool.clone(),
            memory_config.clone(),
            embedder_fallback_key(config, memory_config).as_deref(),
        )
        .await?,
    );

    let file_store = Arc::new(open_file_store(pool.clone(), &config.storage).await?);

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
        file_store,
        judge_store,
        judges_list,
        memory,
        mcp_vault,
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

/// Construct the blob backend and open the file store from YAML config.
async fn open_file_store(
    pool: memory::SqlitePool,
    yaml: &StorageYaml,
) -> Result<FileStore, storage::StorageError> {
    let backend = match yaml.backend {
        storage::BackendKind::Fs => {
            let fs = FsBackend::new(&yaml.fs.path).await?;
            BlobBackend::Fs(fs)
        }
        #[cfg(feature = "s3")]
        storage::BackendKind::S3 => {
            let s3_cfg = yaml.s3.as_ref().expect("s3 config required for s3 backend");
            BlobBackend::S3(storage::S3Backend::from_config(s3_cfg).await?)
        }
    };
    FileStore::open(pool, backend, QuotaConfig::from(yaml)).await
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
    tasks: Arc<Tasks>,
    tracker: Tracker,
}

async fn build_runtime(
    config: &Config,
    memory_config: &MemoryConfig,
    stores: &Stores,
) -> Result<Runtime, Box<dyn std::error::Error>> {
    // NOTE: build runtime Judge objects from the merged list (DB shadows +
    // YAML) so DB-only judges are usable from the moment they're created.
    // The HashMap itself is rebuilt only at boot — runtime hot-reload of
    // the Judge instances is a follow-up.
    let judges = build_judges(&stores.judges_list.load())?;
    let mcp = Arc::new(
        McpServers::connect_with_vault(config.mcp.clone(), stores.mcp_vault.clone()).await?,
    );
    let experiments = Arc::new(ExperimentRouter::new(
        stores.experiments_list.load().to_vec(),
    ));
    let resolver: Arc<dyn AgentResolver> = Arc::new(ExperimentResolver::new(
        Arc::clone(&experiments),
        Some(Arc::clone(&stores.judge_store) as Arc<dyn ScoreLookup>),
    ));
    let tasks = Arc::new(Tasks::open(stores.pool.clone()).await?);
    let prompter = Arc::new(RigAgents::new(BootConfig {
        agents: Arc::clone(&stores.agents_list),
        mcp,
        providers: config.providers.clone(),
        resolver,
        task_queue: Some(Arc::clone(&tasks) as Arc<dyn TaskQueue>),
        task_status: Some(Arc::clone(&tasks) as Arc<dyn TaskStatus>),
    })?);
    let extractor = memory_config
        .extractor
        .as_ref()
        .map(|cfg| Arc::new(Extractor::new(cfg.clone(), Arc::clone(&prompter) as _)));
    let tracker = Tracker::open(stores.pool.clone()).await?;
    Ok(Runtime {
        experiments,
        extractor,
        judges,
        prompter,
        tasks,
        tracker,
    })
}

fn build_proxy_state(
    default_user_id: Option<UserId>,
    stores: &Stores,
    runtime: Runtime,
) -> Arc<AppState<RigAgents>> {
    Arc::new(AppState {
        agents: runtime.prompter,
        default_user_id,
        experiments: runtime.experiments,
        extractor: runtime.extractor,
        judges: Arc::new(runtime.judges),
        judge_store: Arc::clone(&stores.judge_store),
        memory: Arc::clone(&stores.memory),
        tracker: runtime.tracker,
    })
}

fn print_banner(
    addr: SocketAddr,
    auth: &Auth,
    _config: &Config,
    memory_config: &MemoryConfig,
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
        extractor: memory_config.extractor.as_ref(),
        judges: &judges_snapshot,
        memory_summary,
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
            // WHY: re-resolve memory config; on failure keep the previous
            // settings view rather than crashing the reload path. The chat
            // path keeps its boot-time Store regardless — memory itself
            // does not hot reload.
            match memory_resolve::resolve_memory(&cfg.memory, &cfg.providers) {
                Err(err) => tracing::warn!(
                    error = %err,
                    "memory config resolution failed during reload; keeping previous settings view",
                ),
                Ok(resolved) => settings_view.store(Arc::new(
                    crate::admin::SettingsView::from_config(&cfg, &resolved),
                )),
            }
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
    tasks: Arc<Tasks>,
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
        .merge(telemetry::admin::router(Arc::clone(&w.telemetry)))
        .merge(crate::admin::live::router(crate::admin::live::State {
            tasks: w.tasks,
            telemetry: w.telemetry,
        }))
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
fn embedder_fallback_key(config: &Config, memory_config: &MemoryConfig) -> Option<String> {
    // WHY: hash and voyage are not completion providers — no fallback
    // applies. For Voyage, the user must set
    // `memory.user_state.embed_with.api_key` explicitly.
    let kind = match &memory_config.embedder {
        EmbedderConfig::Hash { .. } | EmbedderConfig::Voyage { .. } => return None,
        EmbedderConfig::Openai { .. } => ProviderKind::Openai,
    };
    config.providers.get(&kind).map(|p| p.api_key.clone())
}

fn memory_summary(config: &MemoryConfig) -> String {
    let backend = match &config.backend {
        BackendConfig::InMemory => "in-memory (ephemeral)".to_string(),
        BackendConfig::Sqlite { path } => format!("sqlite at {}", path.display()),
    };
    if config.extractor.is_none() && config.recall_k == 0 {
        return format!("{backend}; user_state: disabled (history only)");
    }
    let embedder = match &config.embedder {
        EmbedderConfig::Hash { dims } => {
            format!("hash (dims={dims}, OFFLINE — no semantic understanding)")
        }
        EmbedderConfig::Openai { model, .. } => format!("openai / {model}"),
        EmbedderConfig::Voyage { model, .. } => format!("voyage / {model}"),
    };
    format!("{backend}; embedder={embedder}")
}
