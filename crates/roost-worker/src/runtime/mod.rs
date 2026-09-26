//! The worker as a service: boot order, the keeper it owns, the coordinator
//! link, and the one rule that a dropped link is not a stopped process.
//! `serve` is the entry point; `roost-cli` and the `roost-worker` binary call
//! it and nothing else here.
//!
//! The boot order is [`boot_order::BOOT_ORDER`] and it is not negotiable. The
//! identity is settled before anything is probed or spawned, the keeper's
//! survivor is admitted before a session is touched, and readiness is announced
//! last because readiness is a claim about the steps before it.
//!
//! Two rules span every module here. A coordinator disconnect is a reconnect,
//! never a shutdown: the keeper holds the PTYs and outlives this process on
//! purpose, so the only things that end the worker are a signal and an explicit
//! shutdown frame. And nothing here may mutate a survivor it cannot prove empty
//! — see [`keeper_boot::decide`], which is a pure function precisely so that
//! decision can be tested without a keeper, a coordinator, or a PTY.

pub mod boot;

// The crate-root contract the CLI calls: `serve` blocks until the worker is
// asked to stop, and `WorkerBoot` is the already-resolved configuration it
// takes. Both re-exported here so a host depends on `runtime`, not on the
// shape of the module tree behind it.
pub use boot::{WorkerBoot, WorkerOverrides};
pub mod boot_order;
pub mod credential;
pub mod keeper_boot;
pub mod keeper_probe;
pub mod link_drain;
pub mod link_loop;
pub mod link_serve;
pub mod link_wire;
pub mod reconnect;
pub mod snapshot_source;
pub mod stop;

use std::sync::Arc;

use anyhow::Context as _;


use boot_order::{BootSequence, Readiness, StepId};
use credential::UnavailableCredential;
use keeper_boot::KeeperBootOutcome;
use crate::link_dial::CoordinatorEndpoint;
use link_loop::{LinkLoop, WorkerIdentity};
use link_wire::UnavailableWire;
use snapshot_source::NoSnapshot;
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
    let mut sequence = BootSequence::new();
    // Never advanced in this build. The two steps that would advance it are the
    // session reconcile and the snapshot provider, and both need a session layer
    // this crate does not have yet — see the TODO at the call site.
    let readiness = Readiness::default();
    tracing::info!(
        fingerprint = %boot.fingerprint,
        version = %boot.worker_version,
        process_epoch = %boot.process_epoch,
        coordinator = %boot.coordinator_base,
        keeper_socket = %boot.keeper_socket.display(),
        "the worker is starting"
    );

    // 1. Identity, settled above before anything was probed or spawned. The
    //    step is recorded so the log says where the refusals happened.
    let because = sequence.complete(StepId::Identity);
    tracing::info!(
        step = StepId::Identity.name(),
        because,
        "boot: identity settled"
    );

    // 2. The keeper. `None` for the coordinator's open-session set, because
    //    nothing has read it: the link below is what would read it, and
    //    `keeper_boot::decide` treats an unread set as "do not replace". That is
    //    the safe direction — a replacement made on an assumed-empty set is how a
    //    restart kills a user's terminals — and it still lets the two cases that
    //    need no coordinator happen: adopting a survivor, and starting a keeper
    //    when nothing is listening at all.
    let keeper = match keeper_boot::ensure_keeper(&boot, None, &boot.log_dir).await {
        Ok(KeeperBootOutcome::Adopted { channels, keeper }) => {
            tracing::info!(
                ?channels,
                "boot: adopted the keeper that already holds this machine's terminals"
            );
            Some(keeper)
        }
        Ok(KeeperBootOutcome::StartedFresh { keeper }) => {
            tracing::info!("boot: started a fresh keeper");
            Some(keeper)
        }
        Ok(KeeperBootOutcome::Held { decision }) => {
            tracing::warn!(
                ?decision,
                "boot: the keeper endpoint is held and nothing was touched; replacing it waits \
                 for the coordinator's open-session set"
            );
            None
        }
        Err(error) => {
            tracing::error!(%error, "boot refused: the keeper endpoint could not be admitted");
            return Err(error);
        }
    };
    let because = sequence.complete(StepId::KeeperAdmission);
    tracing::info!(
        step = StepId::KeeperAdmission.name(),
        because,
        "boot: keeper admitted"
    );

    // TODO(roost-phase2): the local door, the session manager, agent tracking and
    // the heartbeat, in v2's order between here and the link. The local door
    // comes before the link in v2 and must here too: a browser on this machine
    // reaches its own PTYs through the door, and it has to keep doing that while
    // the coordinator is unreachable.

    // 3. The coordinator link, after the keeper rather than before it, because
    //    the keeper is what holds the terminals and a coordinator outage must not
    //    cost this process them.
    let endpoint =
        CoordinatorEndpoint::new(boot.coordinator_base.clone(), boot.fingerprint.as_str())?;
    let link = LinkLoop::new(
        endpoint,
        WorkerIdentity {
            worker_fp: boot.fingerprint.clone(),
            version: boot.worker_version.clone(),
            process_epoch: boot.process_epoch.clone(),
        },
        Arc::new(UnavailableWire),
        Arc::new(NoSnapshot),
        Arc::new(UnavailableCredential),
    );
    let because = sequence.complete(StepId::CoordinatorLink);
    tracing::info!(
        step = StepId::CoordinatorLink.name(),
        because,
        keeper = keeper.is_some(),
        "boot: the coordinator link is starting"
    );

    // TODO(roost-phase2): reconcile the coordinator's open-session set against
    // the local one, activate the snapshot provider, and only then advance
    // `readiness` through `Readiness::advance`. Both need the link to be live and
    // a session layer to reconcile, so until they exist those two steps are
    // refused rather than skipped — see `BOOT_ORDER`. `serve_until` deliberately
    // does not call `Readiness::advance(ReadyStep::Reconciled)`: a worker that
    // announces readiness it cannot back is worse than one that never does.
    let reason = link.run(stop.subscribe()).await;

    tracing::info!(
        reason = %reason,
        readiness = ?readiness,
        completed = ?sequence.completed(),
        keeper = keeper.is_some(),
        "the worker is stopping"
    );
    // The keeper handle is dropped here, not killed. Dropping the connection is
    // the whole contract: the keeper treats a disconnect as a reason to keep
    // serving, and a worker that took its PTYs down on the way out would be the
    // one bug this architecture exists to prevent.
    drop(keeper);
    Ok(())
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
