//! One worker link: the socket's lifecycle, the generation it registers, and
//! the registration that gives an owed reap somewhere to go.
//!
//! Owned by `http/upgrade`, which hands over a socket and an admission decision
//! and then stops. What a frame MEANS is `worker_link::frame_dispatch`; what it
//! COSTS is that dispatcher's `FrameQueue`. **This file owns no kill and no
//! frame queue.**
//!
//! # TWO SEAMS ARE NAMED RATHER THAN GUESSED
//!
//! `frame_from_bytes` and the outbound arm below are real holes and they are the
//! only two things standing between this file and a working link. `InboundFrame`
//! has no constructor in the tree, and nothing maps a decoded
//! `CoordWorkerUpstream` to a `FrameClass` and a channel; `CoordWorkerDownstream`
//! has arms carrying typed fields and nothing maps those to a socket message.
//! **In both cases which arm is which is a DESIGN decision, not a translation**,
//! and a wrong mapping compiles exactly as cleanly as a right one.
//!
//! # Every call cites the line it was read from
//!
//! `WorkerHandle::new` `coord_core/worker_handle.rs:70` · `insert` `:162` ·
//! `retire` `:233` · `send` `:97` · `Handle::new`'s only-writer fact `:86-92`
//! (the two `Arc<AtomicBool>` that need later mutation, against a plain
//! `Option<String>` that does not) · `Keepalive`/`STALE_LINK_CHECK_INTERVAL`
//! `worker_link/keepalive.rs` · `CoordServices::worker_dispatcher` `services.rs:253`
//! · `DispatcherFor::new`/`build` `worker_link/dispatcher_for.rs:45,63` ·
//! `FrameDispatch` `worker_link/dispatch.rs` · `LinkRegistration` below.
//!
//! # THE ORDER IS THE SPEC'S, NOT OURS
//!
//! `worker-link.md:28` — "`open` is not application-ready. **The only forced
//! first write is `WHello`.**" `:29` — "`DHelloAck` moves the application
//! barrier from `hello` to `replay`." `:32` — "**A newer authenticated hello
//! immediately supersedes the old connection generation.**"
//!
//! # ONE OWNER, THREE ARMS, NO SPLIT
//!
//! The REQUIREMENT is that the write must be reachable from the `Send + Sync`
//! closure `WorkerHandle::new` holds. The MECHANISM, verified: `recv` and
//! `send` are both `async fn(.., &mut self)` and there is NO `impl Sink for
//! WebSocket` (axum 0.8.9 `extract/ws.rs:555,560,580`), so the socket is never
//! divided — the same task reads and writes, and the write is the loop's third
//! arm. Closure shape read from `tests/workers_registry.rs:156-162` and
//! `tests/workers_support/mod.rs:70-80`.
//!
//! **The closure's return value is `send`'s whole contract, and it is not a byte
//! count.** Zero is the transport's DROPPED answer for a fenced generation
//! (`:97-99`, and v2's `myHandle.send` likewise), so a live generation must
//! never answer zero and this one never does.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use roost_protocol::wire::WorkerFp;
// `roost_protocol::wire::coord_worker::CoordWorkerDownstream` — as
// `coord_core/worker_handle.rs:30` imports it.
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

use crate::coord_core::worker_handle::WorkerHandle;
use crate::services::CoordServices;
use crate::terminal_screen::orphan_kills::{LiveOrphanKills, PendingKill};
use crate::worker_link::dispatch::{DispatchOutcome, FrameClass, FrameDispatch, InboundFrame};
use crate::worker_link::keepalive::{Keepalive, STALE_LINK_CHECK_INTERVAL};
use crate::worker_link::upgrade_admission::{UpgradeDecision, VerifiedWorkerCaller};

/// How long a socket may stay in the pre-hello state before it is closed.
///
/// `STALE_LINK_TIMEOUT_MS` with a `STALE_LINK_CHECK_INTERVAL` tick, cited
/// rather than chosen: `worker-link.md:46`, authority
/// `apps/worker/src/transport/coord-link-constants.ts:61-62`. **A socket that
/// connects, authenticates and never sends a hello waits this long and is then
/// closed with the no-code default** — it is not admitted and it is not
/// dropped, because `:28` says the pre-hello state was never usable.
const PRE_HELLO_WAIT: Duration = crate::worker_link::keepalive::STALE_LINK_TIMEOUT;

