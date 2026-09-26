//! `roost state` — print the STATE.md snapshot an agent's Stop hook redirects
//! into the repository. Called by the crate's dispatcher. Reads git and
//! nothing else: every fact in the snapshot is a fact about the checkout, and a
//! command that also reported service health would have to keep two truths in
//! step.
//!
//! One thing genuinely changed from the TypeScript original, and it is worth
//! naming: it resolved the repository root from its own source path
//! (`new URL("../../../", import.meta.url)`), which a compiled binary does not
//! have. The root is an argument here, defaulting to the working directory.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use crate::command_error::CommandFailure;
use crate::utc_clock::format_utc_minute;
use crate::wall_clock;

/// How many `git status` lines the snapshot carries. Past this it is a wall of
/// unrelated filenames and the block stops being a summary.
pub const STATE_STATUS_LINES: usize = 30;

#[derive(Debug, Args)]
#[command(about = "Print a STATE.md snapshot of this checkout")]
pub struct StateArgs {
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub repo: PathBuf,
}

pub fn run(args: &StateArgs) -> Result<ExitCode, CommandFailure> {
    let branch = git(&args.repo, &["branch", "--show-current"])?;
    let commits = git(&args.repo, &["log", "--oneline", "-5"])?;
    let status: String = git(&args.repo, &["status", "--short"])?
        .lines()
        .take(STATE_STATUS_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let status_block = if status.is_empty() {
        "(clean)"
    } else {
        &status
    };
    print!(
        "<!-- AUDIENCE: claude (auto-updated by Stop hook) -->\n\
         # STATE — Roost v3 snapshot\n\
         \n\
         updated={}\n\
         branch={branch}\n\
         \n\
         ## last 5 commits\n\
         ```\n\
         {commits}\n\
         ```\n\
         \n\
         ## git status\n\
         ```\n\
         {status_block}\n\
         ```\n\
         \n\
         ## next action\n\
         Active work is tracked per-commit (`<area>: <scope>`). See `git log --oneline` for\n\
         the current arc, and record the next architectural shift in ARCHITECTURE.md\n\
         before starting it.\n",
        format_utc_minute(wall_clock::now_ms())
    );
    Ok(ExitCode::SUCCESS)
}

fn git(repo: &std::path::Path, args: &[&str]) -> Result<String, CommandFailure> {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|error| CommandFailure::generic(format!("git {}: {error}", args.join(" "))))?;
    if !output.status.success() {
        return Err(CommandFailure::generic(format!(
            "git {} failed in {}",
            args.join(" "),
            repo.display()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}
