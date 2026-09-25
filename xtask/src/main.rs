//! `cargo xtask` — the repository gate runner, and the only place a Roost
//! developer runs a check that is not `cargo build` or `cargo test`.
//! Blocking in CI; see CLAUDE.md "Verification" for the full command list.

mod crate_dag;
mod design_raw;
mod file_size;
mod fmt;
mod ratchet;
mod source_tree;
mod stdout_rule;
mod violation;

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use ratchet::RatchetOutcome;
use violation::Violation;

#[derive(Parser)]
#[command(name = "xtask", about = "Roost v3 repository gates")]
struct Xtask {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run every gate: the file-size cap, the crate dependency DAG, the
    /// stdout rule, and the design raw-value ratchet.
    Lint(LintArgs),
    /// The formatting gate. Separate from `lint` because it shells out to
    /// cargo, and separate because `cargo fmt --all` would reformat the
    /// vendored terminal core — see `fmt` for why that is not a matter of
    /// taste.
    Fmt,
}

#[derive(Args)]
struct LintArgs {
    /// Re-snapshot xtask/file-size-baseline.json. Run only after a split
    /// lowered a count, never to silence a regression.
    #[arg(long)]
    update_size_baseline: bool,
    /// Re-snapshot xtask/design-raw-baseline.json under the same rule.
    #[arg(long)]
    update_design_baseline: bool,
}

/// `cargo fmt --check` over the crates this repository authors.
fn fmt() -> ExitCode {
    if fmt::check() {
        println!("xtask fmt: formatted");
        return ExitCode::SUCCESS;
    }
    println!("xtask fmt: run `cargo fmt -p <crate>` over the workspace and commit");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    match Xtask::parse().command {
        Command::Lint(arguments) => lint(&arguments),
        Command::Fmt => fmt(),
    }
}

fn lint(arguments: &LintArgs) -> ExitCode {
    let mut violations: Vec<Violation> = Vec::new();
    let mut snapshots: Vec<String> = Vec::new();

    match file_size::run(arguments.update_size_baseline) {
        RatchetOutcome::Regressions(found) => violations.extend(found),
        RatchetOutcome::BaselineRewritten { file_count, total } => {
            snapshots.push(format!("{file_count} files, {total} lines"));
        }
    }
    violations.extend(crate_dag::run());
    violations.extend(stdout_rule::run());
    match design_raw::run(arguments.update_design_baseline) {
        RatchetOutcome::Regressions(found) => violations.extend(found),
        RatchetOutcome::BaselineRewritten { file_count, total } => {
            snapshots.push(format!("{file_count} files, {total} raw-value lines"));
        }
    }

    for snapshot in snapshots {
        println!("xtask: re-baselined — {snapshot}");
    }
    if violations.is_empty() {
        println!("xtask: 0 violations");
        return ExitCode::SUCCESS;
    }
    println!("xtask: {} violations\n", violations.len());
    for violation in &violations {
        println!("{}", violation.render());
    }
    ExitCode::FAILURE
}