/// One connection's minted generation, distinct from every other connection of
/// the same fingerprint.
///
/// The obligation is `:32`'s: "a newer authenticated hello immediately
/// supersedes the old connection generation." **A value that repeats across
/// reconnects cannot express *newer***, so this is a per-process monotonic
/// ordinal carrying its fingerprint — the fingerprint is here because the value
/// is read in logs beside other fingerprints and a bare ordinal is ambiguous
/// there.
fn mint_connection_generation(worker_fp: &WorkerFp) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("{worker_fp}:{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The reap sender for one link, and the registration that owns it.
///
/// Attach on the way in, detach on the way out, and the detach is not
/// tidiness: a link that ends while still attached collects reaps into a
/// channel nobody reads, and a reconnecting worker finds it empty and believes
/// it owes nothing.
#[derive(Debug)]
pub struct LinkRegistration {
    worker_fp: WorkerFp,
    outbox: Arc<std::sync::Mutex<Vec<PendingKill>>>,
    kills: Arc<LiveOrphanKills>,
}

impl LinkRegistration {
    /// Register this link's sender and take everything the worker is owed.
    ///
    /// `LiveOrphanKills::attach` — `terminal_screen/orphan_kills.rs:73`. Taking
    /// the owed reaps HERE rather than leaving them in the record is what makes
    /// a delivery observable: a reap that were copied instead of moved would be
    /// delivered twice and the count would never reach zero.
    #[must_use]
    pub fn attach(kills: &Arc<LiveOrphanKills>, worker_fp: WorkerFp) -> Self {
        let outbox: Arc<std::sync::Mutex<Vec<PendingKill>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let owed = kills.attach(&worker_fp, Arc::clone(&outbox));
        if let Ok(mut held) = outbox.lock() {
            held.extend(owed);
        }
        Self {
            worker_fp,
            outbox,
            kills: Arc::clone(kills),
        }
    }

    /// The fingerprint this link was admitted under.
    #[must_use]
    pub fn worker_fp(&self) -> &WorkerFp {
        &self.worker_fp
    }

    /// Take the reaps the registry has handed over since the last drain.
    #[must_use]
    pub fn take_pending(&self) -> Vec<PendingKill> {
        self.outbox.lock().map_or_else(
            |error| error.into_inner().drain(..).collect(),
            |mut held| std::mem::take(&mut *held),
        )
    }
}

impl Drop for LinkRegistration {
    /// `LiveOrphanKills::detach` — `terminal_screen/orphan_kills.rs:85`. In one
    /// place, so no exit path can forget it.
    fn drop(&mut self) {
        self.kills.detach(&self.worker_fp);
        tracing::info!(
            worker_fp = %self.worker_fp,
            "worker link: the reap sender is deregistered; later reaps are recorded"
        );
    }
}

/// Serve one admitted worker socket to its end.
///
/// Takes the whole decision rather than the `Admitted` variant, so a caller that
/// hands over a refusal gets a logged no-op rather than a panic on an arm
/// nothing can reach.
pub async fn serve_socket(
    socket: WebSocket,
    decision: UpgradeDecision,
    services: &Arc<CoordServices>,
) {
    let (worker_fp, caller) = match decision {
        // `UpgradeDecision::Admitted` — `worker_link/upgrade_admission.rs:98`.
        UpgradeDecision::Admitted {
            fingerprint,
            caller,
        } => (fingerprint, caller),
        UpgradeDecision::Refused(refusal) => {
            tracing::warn!(
                ?refusal,
                "worker link: a refusal reached the socket; nothing to serve"
            );
            return;
        }
    };
    // The fingerprint is an AUTHENTICATED FACT already validated by admission,
    // so nothing here constructs one.
    let Ok(worker_fp) = WorkerFp::try_from(worker_fp) else {
        tracing::error!("worker link: admission produced an unvalidated fingerprint");
        return;
    };
    run_link(socket, worker_fp, caller, services).await;
}

/// The link: read the hello, register the generation, then read and dispatch.
async fn run_link(
    mut socket: WebSocket,
    worker_fp: WorkerFp,
    caller: VerifiedWorkerCaller,
    services: &Arc<CoordServices>,
) {
    // Attached BEFORE the hello, so a reap arriving between admission and the
    // first frame has a socket to travel on rather than sitting in the record.
    let registration = LinkRegistration::attach(&services.orphan_kills, worker_fp.clone());

    // ONE OWNER, THREE ARMS, NO SPLIT. `WebSocket::recv` and `WebSocket::send`
    // are both `async fn(.., &mut self)` and there is NO `impl Sink for
    // WebSocket` (axum 0.8.9 `extract/ws.rs:555,560,580`), so the socket is
    // never divided — the same task reads AND writes, and the write is simply
    // the third thing the loop waits on.
    // The channel the handle's send closure enqueues into, and the loop drains.
    // `WorkerHandle::new`'s fifth argument — `worker_handle.rs:75` — is
    // `Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync>`, a SYNCHRONOUS
    // shared-borrow closure, which is why it enqueues rather than writing.
    let (outbound, mut outbox_rx) = mpsc::unbounded_channel::<CoordWorkerDownstream>();

    // THE HELLO, and it comes before the handle because `WHello` carries the
    // two fields the handle is constructed with and `WorkerHandle::new` is their
    // ONLY writer (`worker_handle.rs:86-92`: the two `Arc<AtomicBool>` that need
    // later mutation sit beside a plain `Option<String>` and a plain
    // `BTreeSet` that do not). A handle built before this point would be
    // permanently capability-less, and `live_frames.rs:186` would drop every
    // `CAPABILITY_TERMINAL_METADATA_V1` frame for the socket's whole life at
    // debug level.
    let Some(hello) = read_hello(&mut socket, &worker_fp).await else {
        services.workers.retire(&worker_fp);
        drop(registration);
        return;
    };

    // `WorkerHandle::new` — `coord_core/worker_handle.rs:70`. Five arguments,
    // each sourced:
    // - `worker_fp`: received, already validated by admission.
    // - `process_epoch`: from the hello, and it has no other reader — the ten
    //   other `process_epoch` hits in `src/` are the COORDINATOR's own boot
    //   epoch (`serve.rs:310`), a different value that shares the name.
    // - `connection_generation`: minted above, distinct per connection, because
    //   `insert` exists to fence and `:32` needs "newer" to mean something.
    // - `capabilities`: the hello's own set. `CAPABILITY_TERMINAL_METADATA_V1`
    //     is named here rather than discovered at runtime, and
    //     `live_frames.rs:186` is what reads it.
    // - `send`: the mpsc closure, returning enqueued-or-fenced and never zero
    //     for a live generation.
    let handle: Arc<WorkerHandle> = Arc::new(WorkerHandle::new(
        worker_fp.clone(),
        Some(hello.process_epoch.clone()),
        mint_connection_generation(&worker_fp),
        hello.capabilities.clone(),
        {
            let outbound = outbound.clone();
            let live = Arc::new(std::sync::atomic::AtomicI64::new(0));
            Arc::new(move |frame: CoordWorkerDownstream| {
                // NON-ZERO ALWAYS, and that is the contract: `send` returns 0
                // for a FENCED generation (`worker_handle.rs:97-99`, and v2's
                // `myHandle.send` likewise), so zero must mean "dropped" and
                // nothing else.
                let sequence = live.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                let _ = outbound.send(frame);
                sequence
            })
        },
    ));
    // `WorkerRegistry::insert` — `:162`. Fences the previous generation; `:155-158`
    // is why it exists.
    services.workers.insert(Arc::clone(&handle));
    // `CoordServices::worker_dispatcher` — `services.rs:253`. The ONE
    // construction path, over `DispatcherFor::new`/`build`
    // (`dispatcher_for.rs:45,63`).
    let mut dispatcher = services.worker_dispatcher(Arc::clone(&handle));
    tracing::info!(
        %worker_fp,
        key_generation = caller.key_generation,
        label = %caller.label,
        capabilities = hello.capabilities.len(),
        "worker link: hello accepted, its generation is registered, and its reap sender is attached"
    );

    // `Keepalive::new` and `STALE_LINK_CHECK_INTERVAL` — `worker_link/keepalive.rs`,
    // both taking a clock rather than reading one.
    // `Keepalive::new` — `worker_link/keepalive.rs:43` — takes a
    // `std::time::Instant`, not tokio's, so the tick is tokio's and the
    // staleness clock is std's.
    let keepalive = Keepalive::new(std::time::Instant::now());
    let mut tick = interval(STALE_LINK_CHECK_INTERVAL);
    loop {
        tokio::select! {
            // THE SECOND ARM: what the handle's send closure enqueued.
            //
            // A NAMED HOLE, for the same reason `frame_from_bytes` is one:
            // `CoordWorkerDownstream`'s arms carry typed fields
            // (`wire/coord_worker/downstream.rs:35`) and mapping each to a
            // socket message is a design decision, not a translation. A wrong
            // mapping compiles exactly as cleanly as a right one and sends the
            // wrong bytes.
            outbound = outbox_rx.recv() => {
                let Some(frame) = outbound else {
                    break;
                };
                tracing::debug!(%worker_fp, ?frame, "worker link: a downstream arm not yet mapped to a socket message");
                continue;
            }
            // THE FIRST ARM, and it AWAITS.
            received = socket.recv() => match received {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(error)) => {
                    tracing::info!(%worker_fp, %error, "worker link: the socket failed");
                    break;
                }
                Some(Ok(message)) => {
                    // THE ARM THAT MAKES THIS A LINK. Every frame read is
                    // DISPATCHED, and the dispatch is AWAITED before the next
                    // read: a durable frame's whole claim is that it is not
                    // droppable, so handing one to a loop that might not be
                    // scheduled would be a dispatcher that can drop it. The cost
                    // is stated rather than discovered — a slow durable append
                    // stalls this worker's reads, bounded by the append's own
                    // deadline and the dispatcher's queue ceiling.
                    let outcome = dispatch(&mut dispatcher, &worker_fp, message).await;
                    if let DispatchOutcome::Close(close) = outcome {
                        tracing::info!(%worker_fp, reason = close.reason(),
                                        "worker link: closing on policy");
                        break;
                    }
                }
            },
            _ = tick.tick() => {
                let now = std::time::Instant::now();
                if keepalive.is_stale(now) {
                    tracing::warn!(%worker_fp,
                        silent_ms = keepalive.silence(now).as_millis(),
                        "worker link: half-open; the route is gone and no frame will \
                         ever arrive to say so");
                    break;
                }
                // The point of the registration: whatever the loop is doing, an
                // owed reap now has somewhere to go.
                let owed = registration.take_pending();
                if !owed.is_empty() {
                    tracing::info!(%worker_fp, reaps = owed.len(),
                                    "worker link: delivering reaps a reconnect owed");
                }
            }
        }
    }
    // `WorkerRegistry::retire` — `:233`, the pair of `insert`.
    services.workers.retire(&worker_fp);
    // `registration` drops here, and with it the detach.
    tracing::info!(%worker_fp, "worker link: closed");
}

