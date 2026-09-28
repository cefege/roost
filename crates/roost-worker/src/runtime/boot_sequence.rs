//! The boot sequence itself: the ordered steps, in one function, so the order
//! is a thing a reader reads top to bottom rather than something they infer
//! from which helper happens to be called first. `runtime::serve_until` is the
//! only caller.
//!
//! The order is [`super::boot_order::BOOT_ORDER`] and it is not negotiable, and
//! the two orderings that look surprising are both forced by a DATA
//! dependency rather than chosen. The door opens before the link because a
//! browser on this machine reaches its own PTYs through it and has to keep
//! doing that while the coordinator is unreachable. The link is CONSTRUCTED
//! after the keeper because the object it dials with dispatches browser
//! commands into the session layer, and the session layer is built over the
//! keeper pool — while the decision that admits the keeper needed the
//! coordinator's open-session set, which is read over Connect at step 5 and
//! needs no socket of ours. The link DIALS before the boot reconcile (v2
//! `main.ts`), because every pass waits for the durable replay before it reads
//! the coordinator's recovery state; its snapshot is held until that pass ends.
//!
//! Every step logs. A boot that cannot say where it got to is a boot whose
//! refusal an operator has to guess at, and this one refuses in more places
//! than it proceeds.

use std::sync::Arc;

use anyhow::Context as _;
use roost_host::env::EnvSource as _;
use roost_host::{ProcessEnv, supported_host_platform};
use roost_observability::clock::EventClock as _;

use super::boot::WorkerBoot;
use super::boot_order::{BootSequence, StepId};
use super::bootstrap_redeem::activation;
use super::bootstrap_redeem::enroll_this_activation;
use super::credential::WorkerKeyCredential;
use super::door_serve::{DoorConfig, LocalDoor};
use super::link_loop::{BrowserLink, LinkLoop, WorkerIdentity};
use super::link_wire::ProtoLinkWire;
use super::reconcile;
use super::session_stack::{self, SessionStack};
use super::snapshot_source::SessionSnapshot;
use super::stop::StopRequests;
use crate::link_dial::CoordinatorEndpoint;

