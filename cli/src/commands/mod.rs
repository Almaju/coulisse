//! Subcommand implementations dispatched from `main`.
//!
//! Each file owns one verb. `serve` is the actual server boot; `start`
//! self-respawns into `serve --foreground` for the detached form.

pub mod check;
pub mod init;
pub mod restart;
pub mod serve;
pub mod start;
pub mod status;
pub mod stop;
pub mod update;
