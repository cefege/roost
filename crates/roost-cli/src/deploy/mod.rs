//! `roost deploy` and `roost keeper-refresh`: the two commands that change
//! another machine, and the four hidden `__remote-*` commands the target side of
//! a deploy runs on that machine.
//!
//! The split is the design. Everything on this side decides WHAT should happen
//! to a target — is its build identity proved, may its keeper be carried across,
//! where does its identity come from — and everything on the other side decides
//! what the target's own files say. Each side answers with a value the other
//! cannot misread, and the boundary between them is a manifest and a report
//! rather than a shared guess about where a machine keeps its state.
//!
//! The exit codes are in `codes.rs` and are shared with `roost push`, which is
//! why they are named rather than inlined: 5 means a keeper was not adopted in
//! either command, and a wrapper that reads 5 from one of them and 1 from the
//! other is a wrapper that retries the destruction of somebody's shells.
//!
//! **Which lines here are load-bearing is written down, not left to be
//! inferred.** `docs/phase6-cli-contract.md` § "The guard map" maps each of the
//! thirteen `docs/FAILURE-INDEX.md` entries whose symptom is a misbehaving
//! deploy to the function that satisfies it and the test that holds it. Read that
//! table before changing anything in this group: a step here that looks like
//! ceremony is usually the guard for a defect that already shipped.

pub mod admission;
pub mod apply;
pub mod apply_release;
pub mod codes;
pub mod convergence;
pub mod facts;
pub mod identity;
pub mod identity_env;
pub mod installed;
pub mod invocation;
pub mod keeper_client;
pub mod keeper_refresh;
pub mod keeper_step;
pub mod machine_txn;
pub mod manifest;
pub mod release;
pub mod remote_commands;
pub mod retire;
pub mod run;
pub mod ssh;
pub mod target_evidence;
pub mod txn_session;

use std::path::PathBuf;

use clap::Args;

/// `roost deploy <host>` — replace one machine's release and restart its worker.
#[derive(Debug, Args)]
#[command(
    name = "deploy",
    about = "Deploy this build to one machine's worker over ssh"
)]
pub struct DeployArgs {
    /// The target: a host this machine can ssh to, or `user@host`.
    pub host: String,
    /// The name the target enrolls under. Never taken from this shell's
    /// environment for a remote host, because that names THIS machine.
    #[arg(long)]
    pub label: Option<String>,
    /// The address the rest of the fleet reaches the target at.
    #[arg(long)]
    pub reachable_addr: Option<String>,
    /// The source checkout to build the release from. Defaults to the checkout
    /// this binary was built from.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// The build this deploy is required to install. A second, independent check
    /// on the build identity.
    #[arg(long)]
    pub expected_sha: Option<String>,
    /// The release digest this deploy is required to install.
    #[arg(long)]
    pub expected_manifest_sha256: Option<String>,
    /// Ship a commit that is not published. The localhost quickstart path only.
    #[arg(long)]
    pub allow_unpublished_local: bool,
    /// Prove the build against the installed coordinator's own release rather
    /// than against an upstream tip. What a coordinator-started deploy uses.
    #[arg(long)]
    pub coordinator_release: bool,
    /// Authorize the new worker to destroy every PTY a keeper it cannot adopt
    /// holds, for this deploy only.
    #[arg(long)]
    pub force_live: bool,
}

/// `roost keeper-refresh <host>` — shut a target's keeper down empty, under the
/// coordinator's fence.
#[derive(Debug, Args)]
#[command(
    name = "keeper-refresh",
    about = "Shut a machine's keeper down empty, keeping its worker installed"
)]
pub struct KeeperRefreshArgs {
    /// The target: a host this machine can ssh to, or `user@host`.
    pub host: String,
    /// Required. Keeper maintenance destroys PTYs, so it is never done without
    /// being asked for on the command line.
    #[arg(long)]
    pub yes: bool,
    /// Authorize the destruction of live PTYs, not merely of an empty keeper.
    #[arg(long)]
    pub force_live: bool,
}