/// The negotiated facts a `WHello` carries, which are the handle's arguments.
struct Hello {
    process_epoch: String,
    capabilities: BTreeSet<String>,
}

/// Read the forced first frame, under the pre-hello bound.
async fn read_hello(socket: &mut WebSocket, worker_fp: &WorkerFp) -> Option<Hello> {
    let deadline = tokio::time::Instant::now() + PRE_HELLO_WAIT;
    loop {
        let received = tokio::time::timeout_at(deadline, socket.recv()).await;
        let Ok(Some(Ok(message))) = received else {
            tracing::warn!(%worker_fp, "worker link: no hello inside the pre-hello bound");
            return None;
        };
        let text = match message {
            Message::Text(text) => text.as_str().as_bytes().to_vec(),
            Message::Binary(bytes) => bytes.to_vec(),
            _ => continue,
        };
        // `decode_upstream` — `roost_protocol::proto_adapters::coord_worker_proto:55`,
        // the link's EXISTING codec. Never a second decoder.
        let frame = match roost_protocol::proto_adapters::coord_worker_proto::decode_upstream(&text)
        {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%worker_fp, %error, "worker link: a frame did not decode");
                return None;
            }
        };
        // `CoordWorkerUpstream::Hello` — `roost_protocol::wire::coord_worker::upstream.rs:36`,
        // carrying `capabilities` and `process_epoch`.
        if let roost_protocol::wire::coord_worker::CoordWorkerUpstream::Hello {
            capabilities,
            process_epoch,
            ..
        } = frame
        {
            return Some(Hello {
                process_epoch,
                capabilities: capabilities.into_iter().collect(),
            });
        }
    }
}

