//! The worker as a service: boot order, the keeper it owns, the coordinator
//! link, and the one rule that a dropped link is not a stopped process.
//! `serve` is the entry point; `roost-cli` and the `roost-worker` binary call
//! it and nothing else here.
//!
//! The boot order is [`boot_order::BOOT_ORDER`] and it is not negotiable. The
//! identity is settled before anything is probed or spawned; the link dials
//! BEFORE the keeper is admitted, because the survivor decision needs the
//! coordinator's open-session set and a set nobody has read cannot decide
//! anything; and readiness is announced last because readiness is a claim
//! about the steps before it.
//!
//! Two rules span every module here. A coordinator disconnect is a reconnect,
//! never a shutdown: the keeper holds the PTYs and outlives this process on
//! purpose, so the only things that end the worker are a signal and an explicit
//! shutdown frame. And nothing here may mutate a survivor it cannot prove empty
//! — see [`keeper_boot::decide`], which is a pure function precisely so that
//! decision can be tested without a keeper, a coordinator, or a PTY.

pub mod boot;
pub mod cell_delivery;

// The crate-root contract the CLI calls: `serve` blocks until the worker is
// asked to stop, and `WorkerBoot` is the already-resolved configuration it
// takes. Both re-exported here so a host depends on `runtime`, not on the
// shape of the module tree behind it.
pub use boot::{WorkerBoot, WorkerOverrides};
pub mod boot_order;
pub mod bootstrap_redeem;
pub mod credential;
pub mod deps;
pub mod keeper_boot;
pub mod keeper_probe;
pub mod link_drain;
pub mod link_loop;
pub mod link_serve;
pub mod link_wire;
pub mod reconcile;
pub mod reconnect;
pub mod snapshot_source;
pub mod stop;

use std::sync::Arc;

use anyhow::Context as _;
use roost_host::ProcessEnv;

