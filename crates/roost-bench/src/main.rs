//! `roost-bench`: boots the v2 (Bun) and v3 (Rust) stacks in isolation on
//! this machine, drives one headless Chromium through the same scenarios on
//! each, samples process CPU/RSS, and writes a side-by-side report. The entry
//! point prints the report path and table; every other module logs via tracing.

#![forbid(unsafe_code)]

mod browser;
mod cli;
mod coord;
mod error;
mod exec;
mod paths;
mod prepare;
mod report;
mod run;
mod sampler;
mod scenario;
mod stack;
mod stats;

use clap::Parser as _;

use crate::cli::{BenchCommand, Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    roost_observability::init()?;
    let cli = Cli::parse();
    match cli.command {
        BenchCommand::Prepare(args) => {
            let manifest = prepare::prepare(&args).await?;
            println!("prepared: {}", manifest.display());
        }
        BenchCommand::Run(args) => {
            let written = run::run(&args).await?;
            println!("{}", written.markdown);
            println!("report: {}", written.markdown_path.display());
        }
    }
    Ok(())
}
