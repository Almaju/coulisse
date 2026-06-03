//! Coulisse CLI entry point. Parses subcommands and delegates to
//! `commands::*`. With no subcommand, defaults to running the server
//! in the foreground (preserving the historical `./coulisse` behavior).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use coulisse::commands::{
    check, init, restart, schema, serve, start, status, stop, studio, update,
};

const DEFAULT_CONFIG: &str = "coulisse.yaml";

#[derive(Parser)]
#[command(name = "coulisse", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the YAML config. Defaults to ./coulisse.yaml in the
    /// current directory. State files (PID, log) are written to
    /// `<dir>/.coulisse/` next to this path.
    #[arg(short, long, global = true, env = "COULISSE_CONFIG")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Validate the YAML config without starting the server.
    Check,
    /// Write a starter coulisse.yaml in the working directory.
    Init {
        /// Overwrite the file if it already exists.
        #[arg(long)]
        force: bool,
        /// Copy the full annotated example instead of the minimal template.
        #[arg(long)]
        from_example: bool,
    },
    /// Restart the running server (stop, then start detached).
    Restart,
    /// Emit the JSON Schema for `coulisse.yaml` to stdout. Redirect to
    /// `coulisse.schema.json` next to your config and reference it via
    /// `# yaml-language-server: $schema=./coulisse.schema.json` for IDE
    /// autocomplete and validation.
    Schema,
    /// Start the server, detached. Use --foreground to run attached.
    Start {
        /// Internal: marker that we are the re-spawned detached child.
        #[arg(long, hide = true)]
        detached_child: bool,
        /// Run in the foreground instead of detaching.
        #[arg(short = 'F', long)]
        foreground: bool,
    },
    /// Report whether a detached server is running.
    Status,
    /// Stop a running detached server (reads .coulisse/coulisse.pid).
    Stop {
        /// SIGKILL instead of SIGTERM if the server doesn't exit promptly.
        #[arg(long)]
        force: bool,
    },
    /// Open the studio UI (`/admin/`) in the default web browser.
    /// Requires the server to be running — use `coulisse start` first.
    #[command(alias = "admin")]
    Studio,
    /// Download and install the latest release from GitHub.
    Update,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = cli.config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));

    let result: Result<(), Box<dyn std::error::Error>> = match cli.command {
        // NOTE: `coulisse` with no subcommand → run foreground.
        None => run_foreground(&config),
        Some(Command::Check) => check::run(&config),
        Some(Command::Init {
            force,
            from_example,
        }) => init::run(
            &config,
            &init::Options {
                force,
                from_example,
            },
        )
        .map_err(std::convert::Into::into),
        Some(Command::Restart) => restart::run(&config),
        Some(Command::Schema) => schema::run(),
        Some(Command::Start {
            detached_child,
            foreground,
        }) => start::run(
            &config,
            &start::Options {
                detached_child,
                foreground,
            },
        )
        .map_err(std::convert::Into::into),
        Some(Command::Status) => status::run(&config),
        Some(Command::Stop { force }) => {
            stop::run(&config, &stop::Options { force }).map_err(std::convert::Into::into)
        }
        Some(Command::Studio) => studio::run(&config).map_err(std::convert::Into::into),
        Some(Command::Update) => update::run().map_err(std::convert::Into::into),
    };

    match result {
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
        Ok(()) => ExitCode::SUCCESS,
    }
}

fn run_foreground(config: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve::run(config, || {}))
}
