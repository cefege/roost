//! `roost push`: one journaled fleet transaction with a single decision
//! boundary, rolling the whole fleet back on any failure before the finalizing
//! checkpoint.
//!
//! The module is split by the question each file answers, and the questions are
//! disjoint on purpose:
//!
//! - [`plan`]: which registered machines this push may converge, and which it
//!   defers. No I/O, no journal, no keeper decision.
//! - [`admission`]: whether each candidate's keeper may be carried across. One
//!   shared rule, decided from one coordinator snapshot, before anything is
//!   touched.
//! - [`journal`]: the durable record of the transaction, on the machine
//!   deploy journal's own phase vocabulary rather than a second one.
//! - [`rollout`]: the decision boundary itself — converge, commit, or roll the
//!   whole fleet back — over a trait, so the branch is a property of the code
//!   rather than of whichever machines happen to be up.
//! - [`coordinator`]: the local POSIX coordinator, which is the only participant
//!   that does not go over ssh.
//! - [`participant`]: one remote machine, in either direction.
//! - [`runtime`]: the real [`rollout::FleetRuntime`], binding the two legs to the
//!   live roster.
//! - [`command`]: the step order, and the only place that decides what may be
//!   shipped.
//!
//! `roost push` takes no arguments, and the two flags v2 read out of raw argv
//! (`--allow-dirty`, `--no-coord`) are gone rather than honoured: both are
//! refusals in v2, and a flag that exists only to be refused is a flag an
//! operator can mistype into a failed push. `ROOST_PUSH_TARGETS` is cut with
//! them — a push is a whole-fleet transaction, and a subset of the fleet is
//! `roost deploy <host>`.
//!
//! The exit codes are `deploy::codes` and are shared with `roost deploy` on
//! purpose: 5 means a keeper was not adopted in either command, 7 means the
//! build could not be proved in either, and 8 means a fleet reached its
//! irreversible point and could not settle.

pub mod admission;
pub mod command;
pub mod coordinator;
pub mod journal;
pub mod participant;
pub mod plan;
pub mod rollout;
pub mod runtime;
pub mod source;

use std::process::ExitCode;

use clap::Args;

use crate::command_error::CommandFailure;

/// `roost push` — publish this commit and roll the whole fleet onto it.
#[derive(Debug, Args)]
#[command(
    name = "push",
    about = "Publish this clean commit and roll the local coordinator and every reachable worker onto it"
)]
pub struct PushArgs {}

/// Run `roost push`.
pub async fn run(args: &PushArgs) -> Result<ExitCode, CommandFailure> {
    command::push(args).await
}