/// Run the ordered boot and then the link, until the requester asks to stop.
pub(super) async fn run(boot: WorkerBoot, stop: StopRequests) -> anyhow::Result<()> {
    let mut sequence = BootSequence::new();
    tracing::info!(
        fingerprint = %boot.fingerprint,
        version = %boot.worker_version,
        process_epoch = %boot.process_epoch,
        coordinator = %boot.coordinator_base,
        keeper_socket = %boot.keeper_socket.display(),
        "the worker is starting"
    );

    // 1. Identity, settled by `serve_until` before anything was probed or
    //    spawned. The step is recorded so the log says where the refusals
    //    happened.
    let because = sequence
        .complete(StepId::Identity)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
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
    //    One client for every boot-time call, so the scheme refusal and the
    //    base-URL parse happen once. Built BEFORE enrollment, so a coordinator
    //    this worker cannot dial is refused before a token is spent against it,
    //    and reused by the open-session read below.
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

    // 3. The local door, and it is HERE because of what it is for rather than
    //    where it is convenient: a browser on this machine reaches its own PTYs
    //    through the door, and it has to keep doing that while the coordinator
    //    is unreachable. v2 opens it at `main.ts:158`, before the link at
    //    `:173`, and a worker that opened it only once the link was up takes the
    //    local terminal away exactly when the link is the thing that is broken.
    //    A door that cannot be opened is a BOOT REFUSAL naming the door, never a
    //    worker that came up without one. The ROUTES on it are `crate::door`'s
    //    to mount; what this step owns is the bind and the refusal.
    let platform = supported_host_platform()
        .map_err(|error| anyhow::anyhow!("this host's platform is not one v3 runs on: {error}"))?;
    crate::agents::install_integrations::install_agent_integrations_at_boot(platform).await;
    let door = LocalDoor::bind(Some(&door_bind())).await?;

    // 4. The durable outbox, opened BEFORE anything can write a session event
    //    and before the link is told about it. Attaching is what makes the
    //    barrier resume at the outbox's high water mark: a link that starts its
    //    sequence at 1 while rows it must replay are numbered from a higher one
    //    would wait for ever for an acknowledgement it never issued. A store
    //    that cannot be opened is a boot refusal, not a warning — a worker that
    //    accepted sessions it could not record would leave the coordinator
    //    believing a dead session is alive.
    let (outbox, outbox_path) = super::boot_outbox::open_outbox(&boot.data_dir).await?;

    // 5. The coordinator's COMPLETE open-session set, and then the keeper, in
    //    THAT ORDER. Both halves are `reconcile::admit_keeper`, so the order
    //    cannot be got wrong HERE: `ensure_keeper` takes the open-session
    //    count, an unread count authorises nothing, and these used to be
    //    separable lines in this function with a closure between them that
    //    shipped untested because it was an `unwrap_or_else` two lines wide.
    //    The why is `admit_keeper`'s own doc; the ROWS travel with it because
    //    step 9 needs this session's id and folder to adopt it INTO, and a
    //    second read would be a second answer arriving after the first was
    //    spent. One read, both consumers.
    let admission = reconcile::admit_keeper(
        &boot,
        &coordinator_client,
        // The same key the link below dials with. The client built above
        // attaches nothing to itself, so without this the read is refused by a
        // coordinator that answers `SessionsList` to a worker principal only.
        &WorkerKeyCredential::new(boot.worker_key_path.clone()),
    )
    .await?;
    let keeper_admitted = matches!(admission, reconcile::KeeperAdmission::Owned(_));

    let because = sequence
        .complete(StepId::KeeperAdmission)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    tracing::info!(
        step = StepId::KeeperAdmission.name(),
        because,
        keeper = keeper_admitted,
        "boot: keeper admitted"
    );

    // The keeper has been admitted, so the flag that authorised a destructive
    // retirement has done its work. A value left in the unit re-authorizes
    // destroying every PTY on each later restart, and this is the only moment
    // at which the authorisation is known to have been spent.
    crate::host::install::spend_keeper_force_live_retire_authorization(&ProcessEnv::new()).await;

    // 6. The session layer, over the pool above: the manager needs the pool,
    //    the pool needs the keeper, and the keeper needed the coordinator's
    //    open-session set — which is why the link is CONSTRUCTED here and not
    //    before (`link.run` below is what dials).
    //    A HELD KEEPER IS A BOOT REFUSAL: a link with no session layer answers
    //    no browser command, adopts no survivor, and publishes a snapshot of
    //    nothing, all silently. The refusal is a fact about the session layer,
    //    so it is stated here rather than at the admission in step 5.
    let reconcile::KeeperAdmission::Owned(reconciled) = admission else {
        anyhow::bail!(
            "the keeper endpoint is held by a process this worker could not admit, and a \
             worker with no keeper has no PTY owner: every browser command would be \
             refused, no survivor could be adopted, and the snapshot would publish an \
             empty session set"
        );
    };
    let pool = reconciled.pool;
    let process = reconciled.process;
    let keeper = Some(reconciled.keeper);

    // The stack takes ownership of the pool, and the adoption needs the same
    // object to read the keeper's channel list through. A CLONE of the handle
    // rather than a second connection: the keeper socket is one, its dispatch
    // loop is one, and a second client would be a second frame stream.
    let survivors = Arc::clone(&pool);
    // `platform` is NOT passed: nothing in the stack branches on it, and the
    // two capabilities that do — the file surface and the attachments — take
    // it through `SessionStack::deps` below, which is where it is used.
    let stack: SessionStack = session_stack::build(
        boot.fingerprint.clone(),
        pool,
        Arc::clone(&outbox),
        &boot.data_dir,
        &boot.log_dir,
        boot.fingerprint.as_str().to_owned(),
        &boot.process_epoch,
        &boot.agent_report,
    )?;

    // The channel-id counter moves PAST what the keeper still holds before any
    // spawn can ask for one. A fresh worker's counter begins at zero and the
    // keeper rejects a channel id it is already serving, so without this a
    // restart would spend its first spawn on a refusal — and, worse, a counter
    // that walked up into an orphan's range would mint an id the orphan's PTY
    // is already answering to.
    match stack.manager.advance_past_keeper() {
        Ok(true) => tracing::info!(
            "boot: channel ids advanced past the keeper's survivors, so a fresh spawn \
             cannot collide with an orphaned PTY"
        ),
        Ok(false) => tracing::info!("boot: the keeper holds no channel, so no id was skipped"),
        Err(fault) => tracing::warn!(
            %fault,
            "boot: the keeper's channel list could not be read, so a spawn may be refused \
             for an id the keeper is already serving"
        ),
    }

    // The browser-command pump is connected to the ONE production `Deps` now
    // that the manager exists to answer through. It used to be `detached()`,
    // which refuses every command with a cause — honest, and a capability that
    // answers in tests and refuses in production is the defect this whole tree
    // is about.
    let deps = Arc::new(stack.deps(platform));
    // One uplink for the process: every owner that puts a frame on the
    // coordinator link sends through a clone of it, and the link loop owns the
    // receiving half, so the link stays the only writer of bytes.
    let (uplink, uplink_rx) = crate::uplink::channel();
    let browser = BrowserLink::connect(Arc::clone(&deps), uplink.clone());
    // The downstream owners, the cell cadence and the query-reply writer, over
    // the stack (moved in; reached as `owners.stack` from here on).
    let mut owners = super::owners::WorkerOwners::build(
        stack,
        &uplink,
        &boot.process_epoch,
        Arc::clone(&survivors),
        boot.fingerprint.as_str(),
        platform,
        boot.terminal_peer,
        super::reconcile_gate::ReconcileInputs {
            boot: boot.clone(),
            process,
            sessions: Arc::new(reconcile::CoordinatorOpenSessions::new(
                coordinator_client.clone(),
                boot.fingerprint.as_str(),
                Arc::new(WorkerKeyCredential::new(boot.worker_key_path.clone())),
            )),
            stop: stop.clone(),
            platform,
        },
    )?;
    let door = door.serve(&DoorConfig::for_boot(&boot), owners.loopback_routes())?;

    // 7. The link, over the session layer's snapshot source. `SessionSnapshot`
    //    is ALWAYS ACTIVE, and that is the point: this worker has a session
    //    table and can describe it even when the set is empty, and an empty set
    //    is a CLAIM it is entitled to make.
    let mut link = LinkLoop::new(
        CoordinatorEndpoint::new(boot.coordinator_base.clone(), boot.fingerprint.as_str())?,
        WorkerIdentity {
            worker_fp: boot.fingerprint.clone(),
            version: boot.worker_version.clone(),
            process_epoch: boot.process_epoch.clone(),
        },
        Arc::new(ProtoLinkWire),
        Arc::new(SessionSnapshot::new(
            boot.fingerprint.clone(),
            Arc::clone(&owners.stack.table),
            owners.stack.clock.now_epoch_ms(),
        )),
        Arc::new(WorkerKeyCredential::new(boot.worker_key_path.clone())),
        browser,
        uplink_rx,
    );

    // v2 `coordLinkSink`: every row the outbox holds (a restart's unacked ones
    // included) is replayed under its stored sequence, and each new one reaches
    // the link through the sink's change signal.
    tracing::info!(outbox = %outbox_path.display(), "the coordinator link replays the durable outbox");
    link.attach_durable_outbox(outbox, Arc::clone(&owners.stack.durable_delivery));
    // The SAME sink the cadence registered with the emitter, so a cell the
    // emitter hands it is the cell this link drains.
    link.attach_cell_sink(Arc::clone(&owners.coord_sink));
    link.attach_owners(owners.downstream.clone());
    link.attach_direct_peers(owners.direct_peer_support().await);

    // 8. The link dials NOW, before the boot reconcile, and its snapshot is
    //    held: the pass waits for the durable replay this link performs before
    //    it reads the coordinator's recovery state (v2 `beforeRecoveryRead`),
    //    and the snapshot describes the set that pass reserves (v2 activates
    //    the provider only after it, `main.ts:296-303`).
    let snapshot = link.hold_snapshot_until_activated();
    let because = sequence
        .complete(StepId::CoordinatorLink)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    tracing::info!(
        step = StepId::CoordinatorLink.name(),
        because,
        "boot: the coordinator link dials and replays; its snapshot waits for the boot reconcile"
    );
    // The link runs to completion in this task: the loop owns every reconnect,
    // and the run ends when something that can END it asks.
    let link_task = tokio::spawn(link.run(stop.subscribe()));

    // 9-10. Survivors through the one reconcile gate, then snapshot activation,
    //    then readiness (`boot_admission`). Any refusal is a boot refusal (v2
    //    `completeWorkerBootAdmission` throws), and a link that never
    //    published a snapshot goes with it.
    let admitted = super::boot_admission::complete_worker_boot_admission(
        &owners.reconcile,
        &snapshot,
        &mut sequence,
    )
    .await;
    let readiness = match admitted {
        Ok((_, readiness)) => readiness,
        Err(refusal) => {
            link_task.abort();
            return Err(refusal);
        }
    };
    tracing::info!(
        live_sessions = owners.stack.table.live().len(),
        "boot: the snapshot publishes the reconciled session set"
    );
    if let Err(error) = owners.heart.start_heartbeat(&boot) {
        link_task.abort();
        return Err(error);
    }
    let ended = link_task.await;
    let reason = match &ended {
        Ok(reason) => reason.to_string(),
        Err(error) => format!("the coordinator link task failed: {error}"),
    };

    tracing::info!(
        reason = %reason,
        readiness = ?readiness,
        completed = ?sequence.completed(),
        door = %door.origin(),
        "the worker is stopping"
    );
    // The keeper handle is dropped here, not killed. For
    // the keeper, dropping the connection is the whole contract: it treats a
    // disconnect as a reason to keep serving, and a worker that took its PTYs
    // down on the way out would be the one bug this architecture exists to
    // prevent. The door stops serving first (v2 `close()`), then the owners
    // release their sockets, routes and cadence.
    owners.close_agent_report().await;
    door.close();
    owners.shutdown();
    drop(keeper);
    ended
        .map(drop)
        .map_err(|error| anyhow::anyhow!("the coordinator link task failed: {error}"))
}

/// The address the door binds, from the environment or the shared default.
///
/// Read HERE, at the moment the bind needs it, and not resolved into
/// `WorkerBoot`: the door is the one collaborator whose address an operator
/// changes without changing anything else about the worker, and a value frozen
/// into the boot configuration would be a second place to change it.
fn door_bind() -> String {
    let environment = ProcessEnv::new();
    // No `.map(str::to_owned)` here: `EnvSource::get` already returns an
    // owned `String`, so the extra map asked `str::to_owned` to take a `String`
    // where it wanted a `&str`.
    environment
        .get(super::door_serve::ENV_DOOR_BIND)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| roost_protocol::local_ui_door::DEFAULT_WORKER_LOCAL_UI_BIND.to_string())
}
