//! Subcommand implementations dispatched from `main`.
//!
//! Each file owns one verb. `serve` is the actual server boot; `start`
//! self-respawns into `serve --foreground` for the detached form.

pub mod check;
pub mod init;
pub mod reset;
pub mod restart;
pub mod schema;
pub mod serve;
pub mod skill;
pub mod start;
pub mod status;
pub mod stop;
pub mod studio;
pub mod token;
pub mod update;

/// Every way a subcommand can fail, one variant per verb, so `main` can
/// print the failure without erasing which command produced it.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Check(#[from] crate::config::ConfigError),
    #[error(transparent)]
    Init(#[from] init::InitError),
    #[error(transparent)]
    Reset(#[from] reset::ResetError),
    #[error(transparent)]
    Restart(#[from] restart::RestartError),
    #[error("failed to render the JSON Schema: {0}")]
    Schema(#[from] serde_json::Error),
    #[error(transparent)]
    Serve(#[from] serve::ServeError),
    #[error(transparent)]
    Skill(#[from] skill::SkillError),
    #[error(transparent)]
    Start(#[from] start::StartError),
    #[error(transparent)]
    Stop(#[from] stop::StopError),
    #[error(transparent)]
    Studio(#[from] studio::StudioError),
    #[error(transparent)]
    Token(#[from] token::TokenError),
    #[error(transparent)]
    Update(#[from] update::UpdateError),
}
