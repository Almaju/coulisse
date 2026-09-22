//! Foreground server. The body of what `coulisse start --foreground`
//! (and the bare `coulisse` invocation) executes — this is also the
//! process the detached `start` re-spawns into.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agents::{Agents, AgentsError, BootConfig, DynamicAgents, DynamicAgentsError, RigAgents};
use arc_swap::ArcSwap;
use auth::{Auth, IdentityMode, TokenStore};
use axum::Router;
use axum::middleware::from_fn;
use axum::response::Redirect;
use axum::routing::get;
use coulisse_core::{
    AgentResolver, BoxFuture, ScoreLookup, SkillCatalog, TaskQueue, TaskStatus, UserId,
};
use experiments::{ExperimentResolver, ExperimentRouter, Experiments};
use judges::{Judge, JudgeConfig, Judges};
use limits::Tracker;
use mcp::{
    ConnectLinkSigner, McpError, McpServers, OAuthRouterState, PublicBaseUrl, TokenVault,
    VaultMigrator,
};
use memory::{BackendConfig, EmbedderConfig, Extractor, MemoryConfig, Store};
use providers::{PricingTable, ProviderKind};
use skills::Skills;
use smoke::{RunDispatcher, SmokeStore};
use storage::{BlobBackend, FsBackend, QuotaConfig, StorageYaml, Store as FileStore};
use tasks::Tasks;
use telemetry::Sink as TelemetrySink;
use tokio::net::TcpListener;
use triggers::Triggers;

use crate::admin::shell as admin_shell;
use crate::banner::Banner;
use crate::config::{Config, ConfigError};
use crate::config_store::ConfigStore;
use crate::error::ServerError;
use crate::files::FilesApi;
use crate::memory_resolve::{MemoryResolveError, MemoryResolver};
use crate::secrets::{EnvKeys, Secrets, SecretsError};
use crate::server::{AppState, Identity};
use crate::smoke_runner::SmokeRunner;
use crate::workers::Workers;

