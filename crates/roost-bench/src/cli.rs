//! The `roost-bench` command line: `prepare` builds the stack once, `run`
//! measures it. Parsed by `main.rs`; the argument structs are consumed by
//! `prepare` and `run`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "roost-bench",
    about = "Measure the v3 stack's latency and footprint through one headless Chromium"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: BenchCommand,
}

#[derive(Debug, Subcommand)]
pub enum BenchCommand {
    /// Build the release binaries and the web bundle.
    Prepare(PrepareArgs),
    /// Boot the stack in isolation, drive the scenarios, and write a report.
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct PrepareArgs {
    /// The Chromium executable to drive.
    #[arg(long)]
    pub chrome: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Rounds: each boots a fresh stack and a fresh browser.
    #[arg(long, default_value_t = 3)]
    pub rounds: u32,
    /// Run directory. Default: target/bench/runs/<UTC timestamp>.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Override the Chromium recorded by `prepare`.
    #[arg(long)]
    pub chrome: Option<PathBuf>,
    /// Run even when HEAD moved since `prepare`.
    #[arg(long)]
    pub allow_stale: bool,
    /// `ROOST_LOG_LEVEL` for the coordinator and the worker (and so keeper);
    /// unset leaves the default.
    #[arg(long)]
    pub child_log_level: Option<String>,
}
