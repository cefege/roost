//! The worker as a service: boot order, the keeper it owns, the coordinator
//! link, and the one rule that a dropped link is not a stopped process.
//! `serve` is the entry point; `roost-cli` and the `roost-worker` binary call
//! it and nothing else here.
//!
//! The boot order is [`boot_order::BOOT_ORDER`] and it is not negotiable, and
//! the sequence that runs it is [`boot_sequence::run`]. The identity is settled
//! before anything is probed or spawned; the door opens before the link so a
//! local browser keeps its terminals while the coordinator is unreachable; the
//! survivor decision needs the coordinator's open-session set, and a set nobody
//! has read cannot decide anything; and readiness is announced last because
//! readiness is a claim about the steps before it.
//!
//! Two rules span every module here. A coordinator disconnect is a reconnect,
//! never a shutdown: the keeper holds the PTYs and outlives this process on
//! purpose, so the only things that end the worker are a signal and an explicit
//! shutdown frame. And nothing here may mutate a survivor it cannot prove empty
//! — see [`keeper_boot::decide`], which is a pure function precisely so that
//! decision can be tested without a keeper, a coordinator, or a PTY.

pub mod agent_owners;
pub mod boot;
pub mod boot_admission;
pub mod boot_order;
mod boot_outbox;
pub mod boot_sequence;
pub mod bootstrap_redeem;
pub mod capabilities;
pub mod cell_cadence;
pub mod cell_delivery;
pub mod channel_delivery;
pub mod credential;
pub mod deps;
pub mod door_routes;
pub mod door_serve;
pub mod downstream;
pub mod heart_owners;
pub mod heartbeat;
pub mod heartbeat_metrics;
pub mod heartbeat_sources;
pub mod keeper_boot;
pub mod keeper_prepare;
pub mod keeper_probe;
pub mod keeper_retire;
pub mod link_downstream;
pub mod link_drain;
pub mod link_loop;
pub mod link_serve;
pub mod link_wire;
pub mod owners;
pub mod reconcile;
mod reconcile_claim;
pub mod reconcile_gate;
pub mod reconcile_restore;
pub mod reconnect;
pub mod session_reconcile;
pub mod session_stack;
pub mod snapshot_source;
pub mod stop;

// The crate-root contract the CLI calls: `serve` blocks until the worker is
// asked to stop, and `WorkerBoot` is the already-resolved configuration it
// takes. Both re-exported here so a host depends on `runtime`, not on the
// shape of the module tree behind it.
pub use boot::{WorkerBoot, WorkerOverrides};

// The boot sequence names these as it runs, and they are re-exported so a
// caller reading a refusal does not have to know which module owns the name it
// is reading.
pub(crate) use crate::event_store::database::{DATABASE_FILE_NAME, Journal};

use anyhow::Context as _;
use stop::{StopRequests, stop_requests_from_signals};

/// Run the worker until it is asked to stop.
///
/// Blocks on its own runtime and RETURNS when the daemon stops; it never ends
/// the process. `roost-cli` runs its own shutdown and exit-code handling around
/// this, and a library that ends the process cannot be called from a test or
/// from a subcommand.
pub fn serve(boot: WorkerBoot) -> anyhow::Result<()> {
    // `block_on` from inside a runtime panics, and a caller that already has one
    // is `roost-cli`. Saying so is better than a panic inside a subcommand.
    if tokio::runtime::Handle::try_current().is_ok() {
        anyhow::bail!("serve owns its runtime; call serve_until from inside one");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("the worker's async runtime could not be built")?;
    let stop = stop_requests_from_signals()?;
    runtime.block_on(serve_until(boot, stop))
}

/// Run the worker until the given requester asks it to stop.
///
/// The seam a caller drives when it owns the runtime and the signals: hand in a
/// [`StopRequests`] rather than relying on a signal handler, and the reason the
/// run ended is the one that was requested.
pub async fn serve_until(boot: WorkerBoot, stop: StopRequests) -> anyhow::Result<()> {
    boot.check()?;
    install_observability();
    boot_sequence::run(boot, stop).await
}

/// Install the JSON-lines log subscriber, or accept that one is already there.
///
/// The second case is `roost-cli`, which installs its own so a subcommand's
/// output is uniform. A subscriber that is already installed is a success for
/// everything this function is for, so the only thing worth saying about it is
/// why it happened.
fn install_observability() {
    if let Err(error) = roost_observability::init() {
        tracing::debug!(?error, "a global log subscriber was already installed");
    }
}