/// Everything that can stop the server from coming up. Each variant is
/// one boot step, so the message names the subsystem that failed.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("agents: {0}")]
    Agents(#[from] AgentsError),
    #[error("auth: {0}")]
    Auth(#[from] auth::BuildError),
    #[error("server.bind: {0}")]
    Bind(#[from] server::BindError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("config watcher: {0}")]
    ConfigWatch(#[from] coulisse_core::ConfigPersistError),
    #[error("dynamic agents: {0}")]
    DynamicAgents(#[from] DynamicAgentsError),
    #[error("experiments: {0}")]
    Experiments(#[from] experiments::ExperimentsError),
    #[error("file storage: {0}")]
    FileStore(#[from] storage::StorageError),
    #[error("judge: {0}")]
    Judge(#[from] judges::JudgeBuildError),
    #[error("judges: {0}")]
    Judges(#[from] judges::JudgeStoreError),
    #[error("rate limits: {0}")]
    Limits(#[from] limits::LimitError),
    #[error("mcp: {0}")]
    Mcp(#[from] McpError),
    #[error("memory: {0}")]
    Memory(#[from] memory::ConfigError),
    #[error(transparent)]
    MemoryResolve(#[from] MemoryResolveError),
    #[error("schema migration: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
    #[error("pricing: {0}")]
    Pricing(#[from] providers::PricingParseError),
    #[error("failed to build the tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error(transparent)]
    Secrets(#[from] SecretsError),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("skills: {0}")]
    Skills(#[from] skills::SkillsError),
    #[error("smoke tests: {0}")]
    Smoke(#[from] smoke::SmokeStoreError),
    #[error("tasks: {0}")]
    Tasks(#[from] tasks::TaskError),
    #[error("telemetry: {0}")]
    Telemetry(#[from] telemetry::InitError),
    #[error("telemetry: {0}")]
    TelemetrySink(#[from] telemetry::TelemetryError),
    #[error("api tokens: {0}")]
    Tokens(#[from] auth::StoreError),
}

/// Parse the config synchronously so the `server:` slice can size the
/// tokio runtime before any async work begins — worker count is fixed
/// once the runtime exists — then serve until the listener closes.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded or any boot step fails.
pub fn run_blocking(config_path: &Path, on_ready: impl FnOnce() + Send) -> Result<(), ServeError> {
    let config = Config::from_path(config_path)?;
    let runtime = config.server.runtime().map_err(ServeError::Runtime)?;
    runtime.block_on(Boot::new(config, config_path).run(on_ready))
}

/// One server boot: the validated config, where it came from, and the
/// state directory (`.coulisse/` next to the YAML) that holds everything
/// Coulisse generates — the `SQLite` database, uploaded files, the MCP
/// secrets file, the detached log/pid. There are no path knobs to point
/// these elsewhere.
struct Boot {
    config: Config,
    config_path: PathBuf,
    state_dir: PathBuf,
}

impl Boot {
    fn new(config: Config, config_path: &Path) -> Self {
        Self {
            config,
            config_path: config_path.to_path_buf(),
            state_dir: crate::secrets::state_dir_for(config_path),
        }
    }

    /// The whole HTTP surface: proxy and files behind the proxy auth
    /// scope, the studio behind the admin scope, OAuth and webhook routes
    /// with their own checks.
    fn app(&self, auth: &Auth, runnable: Runnable<'_>) -> Router {
        let Runnable {
            config_store,
            proxy_state,
            signer,
            stores,
            triggers,
            worker_tasks,
        } = runnable;
        let admin_router = auth.wrap_admin(
            AdminWiring {
                agents_list: stores.agents_list.clone(),
                config_store,
                dynamic_agents: Arc::clone(&stores.dynamic_agents),
                experiments_list: stores.experiments_list.clone(),
                experiments_store: Arc::clone(&stores.experiments_store),
                judge_store: Arc::clone(&stores.judge_store),
                judges_list: stores.judges_list.clone(),
                memory: Arc::clone(&stores.memory),
                proxy_state: Arc::clone(proxy_state),
                settings_view: Arc::clone(&stores.settings_view),
                smoke_list: stores.smoke_list.clone(),
                smoke_store: Arc::clone(&stores.smoke_store),
                tasks: Arc::clone(worker_tasks),
                telemetry: Arc::clone(&stores.telemetry),
                token_store: Arc::clone(&stores.token_store),
                yaml_agents: stores.yaml_agents.clone(),
                yaml_experiments: stores.yaml_experiments.clone(),
                yaml_judges: stores.yaml_judges.clone(),
                yaml_smoke: stores.yaml_smoke.clone(),
            }
            .into_router(),
        );
        let proxy_router = auth.wrap_proxy(Arc::clone(proxy_state).router());
        let files_router = auth.wrap_proxy(
            FilesApi {
                store: Arc::clone(&stores.file_store),
            }
            .router(),
        );

        // WHY: axum 0.8 nests asymmetrically — `nest("/admin", ...)` matches
        // the inner `/` route at `/admin`, but a request to `/admin/` returns
        // 404. Redirect the trailing-slash form so bookmarks don't break.
        let mut app = Router::new()
            .merge(proxy_router)
            .merge(files_router)
            .route("/admin/", get(|| async { Redirect::permanent("/admin") }))
            .nest("/admin", admin_router);
        if let Some(oauth) = self.oauth_routes(stores, signer) {
            app = app.merge(oauth);
        }
        app = app.merge(triggers.webhook_router());
        self.config.server.apply_layers(app)
    }

    /// `ConfigStore` is the single point all YAML edits flow through —
    /// admin POSTs, the `PUT /admin/config` handler, hand-edits picked up
    /// by the file watcher. Its `on_reload` closure is the seam back into
    /// the in-memory hot state held by feature crates.
    fn config_store(&self, stores: &Stores) -> Arc<ConfigStore> {
        let on_reload = ReloadHandles {
            agents_list: stores.agents_list.clone(),
            dynamic_agents: Arc::clone(&stores.dynamic_agents),
            experiments_list: stores.experiments_list.clone(),
            experiments_store: Arc::clone(&stores.experiments_store),
            judge_store: Arc::clone(&stores.judge_store),
            judges_list: stores.judges_list.clone(),
            settings_view: Arc::clone(&stores.settings_view),
            smoke_list: stores.smoke_list.clone(),
            smoke_store: Arc::clone(&stores.smoke_store),
            state_dir: self.state_dir.clone(),
            yaml_agents: stores.yaml_agents.clone(),
            yaml_experiments: stores.yaml_experiments.clone(),
            yaml_judges: stores.yaml_judges.clone(),
            yaml_smoke: stores.yaml_smoke.clone(),
        }
        .into_hook();
        let config_path_abs =
            std::fs::canonicalize(&self.config_path).unwrap_or(self.config_path.clone());
        Arc::new(ConfigStore::new(
            config_path_abs,
            self.config.clone(),
            on_reload,
        ))
    }

    /// Build the per-user `ConnectLinkSigner` exactly when OAuth is wired up
    /// (i.e. the vault was opened because at least one MCP server has an
    /// `oauth:` block). One signer serves both the MCP runtime (so
    /// `NotConnectedTool` can mint URLs) and the OAuth route state (so
    /// `/mcp/.../connect` validates the same signature) — they must use
    /// the same key.
    fn connect_link_signer(
        &self,
        secrets: Option<&Secrets>,
    ) -> Result<Option<ConnectLinkSigner>, McpError> {
        let Some(secrets) = secrets else {
            return Ok(None);
        };
        Ok(Some(ConnectLinkSigner::new(
            &secrets.hmac_key,
            PublicBaseUrl::new(self.config.effective_public_base_url()),
        )?))
    }

    /// Token auth binds identity to the credential by construction, so it
    /// forces `from_credential` regardless of the (possibly default)
    /// `identity` field — otherwise a tokened request could spoof another
    /// user via `safety_identifier`.
    fn identity(&self) -> Identity {
        let mode = match self.config.auth.proxy.as_ref() {
            Some(scope) if scope.tokens.is_some() => IdentityMode::FromCredential,
            Some(scope) => scope.identity,
            None => IdentityMode::default(),
        };
        Identity {
            default_user_id: self
                .config
                .default_user_id
                .as_ref()
                .map(crate::config::UserKey::user_id),
            mode,
        }
    }

    /// Resolve infrastructure secrets (vault encryption + HMAC) only when
    /// an OAuth-enabled MCP server is configured. Priority: env vars >
    /// `.coulisse/secrets.env` > generated on the fly. Zero-config for
    /// local boots; deploy-friendly via env vars.
    fn mcp_secrets(&self) -> Result<Option<Secrets>, SecretsError> {
        if self.config.mcp.values().any(|c| c.oauth.is_some()) {
            return Ok(Some(Secrets::resolve(
                &self.state_dir,
                EnvKeys::from_env(),
            )?));
        }
        Ok(None)
    }

    fn memory_config(&self) -> Result<MemoryConfig, MemoryResolveError> {
        MemoryResolver {
            providers: &self.config.providers,
            state_dir: &self.state_dir,
        }
        .resolve(&self.config.memory)
    }

    /// OAuth routes live outside the auth wrappers — they have their own
    /// consumer-secret check (for the admin endpoint) and HMAC-signed
    /// tokens (for the per-user connect link). They use the same signer
    /// the MCP runtime was built with, so both sides agree on the key.
    fn oauth_routes(&self, stores: &Stores, signer: Option<ConnectLinkSigner>) -> Option<Router> {
        let vault = stores.mcp_vault.clone()?;
        let signer = signer?;
        Some(
            OAuthRouterState {
                configs: self.config.mcp.clone(),
                consumer_secret: self
                    .config
                    .auth
                    .mcp_consumer_secret
                    .as_ref()
                    .map(|s| mcp::ConsumerSecret::new(s.expose())),
                signer,
                vault,
            }
            .router(),
        )
    }

    async fn run(self, on_ready: impl FnOnce() + Send) -> Result<(), ServeError> {
        let memory_config = self.memory_config()?;
        let pricing = Arc::new(PricingTable::vendored()?);
        let mcp_secrets = self.mcp_secrets()?;
        let stores = Stores::open(
            &self.config,
            &memory_config,
            &self.state_dir,
            mcp_secrets.as_ref(),
        )
        .await?;
        // Auth is built after stores so the token scheme can borrow the token
        // store opened during boot. OIDC discovery (its only network step) still
        // happens here.
        let auth = Auth::from_config(
            self.config.auth.clone(),
            Some(Arc::clone(&stores.token_store)),
        )
        .await?;
        let _telemetry_guard = self.config.telemetry.init_subscriber(stores.pool.clone())?;
        let signer = self.connect_link_signer(mcp_secrets.as_ref())?;
        let runtime = Runtime::build(&self.config, &memory_config, &stores, signer.clone()).await?;
        runtime.reap_stale_tasks().await;
        let worker_tasks = Arc::clone(&runtime.tasks);
        Workers {
            agents: Arc::clone(&runtime.prompter),
            tasks: Arc::clone(&worker_tasks),
        }
        .spawn(4);
        let identity = self.identity();
        let triggers = self.triggers(&worker_tasks, identity);
        triggers.spawn_cron();
        triggers.fire_boot().await;
        sidecars::spawn_all(&self.config.sidecars);
        let proxy_state = runtime.into_app_state(&stores, identity, pricing);

        let addr = self.config.server.socket_addr()?;
        Banner {
            addr,
            agents: &proxy_state.agents.agents(),
            auth: &auth,
            experiments: &stores.experiments_list.load(),
            extractor: memory_config.extractor.as_ref(),
            judges: &stores.judges_list.load(),
            memory_summary: &MemorySummary::of(&memory_config).0,
        }
        .print();

        let config_store = self.config_store(&stores);
        let _watcher_guard = config_store.spawn_watcher()?;
        let app = self.app(
            &auth,
            Runnable {
                config_store,
                proxy_state: &proxy_state,
                signer,
                stores: &stores,
                triggers: &triggers,
                worker_tasks: &worker_tasks,
            },
        );
        let listener = TcpListener::bind(addr).await.map_err(ServerError::Bind)?;
        // Signal readiness only after the port is bound — anything failing
        // before this point exits the child without firing the callback, so
        // the launching `coulisse start` surfaces the error instead of
        // falsely reporting success.
        on_ready();
        axum::serve(listener, app)
            .await
            .map_err(ServerError::Serve)?;
        Ok(())
    }

    /// Time- and event-based triggers enqueue on behalf of the default
    /// user, or a fixed `cron` identity when no default is configured.
    fn triggers(&self, tasks: &Arc<Tasks>, identity: Identity) -> Triggers {
        let trigger_user_id = identity
            .default_user_id
            .unwrap_or_else(|| UserId::from_string("cron"));
        Triggers::new(
            &self.config.triggers,
            Arc::clone(tasks) as Arc<dyn TaskQueue>,
            trigger_user_id,
        )
    }
}

/// Everything a booted server exposes over HTTP, ready to be composed
/// into one router.
struct Runnable<'a> {
    config_store: Arc<ConfigStore>,
    proxy_state: &'a Arc<AppState<RigAgents>>,
    signer: Option<ConnectLinkSigner>,
    stores: &'a Stores,
    triggers: &'a Triggers,
    worker_tasks: &'a Arc<Tasks>,
}

/// Persistent stores opened against the shared `SQLite` pool, plus the
/// hot-reloadable arc-swap lists each feature crate watches.
struct Stores {
    /// The merged effective list the runtime resolves against; `yaml_agents`
    /// is the raw YAML view, kept alongside so the admin layer can compute
    /// source labels and the smart DELETE handler can decide
    /// tombstone-vs-physical-delete.
    agents_list: agents::AgentList,
    dynamic_agents: Arc<DynamicAgents>,
    experiments_list: experiments::ExperimentList,
    experiments_store: Arc<Experiments>,
    file_store: Arc<FileStore>,
    judge_store: Arc<Judges>,
    judges_list: judges::JudgeList,
    mcp_vault: Option<Arc<TokenVault>>,
    memory: Arc<Store>,
    pool: memory::SqlitePool,
    settings_view: crate::admin::SettingsHandle,
    smoke_list: smoke::SmokeList,
    smoke_store: Arc<SmokeStore>,
    telemetry: Arc<TelemetrySink>,
    /// Always opened: the studio token page is always mounted (it shows an
    /// empty state and the create form like every other admin page). Minted
    /// tokens only *gate* the proxy once `auth.proxy.tokens` is set — until
    /// then the page notes they're inert.
    token_store: Arc<TokenStore>,
    yaml_agents: agents::AgentList,
    yaml_experiments: experiments::ExperimentList,
    yaml_judges: judges::JudgeList,
    yaml_smoke: smoke::SmokeList,
}

impl Stores {
    /// Open one `SQLite` pool, every per-feature store, and reconcile each
    /// store with the YAML it was given. Each crate runs its own schema
    /// migrations against the shared pool — table ownership is per-crate,
    /// the connection is shared so operators back up one file.
    async fn open(
        config: &Config,
        memory_config: &MemoryConfig,
        state_dir: &Path,
        mcp_secrets: Option<&Secrets>,
    ) -> Result<Self, ServeError> {
        let agents_list = agents::AgentList::new(config.agents.clone());
        let yaml_agents = agents::AgentList::new(config.agents.clone());
        let judges_list = judges::JudgeList::new(config.judges.clone());
        let yaml_judges = judges::JudgeList::new(config.judges.clone());
        let experiments_list = experiments::ExperimentList::new(config.experiments.clone());
        let yaml_experiments = experiments::ExperimentList::new(config.experiments.clone());
        let smoke_list = smoke::SmokeList::new(config.smoke_tests.clone());
        let yaml_smoke = smoke::SmokeList::new(config.smoke_tests.clone());
        let settings_view = Arc::new(ArcSwap::from_pointee(
            crate::admin::SettingsView::from_config(config, memory_config),
        ));

        let pool = memory_config.backend.open_pool().await?;
        let mcp_vault = Self::open_vault(&pool, mcp_secrets).await?;
        let dynamic_agents = Arc::new(DynamicAgents::open(pool.clone()).await?);
        let report = dynamic_agents.rebuild(&agents_list, &config.agents).await?;
        MergeCounts::from(&report).log("agents");

        let memory = Arc::new(
            Store::open(
                pool.clone(),
                memory_config.clone(),
                config.embedder_fallback_key(memory_config).as_deref(),
            )
            .await?,
        );

        let file_store = Arc::new(
            Self::open_file_store(pool.clone(), &config.storage, &state_dir.join("files")).await?,
        );

        let telemetry = Arc::new(TelemetrySink::open(pool.clone()).await?);
        let judge_store = Arc::new(Judges::open(pool.clone()).await?);
        let report = judge_store
            .rebuild_judges(&judges_list, &config.judges)
            .await?;
        MergeCounts::from(&report).log("judges");

        let smoke_store = Arc::new(SmokeStore::open(pool.clone()).await?);
        let report = smoke_store
            .rebuild_smoke(&smoke_list, &config.smoke_tests)
            .await?;
        MergeCounts::from(&report).log("smoke tests");

        let experiments_store = Arc::new(Experiments::open(pool.clone()).await?);
        let report = experiments_store
            .rebuild(&experiments_list, &config.experiments)
            .await?;
        MergeCounts::from(&report).log("experiments");

        let token_store = Arc::new(TokenStore::open(pool.clone()).await?);

        Ok(Self {
            agents_list,
            dynamic_agents,
            experiments_list,
            experiments_store,
            file_store,
            judge_store,
            judges_list,
            mcp_vault,
            memory,
            pool,
            settings_view,
            smoke_list,
            smoke_store,
            telemetry,
            token_store,
            yaml_agents,
            yaml_experiments,
            yaml_judges,
            yaml_smoke,
        })
    }

    /// Open the MCP token vault if any server has an oauth block. The
    /// vault + HMAC keys come from the resolved `Secrets` (env, then the
    /// on-disk file, then generated) so no manual env-var setup is needed
    /// for zero-config local boots.
    async fn open_vault(
        pool: &memory::SqlitePool,
        mcp_secrets: Option<&Secrets>,
    ) -> Result<Option<Arc<TokenVault>>, ServeError> {
        let Some(secrets) = mcp_secrets else {
            return Ok(None);
        };
        coulisse_core::migrate::run(pool, &VaultMigrator).await?;
        Ok(Some(Arc::new(TokenVault::new(
            pool.clone(),
            &secrets.vault_key,
        )?)))
    }
}

impl Stores {
    /// Construct the blob backend and open the file store. The filesystem
    /// backend always lives under `.coulisse/files` (`files_dir`); only the
    /// backend choice and quotas come from YAML.
    async fn open_file_store(
        pool: memory::SqlitePool,
        yaml: &StorageYaml,
        files_dir: &Path,
    ) -> Result<FileStore, storage::StorageError> {
        let backend = match yaml.backend {
            storage::BackendKind::Fs => {
                let fs = FsBackend::new(files_dir).await?;
                BlobBackend::Fs(fs)
            }
            #[cfg(feature = "s3")]
            storage::BackendKind::S3 => {
                let Some(s3_cfg) = yaml.s3.as_ref() else {
                    return Err(storage::StorageError::backend(
                        "storage.backend: s3 — an `s3:` block is required for the s3 backend",
                    ));
                };
                BlobBackend::S3(storage::S3Backend::new(s3_cfg).await?)
            }
            #[cfg(not(feature = "s3"))]
            storage::BackendKind::S3 => {
                return Err(storage::StorageError::backend(
                    "storage.backend: s3 — this binary was built without the 's3' feature; rebuild with `--features s3`",
                ));
            }
        };
        FileStore::open(pool, backend, QuotaConfig::from(yaml)).await
    }
}

/// The four counts every feature's YAML/database merge reports.
struct MergeCounts {
    dynamic: usize,
    overrides: usize,
    tombstones: usize,
    yaml: usize,
}

impl From<&agents::MergeReport> for MergeCounts {
    fn from(report: &agents::MergeReport) -> Self {
        Self {
            dynamic: report.dynamic_count,
            overrides: report.override_count,
            tombstones: report.tombstone_count,
            yaml: report.yaml_count,
        }
    }
}

impl From<&judges::MergeReport> for MergeCounts {
    fn from(report: &judges::MergeReport) -> Self {
        Self {
            dynamic: report.dynamic_count,
            overrides: report.override_count,
            tombstones: report.tombstone_count,
            yaml: report.yaml_count,
        }
    }
}

impl From<&smoke::MergeReport> for MergeCounts {
    fn from(report: &smoke::MergeReport) -> Self {
        Self {
            dynamic: report.dynamic_count,
            overrides: report.override_count,
            tombstones: report.tombstone_count,
            yaml: report.yaml_count,
        }
    }
}

impl From<&experiments::MergeReport> for MergeCounts {
    fn from(report: &experiments::MergeReport) -> Self {
        Self {
            dynamic: report.dynamic_count,
            overrides: report.override_count,
            tombstones: report.tombstone_count,
            yaml: report.yaml_count,
        }
    }
}

impl MergeCounts {
    fn log(&self, feature: &'static str) {
        tracing::info!(
            yaml = self.yaml,
            overrides = self.overrides,
            dynamic = self.dynamic,
            tombstones = self.tombstones,
            "{feature} merged",
        );
    }
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

impl Runtime {
    /// Build the runtime `Judge` objects from the merged list (DB shadows
    /// and YAML) so DB-only judges are usable from the moment they're
    /// created. The map itself is rebuilt only at boot — runtime hot-reload
    /// of the `Judge` instances is a follow-up.
    async fn build(
        config: &Config,
        memory_config: &MemoryConfig,
        stores: &Stores,
        signer: Option<ConnectLinkSigner>,
    ) -> Result<Self, ServeError> {
        let judges = build_judges(&stores.judges_list.load())?;
        let mcp = Arc::new(
            McpServers::connect_with_vault(config.mcp.clone(), stores.mcp_vault.clone(), signer)
                .await?,
        );
        let experiments = Arc::new(ExperimentRouter::new(
            stores.experiments_list.load().to_vec(),
        ));
        let resolver: Arc<dyn AgentResolver> = Arc::new(ExperimentResolver::new(
            Arc::clone(&experiments),
            Some(Arc::clone(&stores.judge_store) as Arc<dyn ScoreLookup>),
        ));
        let tasks = Arc::new(Tasks::open(stores.pool.clone()).await?);
        let skills = Skills::load(&config.skills)?;
        let skill_catalog: Option<Arc<dyn SkillCatalog>> = if skills.is_empty() {
            None
        } else {
            Some(Arc::new(skills) as Arc<dyn SkillCatalog>)
        };
        let prompter = Arc::new(RigAgents::new(BootConfig {
            agents: stores.agents_list.clone(),
            mcp,
            providers: config.providers.clone(),
            resolver,
            skills: skill_catalog,
            task_queue: Some(Arc::clone(&tasks) as Arc<dyn TaskQueue>),
            task_status: Some(Arc::clone(&tasks) as Arc<dyn TaskStatus>),
        })?);
        let extractor = memory_config
            .extractor
            .as_ref()
            .map(|cfg| Arc::new(Extractor::new(cfg.clone(), Arc::clone(&prompter) as _)));
        let tracker = Tracker::open(stores.pool.clone()).await?;
        Ok(Self {
            experiments,
            extractor,
            judges,
            prompter,
            tasks,
            tracker,
        })
    }

    fn into_app_state(
        self,
        stores: &Stores,
        identity: Identity,
        pricing: Arc<PricingTable>,
    ) -> Arc<AppState<RigAgents>> {
        Arc::new(AppState {
            agents: self.prompter,
            experiments: self.experiments,
            extractor: self.extractor,
            identity,
            judge_store: Arc::clone(&stores.judge_store),
            judges: Arc::new(self.judges),
            memory: Arc::clone(&stores.memory),
            pricing,
            tokens: Arc::clone(&stores.token_store),
            tracker: self.tracker,
        })
    }

    /// Reap `running` tasks left over from a previous process before
    /// workers start, so PM sees them as `errored` on the next wakeup
    /// instead of believing the work is still in flight. Cutoff = now: any
    /// task still in `running` is by definition orphaned.
    async fn reap_stale_tasks(&self) {
        let now = coulisse_core::now_secs();
        match TaskStatus::reap_stale_running(
            self.tasks.as_ref(),
            now,
            "process restarted before task completed",
        )
        .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!(reaped = n, "stale running tasks marked errored"),
            Err(e) => tracing::warn!(%e, "task reap on boot failed; continuing"),
        }
    }
}

/// The seam back into in-memory hot state for the `ConfigStore`. Every
/// YAML edit (admin POST, `PUT /admin/config`, hand-edit picked up by
/// the file watcher) flows through this closure.
type ReloadHook = Arc<dyn Fn(Config) -> BoxFuture<'static, ()> + Send + Sync>;

/// Handles the reload hook needs. A struct so the long argument list
/// stays self-documenting at the call site.
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
    state_dir: PathBuf,
    yaml_agents: agents::AgentList,
    yaml_experiments: experiments::ExperimentList,
    yaml_judges: judges::JudgeList,
    yaml_smoke: smoke::SmokeList,
}

impl ReloadHandles {
    async fn apply(&self, cfg: Config) {
        self.yaml_agents.store(Arc::new(cfg.agents.clone()));
        self.yaml_judges.store(cfg.judges.clone());
        self.yaml_experiments.store(cfg.experiments.clone());
        self.yaml_smoke.store(Arc::new(cfg.smoke_tests.clone()));
        // WHY: re-resolve memory config; on failure keep the previous
        // settings view rather than crashing the reload path. The chat
        // path keeps its boot-time Store regardless — memory itself
        // does not hot reload.
        let resolver = MemoryResolver {
            providers: &cfg.providers,
            state_dir: &self.state_dir,
        };
        match resolver.resolve(&cfg.memory) {
            Err(err) => tracing::warn!(
                error = %err,
                "memory config resolution failed during reload; keeping previous settings view",
            ),
            Ok(memory_config) => {
                self.settings_view
                    .store(Arc::new(crate::admin::SettingsView::from_config(
                        &cfg,
                        &memory_config,
                    )));
            }
        }
        log_rebuild_failure(
            "agents",
            self.dynamic_agents
                .rebuild(&self.agents_list, &cfg.agents)
                .await,
        );
        log_rebuild_failure(
            "judges",
            self.judge_store
                .rebuild_judges(&self.judges_list, &cfg.judges)
                .await,
        );
        log_rebuild_failure(
            "experiments",
            self.experiments_store
                .rebuild(&self.experiments_list, &cfg.experiments)
                .await,
        );
        log_rebuild_failure(
            "smoke",
            self.smoke_store
                .rebuild_smoke(&self.smoke_list, &cfg.smoke_tests)
                .await,
        );
    }

    fn into_hook(self) -> ReloadHook {
        let handles = Arc::new(self);
        Arc::new(move |cfg: Config| {
            let handles = Arc::clone(&handles);
            Box::pin(async move { handles.apply(cfg).await })
        })
    }
}

fn log_rebuild_failure<T, E: std::fmt::Display>(kind: &str, result: Result<T, E>) {
    if let Err(err) = result {
        tracing::warn!(
            error = %err,
            "{kind} rebuild failed during reload; previous list kept",
        );
    }
}

/// Handles the admin surface is composed from. As with `ReloadHandles`,
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
    token_store: Arc<TokenStore>,
    yaml_agents: agents::AgentList,
    yaml_experiments: experiments::ExperimentList,
    yaml_judges: judges::JudgeList,
    yaml_smoke: smoke::SmokeList,
}

impl AdminWiring {
    /// Compose the admin surface from each feature crate's router.
    /// Cross-feature views (e.g. tool calls inside a conversation page) are
    /// filled in via htmx fragments so feature crates remain decoupled.
    fn into_router(self) -> Router {
        let smoke_runner: Arc<dyn RunDispatcher> = Arc::new(SmokeRunner {
            configs: self.smoke_list.clone(),
            state: self.proxy_state,
            store: Arc::clone(&self.smoke_store),
        });
        Router::new()
            .merge(
                agents::admin::AgentsAdmin {
                    dynamic_agents: self.dynamic_agents,
                    runtime_agents: self.agents_list,
                    yaml_agents: self.yaml_agents,
                }
                .router(),
            )
            .merge(Arc::clone(&self.config_store).file_router())
            .merge(crate::admin::static_router())
            .merge(Arc::clone(&self.config_store).sections_router())
            .merge(self.config_store.openapi_router())
            .merge(
                experiments::admin::ExperimentsAdmin {
                    runtime_experiments: self.experiments_list,
                    store: self.experiments_store,
                    yaml_experiments: self.yaml_experiments,
                }
                .router(),
            )
            .merge(
                judges::admin::JudgesAdmin {
                    runtime_configs: self.judges_list,
                    store: self.judge_store,
                    yaml_configs: self.yaml_judges,
                }
                .router(),
            )
            .merge(self.memory.admin_router())
            .merge(
                smoke::SmokeAdmin {
                    dispatcher: smoke_runner,
                    runtime_configs: self.smoke_list,
                    store: self.smoke_store,
                    yaml_configs: self.yaml_smoke,
                }
                .router(),
            )
            .merge(telemetry::admin::TelemetryAdmin::new(Arc::clone(&self.telemetry)).router())
            .merge(
                crate::admin::HomeState {
                    settings: Arc::clone(&self.settings_view),
                    telemetry: Arc::clone(&self.telemetry),
                }
                .router(),
            )
            .merge(
                crate::admin::live::LiveBoard {
                    tasks: self.tasks,
                    telemetry: self.telemetry,
                }
                .router(),
            )
            .merge(
                Router::new()
                    .route("/settings", get(crate::admin::settings))
                    .with_state(self.settings_view),
            )
            .route(
                "/",
                get(|| async { Redirect::permanent("/admin/overview") }),
            )
            .merge(auth::admin::TokenAdmin::new(self.token_store).router())
            .layer(from_fn(admin_shell))
    }
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

impl Config {
    /// Derive an API key to use when the memory embedder config doesn't carry
    /// its own. Looks up the matching top-level provider entry so users who
    /// already configured `OpenAI` for completions don't have to repeat the key.
    fn embedder_fallback_key(&self, memory_config: &MemoryConfig) -> Option<String> {
        // WHY: hash and voyage are not completion providers — no fallback
        // applies. For Voyage, the user must set
        // `memory.user_state.embed_with.api_key` explicitly.
        let kind = match &memory_config.embedder {
            EmbedderConfig::Hash { .. } | EmbedderConfig::Voyage { .. } => return None,
            EmbedderConfig::Openai { .. } => ProviderKind::Openai,
        };
        self.providers.get(&kind).map(|p| p.api_key.clone())
    }
}

/// The one-line memory description on the startup banner.
struct MemorySummary(String);

impl MemorySummary {
    fn of(config: &MemoryConfig) -> Self {
        let backend = match &config.backend {
            BackendConfig::InMemory => "in-memory (ephemeral)".to_string(),
            BackendConfig::Sqlite { path } => format!("sqlite at {}", path.display()),
        };
        if config.extractor.is_none() && config.recall_k == 0 {
            return Self(format!("{backend}; user_state: disabled (history only)"));
        }
        let embedder = match &config.embedder {
            EmbedderConfig::Hash { dims } => {
                format!("hash (dims={dims}, OFFLINE — no semantic understanding)")
            }
            EmbedderConfig::Openai { model, .. } => format!("openai / {model}"),
            EmbedderConfig::Voyage { model, .. } => format!("voyage / {model}"),
        };
        Self(format!("{backend}; embedder={embedder}"))
    }
}
