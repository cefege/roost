//! `roost worktree add`, the CLI boundary for Git-linked checkouts.
//! Called by command dispatch; delegates checkout and clone safety to roost-vcs.
//! Git owns ref creation, worktree metadata, index, checkout, and post-checkout hooks.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::{Args, Subcommand};

use crate::command_error::CommandFailure;

#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeCommand,
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    /// Create a linked Git worktree.
    Add(AddArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Run Git relative to this directory.
    #[arg(short = 'C', long = "cwd")]
    cwd: Option<PathBuf>,
    /// Create a new branch.
    #[arg(short = 'b', conflicts_with = "force_branch")]
    branch: Option<String>,
    /// Create or reset a branch.
    #[arg(short = 'B', conflicts_with = "branch")]
    force_branch: Option<String>,
    /// Detach HEAD rather than checking out a branch.
    #[arg(short = 'd', long)]
    detach: bool,
    /// Suppress preparation and result messages.
    #[arg(short, long)]
    quiet: bool,
    /// New worktree path.
    #[arg(required = true)]
    path: PathBuf,
    /// Commit-ish to check out.
    commit: Option<String>,
}

pub fn run(args: &WorktreeArgs) -> Result<ExitCode, CommandFailure> {
    match &args.command {
        WorktreeCommand::Add(add) => add_worktree(add),
    }
}

fn add_worktree(args: &AddArgs) -> Result<ExitCode, CommandFailure> {
    let cwd = std::env::current_dir()?;
    let workdir = match &args.cwd {
        Some(path) => absolute(&cwd, path),
        None => cwd,
    };
    let workdir = std::fs::canonicalize(&workdir).map_err(|error| {
        CommandFailure::generic(format!("cannot open {}: {error}", workdir.display()))
    })?;
    let root = git_text(&workdir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root);
    let path = absolute(&workdir, &args.path);
    validate_destination(&path, &args.path)?;
    let reference = args.commit.as_deref().unwrap_or("HEAD");
    let mut detach = false;
    let mut created_branch = None;
    let branch = args.branch.as_ref().or(args.force_branch.as_ref());
    let selected_ref = if let Some(branch) = branch {
        let start = args.commit.as_deref().unwrap_or("HEAD");
        let mut command = Command::new("git");
        command.current_dir(&root).arg("branch");
        if args.force_branch.is_some() {
            command.arg("-f");
        }
        command.arg("--").arg(branch).arg(start);
        run_git(&mut command, "creating branch")?;
        created_branch = Some(branch.as_str());
        branch.clone()
    } else if args.detach {
        detach = true;
        reference.to_owned()
    } else if args.commit.is_some() {
        let local = format!("refs/heads/{reference}");
        if git_succeeds(&root, ["show-ref", "--verify", "--quiet", &local]) {
            reference.to_owned()
        } else {
            detach = true;
            reference.to_owned()
        }
    } else {
        let name = path
            .file_name()
            .and_then(|part| part.to_str())
            .ok_or_else(|| {
                CommandFailure::usage("worktree path must have a final UTF-8 component")
            })?;
        let local = format!("refs/heads/{name}");
        if git_succeeds(&root, ["show-ref", "--verify", "--quiet", &local]) {
            name.to_owned()
        } else {
            run_git(
                Command::new("git")
                    .current_dir(&root)
                    .args(["branch", "--", name, "HEAD"]),
                "creating branch",
            )?;
            created_branch = Some(name);
            name.to_owned()
        }
    };

    let sha = git_text(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{selected_ref}^{{commit}}"),
        ],
    )?;
    if !args.quiet {
        let preparation = if let Some(branch) = created_branch {
            format!("new branch '{branch}'")
        } else if detach {
            format!("detached HEAD {}", &sha[..sha.len().min(7)])
        } else {
            format!("checking out '{selected_ref}'")
        };
        println!("Preparing worktree ({preparation})");
    }
    let result = roost_vcs::worktree_add(
        &root,
        &path,
        &selected_ref,
        roost_vcs::WorktreeAddOptions {
            detach,
            clone: roost_vcs::CloneMode::Auto,
            keep_changes: false,
        },
    )
    .map_err(|error| CommandFailure::generic(error.to_string()))?;
    if !args.quiet {
        println!(
            "HEAD is now at {} {}",
            &result.head[..result.head.len().min(7)],
            result.subject
        );
        if result.cloned {
            println!("Cloned from {} via reflink", root.display());
        }
    }
    if let Some(error) = result.clone_error {
        eprintln!("warning: worktree clone fell back to plain checkout: {error}");
    }
    let hook = roost_vcs::run_post_checkout_hook(&path, &result.head)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    std::io::stderr().write_all(&hook.stdout)?;
    std::io::stderr().write_all(&hook.stderr)?;
    if !hook.status.success() {
        let code = hook
            .status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1);
        return Ok(ExitCode::from(code));
    }
    Ok(ExitCode::SUCCESS)
}

fn validate_destination(path: &Path, display: &Path) -> Result<(), CommandFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let mut entries = std::fs::read_dir(path)
                .map_err(|error| CommandFailure::generic(error.to_string()))?;
            if entries.next().is_some() {
                Err(CommandFailure::generic(format!(
                    "fatal: '{}' already exists",
                    display.display()
                )))
            } else {
                Ok(())
            }
        }
        Ok(_) => Err(CommandFailure::generic(format!(
            "fatal: '{}' already exists",
            display.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CommandFailure::generic(error.to_string())),
    }
}

fn absolute(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

fn git_succeeds<const N: usize>(cwd: &Path, args: [&str; N]) -> bool {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String, CommandFailure> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(CommandFailure::generic(format!(
            "git {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|error| CommandFailure::generic(error.to_string()))
}

fn run_git(command: &mut Command, action: &str) -> Result<(), CommandFailure> {
    let output = command.stdin(Stdio::null()).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CommandFailure::generic(format!(
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}