use crate::event_store::database::{DATABASE_FILE_NAME, Journal};
use crate::keeper_pool::KeeperPool;
use crate::link_dial::CoordinatorEndpoint;
use boot_order::{BootSequence, Readiness, StepId};
use bootstrap_redeem::activation;
use bootstrap_redeem::enroll_this_activation;
use credential::WorkerKeyCredential;
use keeper_boot::KeeperBootOutcome;
use link_loop::{BrowserLink, CoordinatorCellSink, LinkLoop, WorkerIdentity};
use link_wire::ProtoLinkWire;
use reconcile::read_open_session_count;
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
    // session reconcile and the snapshot provider, and both need the session
    // MANAGER — the record, its sinks and the launch contract are in
    // `crate::session`, and the thing that owns a set of them is not.
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

    // 2. Enrollment, BEFORE the link dials and therefore before the keeper is
    //    admitted. A link that opens before the coordinator holds this
    //    machine's `authorized_keys` row sends its first frame to a
    //    coordinator that does not know the sender, and the retry that follows
    //    is a reconnect rather than a registration. Position is the whole
    //    property here: `enroll_this_activation` on the far side of the dial
    //    satisfies every test in the enrollment suite and is still the defect
    //    the suite was written to prevent.
    //
    // One client for every boot-time call, so the scheme refusal and the
    // base-URL parse happen once. Built BEFORE enrollment, so a coordinator
    // this worker cannot dial is refused before a token is spent against it,
    // and reused by the open-session read below.
    let coordinator_client =
        activation::coordinator_client(&boot.coordinator_base).with_context(|| {
            format!(
                "{} is not a coordinator this worker can dial",
                boot.coordinator_base
            )
        })?;
    let enrollment = enroll_this_activation(&boot)
        .await
        .context("this activation could not be enrolled")?;
    if enrollment.is_some() {
        tracing::info!(
            fingerprint = %boot.fingerprint,
            "boot: this machine is a member of the fleet before the link opens"
        );
    }

    // 3. The coordinator link, dialed BEFORE the keeper is admitted. v2 starts
    //    the link at `main.ts:173` and does not reach the survivor decision
    //    until `boot-session-reconcile.ts:174`, and the reason is the next
    //    step: the decision needs a fact only the coordinator has.
    let endpoint =
        CoordinatorEndpoint::new(boot.coordinator_base.clone(), boot.fingerprint.as_str())?;
    let mut link = LinkLoop::new(
        endpoint,
        WorkerIdentity {
            worker_fp: boot.fingerprint.clone(),
            version: boot.worker_version.clone(),
            process_epoch: boot.process_epoch.clone(),
        },
        Arc::new(ProtoLinkWire),
        Arc::new(NoSnapshot),
        Arc::new(WorkerKeyCredential::new(boot.worker_key_path.clone())),
        // The command pump is detached until this function builds the session
        // manager: there is no session layer yet, so a browser command is
        // refused with a cause rather than answered by a stub. The replacement
        // is one line — `BrowserLink::connect(deps)` over
        // `deps::WorkerCapabilities` — and it lands with the manager.
        BrowserLink::detached(),
    );

    // The durable outbox is opened BEFORE the link is told about it, because
    // attaching is what makes the barrier resume at the outbox's high water
    // mark: a link that starts its sequence at 1 while rows it must replay are
    // numbered from a higher one would wait for ever for an acknowledgement it
    // never issued. A store that cannot be opened is a boot refusal, not a
    // warning — a worker that accepted sessions it could not record would
    // leave the coordinator believing a dead session is alive.
    let outbox_path = boot.data_dir.join(DATABASE_FILE_NAME);
    let outbox = Arc::new(Journal::open(&outbox_path).await.with_context(|| {
        format!(
            "the durable outbox at {} could not be opened",
            outbox_path.display()
        )
    })?);
    // Logged, not propagated: a stats read that fails says the store is
    // answering, which is the only thing the line is for. The open above
    // already refused anything that is not.
    match outbox.stats().await {
        Ok(stats) => tracing::info!(
            path = %outbox_path.display(),
            rows = stats.rows,
            resumed_at = outbox.handed_over_at(),
            "the durable outbox is open and the barrier resumes at its high water mark"
        ),
        Err(error) => tracing::warn!(
            path = %outbox_path.display(),
            %error,
            "the durable outbox is open but its row count could not be read"
        ),
    }
    // An unaligned barrier is a boot refusal, not a warning: rows written under
    // a sequence the barrier will not issue are worse than rows never written,
    // and a barrier that cannot be aligned wedges in `replay` for ever.
    link.attach_durable_outbox(outbox).with_context(|| {
        format!(
            "the link barrier could not be aligned to the outbox at {}",
            outbox_path.display()
        )
    })?;
    link.attach_cell_sink(Arc::new(CoordinatorCellSink::new(Arc::new(ProtoLinkWire))));

    // 4. The coordinator's COMPLETE open-session set, and then the keeper. In
    //    THAT ORDER, and the ordering is the fix: `ensure_keeper` used to be
    //    called with `None` here, and under the old boot order that `None` was
    //    not a gap a later step filled — it was permanent. `decide` reads an
    //    unread set as "do not replace", so a machine whose keeper genuinely
    //    needed replacing never replaced it, forever, and no test noticed
    //    because the refusal is the safe direction.
    //    `open_sessions_or_unknown` is `reconcile`'s, not a closure written
    //    here, so the "an unanswered coordinator is not an empty one" half of
    //    that decision is something a test can call. It shipped untested
    //    precisely because it was an `unwrap_or_else` two lines wide.
    let open_sessions = reconcile::open_sessions_or_unknown(
        read_open_session_count(&coordinator_client, boot.fingerprint.as_str()).await,
    );
    let keeper = match keeper_boot::ensure_keeper(&boot, open_sessions, &boot.log_dir).await {
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
                "boot: the keeper endpoint is held and nothing was touched"
            );
            None
        }
        Err(error) => {
            tracing::error!(%error, "boot refused: the keeper endpoint could not be admitted");
            return Err(error);
        }
    };

    // The pool is built from the admitted keeper HERE, and its dispatch loop
    // starts inside `KeeperPool::new` — which is before any history is read.
    // The keeper streams `PtyOut` from the moment a worker connects, and an
    // unbound frame is dropped at `keeper_pool/dispatch.rs`, so a pool that
    // started after the first history read would lose the bytes a survivor
    // produced during the read. Nothing reports that loss: the frames were
    // never expected by anyone.
    let pool = keeper
        .as_ref()
        .map(|handle| KeeperPool::new(handle.clone()));

    let because = sequence.complete(StepId::KeeperAdmission);
    tracing::info!(
        step = StepId::KeeperAdmission.name(),
        because,
        keeper = keeper.is_some(),
        "boot: keeper admitted"
    );

    // The keeper has been admitted, so the flag that authorised a destructive
    // retirement has done its work. A value left in the unit re-authorizes
    // destroying every PTY on each later restart, and this is the only moment
    // at which the authorisation is known to have been spent.
    crate::host::install::spend_keeper_force_live_retire_authorization(&ProcessEnv::new()).await;

    // The link records its step last of the two, because it is the step that
    // made step 4 possible: the order is identity, link, keeper, and the
    // `because` on the link row says so.
    let because = sequence.complete(StepId::CoordinatorLink);
    tracing::info!(
        step = StepId::CoordinatorLink.name(),
        because,
        "boot: the coordinator link is starting"
    );

    // UNIMPLEMENTED: the local door (`crate::door` over `crate::local_door`'s
    // policy), the session manager (`crate::session`, built over `pool` above
    // with its four production collaborators), agent tracking
    // (`crate::agents`) and the heartbeat, in v2's order. The local door comes
    // before the link in v2 and must here too: a browser on this machine
    // reaches its own PTYs through the door, and it has to keep doing that
    // while the coordinator is unreachable.

    // The link runs to completion here, which is the whole reason boot is a
    // function and not a loop of its own: the loop owns every reconnect, and
    // the run ends when something that can END it asks.
    let reason = link.run(stop.subscribe()).await;

    if pool.is_some() {
        tracing::info!("boot: the keeper pool's dispatch loop ends with the link");
    }

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
