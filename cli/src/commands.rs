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
