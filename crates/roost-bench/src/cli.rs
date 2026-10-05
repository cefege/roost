//! The `roost-bench` command line: `prepare` builds both stacks once, `run`
//! measures them. Parsed by `main.rs`; the argument structs are consumed by
//! `prepare` and `run`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::stack::StackId;

#[derive(Debug, Parser)]
#[command(
    name = "roost-bench",
    about = "Measure the v2 (Bun) and v3 (Rust) stacks through one headless Chromium"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: BenchCommand,
}

#[derive(Debug, Subcommand)]
pub enum BenchCommand {
    /// Build the v3 release binaries and web bundle, and the v2 checkout's web bundle.
    Prepare(PrepareArgs),
    /// Boot each stack in isolation, drive the scenarios, and write a report.
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct PrepareArgs {
    /// A v2 (`main`) checkout. Default: a `main` worktree at target/bench/v2-src.
    #[arg(long)]
    pub v2_root: Option<PathBuf>,
    /// The Chromium executable to drive.
    #[arg(long)]
    pub chrome: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Rounds per stack; stacks alternate within a round so drift hits both.
    #[arg(long, default_value_t = 3)]
    pub rounds: u32,
    /// Which stacks to measure, in per-round order.
    #[arg(long, value_enum, value_delimiter = ',', default_values_t = [StackId::V2, StackId::V3])]
    pub stacks: Vec<StackId>,
    /// Run directory. Default: target/bench/runs/<UTC timestamp>.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Override the Chromium recorded by `prepare`.
    #[arg(long)]
    pub chrome: Option<PathBuf>,
    /// Run even when HEAD or the v2 checkout moved since `prepare`.
    #[arg(long)]
    pub allow_stale: bool,
}
