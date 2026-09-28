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
//! needs no socket of ours.
//!
//! Every step logs. A boot that cannot say where it got to is a boot whose
//! refusal an operator has to guess at, and this one refuses in more places
//! than it proceeds.

use std::sync::Arc;

use anyhow::Context as _;
use roost_host::{ProcessEnv, supported_host_platform};
use roost_host::env::EnvSource as _;
use roost_observability::clock::EventClock as _;

use super::boot::{WorkerBoot};
use super::boot_order::{BootSequence, Readiness, ReadyStep, StepId};
use super::bootstrap_redeem::activation;
use super::bootstrap_redeem::enroll_this_activation;
use super::credential::WorkerKeyCredential;
use super::door_serve::LocalDoor;
use super::link_loop::{BrowserLink, CoordinatorCellSink, LinkLoop, WorkerIdentity};
use super::link_wire::ProtoLinkWire;
use super::reconcile;
use super::session_stack::{self, SessionStack};
use super::snapshot_source::SessionSnapshot;
use super::stop::StopRequests;
use super::{DATABASE_FILE_NAME, Journal};
use crate::link_dial::CoordinatorEndpoint;

/// Run the ordered boot and then the link, until the requester asks to stop.
pub(super) async fn run(boot: WorkerBoot, stop: StopRequests) -> anyhow::Result<()> {
    let mut sequence = BootSequence::new();
    // Readiness is the boot's own claim about the steps before it, and it is a
    // state machine because the claim is about ORDER: reconciliation must
    // reserve every durable session before a snapshot publishes, so a failed
    // keeper adoption cannot expose a partial worker state. It is advanced at
    // the points that earn it, and `Readiness::advance` refuses any other order
    // rather than trusting this function to keep it.
    let mut readiness = Readiness::default();
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
    //    One client for every boot-time call, so the scheme refusal and the
    //    base-URL parse happen once. Built BEFORE enrollment, so a coordinator
    //    this worker cannot dial is refused before a token is spent against it,
    //    and reused by the open-session read below.
    let coordinator_client = activation::coordinator_client(&boot.coordinator_base).with_context(
        || {
            format!(
                "{} is not a coordinator this worker can dial",
                boot.coordinator_base
            )
        },
    )?;
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
    let door = LocalDoor::bind(Some(&door_bind())).await?;

    // 4. The durable outbox, opened BEFORE anything can write a session event
    //    and before the link is told about it. Attaching is what makes the
    //    barrier resume at the outbox's high water mark: a link that starts its
    //    sequence at 1 while rows it must replay are numbered from a higher one
    //    would wait for ever for an acknowledgement it never issued. A store
    //    that cannot be opened is a boot refusal, not a warning — a worker that
    //    accepted sessions it could not record would leave the coordinator
    //    believing a dead session is alive.
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
    let admission = reconcile::admit_keeper(&boot, &coordinator_client).await?;
    let keeper_admitted = matches!(admission, reconcile::KeeperAdmission::Owned(_));

    let because = sequence.complete(StepId::KeeperAdmission);
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

    // 6. The session layer, over the pool above. THIS IS WHERE THE ORDER IN
    //    THIS FUNCTION BECOMES VISIBLE: the manager needs the pool, the pool
    //    needs the keeper, and the keeper needed the coordinator's open-session
    //    set — which is why the link is CONSTRUCTED here and not before. The
    //    link's own DIAL is still after everything, and it is `link.run` below
    //    that opens the socket; what this step builds is the object that dial
    //    will use, and it cannot exist before the session layer it dispatches
    //    browser commands into.
    //    A HELD KEEPER IS A BOOT REFUSAL, and this is the change from the
    //    previous shape: that shape logged a warning and carried on to run a
    //    link with no session layer, which answers no browser command, adopts
    //    no survivor, and publishes a snapshot of nothing. All three fail
    //    silently, so the worker now refuses with a reason instead. The
    //    refusal stays HERE rather than in step 5 with the admission that
    //    produced it: it is a fact about the session layer below, and a boot
    //    that said it before building that layer would be explaining a
    //    consequence it had not reached.
    let reconcile::KeeperAdmission::Owned(reconciled) = admission else {
        anyhow::bail!(
            "the keeper endpoint is held by a process this worker could not admit, and a \
             worker with no keeper has no PTY owner: every browser command would be \
             refused, no survivor could be adopted, and the snapshot would publish an \
             empty session set"
        );
    };
    let pool = reconciled.pool;
    let open_rows = reconciled.open;
    let survivor_channels = reconciled.survivors;
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
    let deps = Arc::new(stack.deps(
        &boot.data_dir,
        &boot.log_dir,
        platform,
        boot.fingerprint.as_str(),
    ));
    let (browser, answers) = BrowserLink::connect(Arc::clone(&deps));

    // 7. The link, over the session layer's snapshot source. `SessionSnapshot`
    //    is ALWAYS ACTIVE, and that is the point: this worker has a session
    //    table and can describe it even when the set is empty, and an empty set
    //    is a CLAIM it is entitled to make. `NoSnapshot` — which refuses to
    //    describe anything and therefore holds the barrier at `snapshot` for
    //    ever — is what this replaced, and the link used to tear itself down
    //    within a dial of opening because of it.
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
            Arc::clone(&stack.table),
            stack.clock.now_epoch_ms(),
        )),
        Arc::new(WorkerKeyCredential::new(boot.worker_key_path.clone())),
        browser,
    );
    // The answers channel is the pump's own half of the pair `connect` returns.
    // The link selects on `link.browser.answers` rather than on this handle, and
    // holding it here is what keeps the sender alive for the run: a dropped
    // handle would end the channel the select is reading.
    let _answers = answers;

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

    // 8. The link records its step, and it is recorded AFTER the keeper because
    //    that is the order this function runs in: the object the link dials
    //    with is built over the session layer, which is built over the pool.
    //    `BOOT_ORDER` states the link before the keeper for the DECISION — the
    //    survivor decision needs the coordinator's open-session set — and that
    //    set is read at step 5 over Connect, not over this socket.
    let because = sequence.complete(StepId::CoordinatorLink);
    tracing::info!(
        step = StepId::CoordinatorLink.name(),
        because,
        "boot: the coordinator link is built and is about to dial"
    );

    // 9. Survivors. `advance_past_keeper` moved the id counter; this is the
    //    other half, and it is a DIFFERENT question: a channel the keeper still
    //    holds is a PTY this worker did not spawn, and adopting it rebuilds a
    //    record around a live terminal.
    //    `runtime::adoption` asks the keeper FIRST whether it can describe that
    //    terminal at all, and declines the ones it cannot — leaving them
    //    running rather than offering them to an adoption that would reattach
    //    their output behind a record it cannot build. A refusal there is
    //    counted and logged, never propagated: propagating would abort a boot
    //    over one undescribable terminal and take every other survivor and the
    //    worker's link down with it.
    let adopted = super::adoption::adopt_survivors(
        &stack,
        &survivors,
        &survivor_channels,
        &open_rows,
        &boot.keeper_socket.display().to_string(),
    )
    .await;
    tracing::info!(
        ?adopted,
        "boot: the keeper's survivors were reconciled against the session table"
    );

    // 10. Reconcile, snapshot activation, readiness — in that order and no
    //     other. The snapshot describes the session set the reconcile just
    //     reserved, so publishing it first is a snapshot of a set the
    //     coordinator has not confirmed, and acting on that closes live
    //     sessions. `Readiness::advance` refuses any other sequence rather than
    //     trusting this function, and an advance that fails is a BOOT REFUSAL
    //     for the same reason.
    let because = sequence.complete(StepId::SessionReconcile);
    tracing::info!(
        step = StepId::SessionReconcile.name(),
        because,
        live_sessions = stack.table.live().len(),
        "boot: the local session set is reconciled and reserved"
    );
    readiness = readiness
        .advance(ReadyStep::Reconciled)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    readiness = readiness
        .advance(ReadyStep::SnapshotProviderActivated)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    readiness = readiness
        .advance(ReadyStep::MarkedReady)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    let because = sequence.complete(StepId::Ready);
    tracing::info!(
        step = StepId::Ready.name(),
        because,
        readiness = ?readiness,
        "boot: this worker is ready, and readiness is a claim about every step above it"
    );

    // The link runs to completion here, which is the whole reason boot is a
    // function and not a loop of its own: the loop owns every reconnect, and
    // the run ends when something that can END it asks.
    let reason = link.run(stop.subscribe()).await;

    tracing::info!(
        reason = %reason,
        readiness = ?readiness,
        completed = ?sequence.completed(),
        door = %door.origin(),
        "the worker is stopping"
    );
    // The door listener and the keeper handle are dropped here, not killed. For
    // the keeper, dropping the connection is the whole contract: it treats a
    // disconnect as a reason to keep serving, and a worker that took its PTYs
    // down on the way out would be the one bug this architecture exists to
    // prevent.
    drop(door);
    drop(keeper);
    Ok(())
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