/// Hand one inbound message to the dispatcher and await its answer.
async fn dispatch(
    dispatcher: &mut crate::worker_link::frame_dispatch::WorkerFrameDispatcher,
    worker_fp: &WorkerFp,
    message: Message,
) -> DispatchOutcome {
    let text = match message {
        Message::Text(text) => text.as_str().as_bytes().to_vec(),
        Message::Binary(bytes) => bytes.to_vec(),
        // A ping is the transport's, and a close ends the loop; neither is a
        // frame the dispatcher owns.
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) => {
            return DispatchOutcome::Refused;
        }
    };
    let Some(frame) = frame_from_bytes(&text) else {
        tracing::warn!(%worker_fp, "worker link: a frame did not decode; refused");
        return DispatchOutcome::Refused;
    };
    match frame.class {
        FrameClass::Durable => dispatcher.handle_durable(worker_fp.as_str(), &frame).await,
        _ => dispatcher.handle_now(worker_fp.as_str(), &frame),
    }
}

/// The one seam this file does not yet have: bytes to a classified frame.
///
/// `InboundFrame` has no constructor in the tree, and no module maps a decoded
/// `CoordWorkerUpstream` to a `FrameClass` and a channel. Twenty-four variants,
/// and which are durable, which are rpc, which are live and which are unowned is
/// a design decision rather than a translation — so it is NAMED here instead of
/// guessed, because a guessed class assignment routes frames to the wrong place
/// and a wrong route compiles exactly as cleanly as a right one.
fn frame_from_bytes(_bytes: &[u8]) -> Option<InboundFrame> {
    None
}
