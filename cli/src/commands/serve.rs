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
use storage::{BackendKind, BlobBackend, FsBackend, QuotaConfig, StorageYaml, Store as FileStore};
use tasks::Tasks;
use telemetry::Sink as TelemetrySink;
use tokio::net::TcpListener;

use crate::admin::shell as admin_shell;
use crate::banner::Banner;
use crate::config::Config;
use crate::config_store::ConfigStore;
use crate::files;
use crate::memory_resolve;
use crate::server::{self, AppState};
use crate::smoke_runner::SmokeRunner;
