//! `roost test [profile]` — the local and CI test entry points, one profile per
//! set of prerequisites. Called by the crate's dispatcher. Every step runs a
//! real tool with inherited stdio and fails on its exit status: this command
//! composes gates, it does not reimplement any of them, and a step whose
//! failure it swallowed would be a gate that reports green.
//!
//! The Rust tiers are `cargo`; the browser tiers are still the TypeScript
//! Playwright oracle until Phase 7 deletes it, and that split is why a
//! profile name from the v2 CLI (`worker`) is gone — there is no per-app
//! JavaScript suite left to isolate.

use std::process::ExitCode;

use clap::{Args, ValueEnum};

use crate::command_error::CommandFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TestProfile {
    /// The repository gates: file-size cap, crate DAG, stdout rule, design ratchet.
    Lint,
    /// `cargo test --workspace` — unit, conformance vectors, terminal-core vectors.
    Unit,
    /// The real-flow browser tier: a real coordinator, worker, keeper, PTY and browser.
    Terminal,
    /// The only gate that proves an EXISTING install survives a new release.
    Upgrade,
    /// An optional monitor against a deployed coordinator. Never a merge blocker.
    LiveApi,
    /// lint, unit, terminal, upgrade — the order a release candidate runs.
    All,
}

impl TestProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            TestProfile::Lint => "lint",
            TestProfile::Unit => "unit",
            TestProfile::Terminal => "terminal",
            TestProfile::Upgrade => "upgrade",
            TestProfile::LiveApi => "live-api",
            TestProfile::All => "all",
        }
    }
}

#[derive(Debug, Args)]
#[command(about = "Run a test profile")]
pub struct TestArgs {
    #[arg(value_name = "PROFILE", value_enum, default_value_t = TestProfile::All)]
    pub profile: TestProfile,
}

pub fn run(args: &TestArgs) -> Result<ExitCode, CommandFailure> {
    for step in steps(args.profile)? {
        step.run()?;
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
}

impl Step {
    fn new(label: &str, program: &str, args: &[&str]) -> Self {
        Self {
            label: label.to_string(),
            program: program.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }

    pub fn run(&self) -> Result<(), CommandFailure> {
        println!(">> {}", self.label);
        let status = std::process::Command::new(&self.program)
            .args(&self.args)
            .status()
            .map_err(|error| CommandFailure::generic(format!("{}: {error}", self.program)))?;
        if status.success() {
            return Ok(());
        }
        Err(CommandFailure::new(
            exit_code_of(status),
            format!("{} failed", self.label),
        ))
    }
}

/// The child's exit status as this process's. A signalled child reports no
/// code at all, and 1 is the honest answer for "it died" rather than a
/// fabricated one; anything a byte cannot hold is refused rather than wrapped.
fn exit_code_of(status: std::process::ExitStatus) -> u8 {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1)
}

pub fn steps(profile: TestProfile) -> Result<Vec<Step>, CommandFailure> {
    let steps = match profile {
        TestProfile::Lint => vec![Step::new("lint", "cargo", &["xtask", "lint"])],
        TestProfile::Unit => vec![Step::new("unit", "cargo", &["test", "--workspace"])],
        // The Playwright oracle is still TypeScript until Phase 7, and it is
        // still the only tier that drives a real browser against a real stack.
        TestProfile::Terminal => vec![Step::new("terminal", "bun", &["run", "test:terminal"])],
        TestProfile::Upgrade => vec![Step::new("upgrade", "bun", &["run", "test:upgrade"])],
        TestProfile::LiveApi => {
            if std::env::var("ROOST_COORD_URL").is_err() {
                return Err(CommandFailure::generic(
                    "live-api requires ROOST_COORD_URL; run \
                     ROOST_COORD_URL=https://<coordinator> roost test live-api",
                ));
            }
            vec![Step::new(
                "live-api",
                "bun",
                &["test", "smoke/api_smoke.test.ts"],
            )]
        }
        TestProfile::All => vec![
            Step::new("lint", "cargo", &["xtask", "lint"]),
            Step::new("unit", "cargo", &["test", "--workspace"]),
            Step::new("terminal", "bun", &["run", "test:terminal"]),
            Step::new("upgrade", "bun", &["run", "test:upgrade"]),
        ],
    };
    Ok(steps)
}
