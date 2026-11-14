//! What delivers INTO a session: the keeper channel's output binding, and the
//! durable boundary every session event crosses on its way to the coordinator.
//! `session::emit` builds frames and hands them to a binding; `event_store` and
//! the coordinator link are the production implementation of the event sink.
//! Depends on `crate::event_store` for the capacity claim and `roost_protocol`
//! for the event union — and on nothing that depends on it back.
//!
//! TWO SINK KINDS, AND THE DIFFERENCE IS THE POINT. The CHANNEL BINDING is
//! upstream of everything: bytes arrive from a separate keeper process and are
//! handed to whatever session is bound to that channel id, with no claim taken
//! and no possibility of refusal. The EVENT SINK is the durability boundary:
//! it can say no, and when it does the session's end is unwritable, which is a
//! condition no per-request answer can fix. Collapsing them would make "the
//! keeper is producing" and "the coordinator can learn about it" one decision.
//! Ports v2 `apps/worker/src/transport/event-sink.ts`.

use std::pin::Pin;

use roost_protocol::wire::event::SessionEvent;

use crate::event_store::{AppendError, DurableEventKind, Reservation, ReserveError};

/// The session a keeper channel's output is delivered into.
///
/// This is the v2 `MuxChannelCallbacks`, and the name is the port's: v2 called
/// it "callbacks" because a channel was a JavaScript object's property bag,
/// and here the thing being bound is the RELATIONSHIP between a keeper PTY and
/// the record it feeds. Note that `roost_keeper::frames::ChannelBinding` is a
/// different type and not this one — that is the wire's `{channel_id, pid}`
/// pair, and this is the worker-side delivery binding.
///
/// `&self` throughout because the keeper's reader thread delivers into a
/// binding it does not own: the record behind the binding is behind a lock the
/// reader must not hold across a PTY write.
pub trait ChannelBinding: Send + Sync {
    /// One chunk of PTY output, exactly as the keeper produced it.
    ///
    /// Never empty in practice, and never coalesced: the byte stream is
    /// contiguous, so a chunk that reordered or merged with its neighbour would
    /// splice an invisible hole into the parser state the core is holding.
    fn on_output(&self, chunk: &[u8]);

    /// The child ended. `None` when it was killed by a signal, which is not
    /// the same as a nonzero exit and a client may care which it was.
    fn on_exit(&self, exit_code: Option<i32>);

    /// The channel could not be driven: a refused write, a decode failure, a
    /// keeper that stopped answering. The session decides what it means; this
    /// binding only says it happened.
    fn on_error(&self, reason: String);
}

/// Why the durable boundary refused.
///
/// A distinct type rather than the store's own errors because the CALLER's
/// obligation differs by cause: a full store is a capacity problem the
/// admission control already accounts for, while a mismatch between a reserved
/// kind and the event actually written is a defect in this crate, and the
/// worker stops rather than logging it and carrying on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionEventError {
    /// The store had no capacity for this kind of event.
    #[error("the durable session-event store refused the claim: {0}")]
    Reserve(#[from] ReserveError),

    /// The event did not match the claim taken for it.
    #[error("the durable session-event store refused the write: {0}")]
    Append(#[from] AppendError),

    /// The durable store could not be reached, or refused in its own right.
    ///
    /// Its own variant because a store that did not answer is neither a
    /// CAPACITY refusal nor a claim mismatch, and collapsing it into either
    /// would tell a caller to do something about capacity when the disk is what
    /// said no.
    #[error("the durable session-event store did not answer: {0}")]
    Store(String),

    /// An event with no worker-authored policy reached this boundary.
    ///
    /// Not a refusal the caller can act on: it means a caller emitted an event
    /// this crate has no rule for, which is a defect rather than a condition.
    #[error("no worker-authored policy covers this session event: {0}")]
    Unclassifiable(String),
}

/// The boundary every session event crosses before the coordinator sees it.
///
/// The four methods are v2's `SessionEventSink`, in the same order, because the
/// ordering IS the contract: a session that is open has not written its
/// `closed` event yet and must be able to, so [`SessionEventSink::reserve`] is
/// taken BEFORE the PTY exists and consumed by the close. [`hold`](
/// SessionEventSink::hold) then marks that claim as committed — it no longer
/// blocks a snapshot, but it is still the same claim and nobody else may take
/// its capacity.
///
/// ASYNC, AND EXPLICITLY SO. The store behind this is `event_store::Journal`, a
/// SQLite database, and every one of its claim operations is `async`. A sync
/// trait over an async store has three exits and all three are wrong: block a
/// runtime thread on a database round trip; spawn and return, so a caller that
/// believes its `closed` event is recorded is mistaken; or queue and report
/// success, which is a claim that APPEARS to be taken while the write is still
/// in flight. That last one is the invisible failure, and it is worse than the
/// trait being inconvenient.
///
/// THE METHODS RETURN [`EventFuture`] RATHER THAN BEING `async fn`, for two
/// reasons that are both load-bearing. `async fn` in a trait is not dyn
/// compatible, and this is held as `Arc<dyn SessionEventSink>`. And the future's
/// `Send` bound has to be NAMEABLE, because the manager holds a sink across an
/// await and a future that is not `Send` would make `SessionManager` un-`Send`
/// — a property of this crate's async surface rather than something a caller
/// should discover.
///
/// THE LIFETIME IS THE POINT OF `'a`. Without it the future would be `'static`,
/// so `emit` would have to CLONE the event into it — and this trait is
/// crossed on every session state change, which is exactly the frequency at
/// which a copy nobody needs is most expensive. Note what this lifetime is NOT:
/// it is a return-position lifetime, which keeps the trait dyn compatible. The
/// undispatchable-receiver problem that killed `self: Arc<Self>` on
/// `SessionLifecycle` does not arise here.
pub trait SessionEventSink: Send + Sync {
    /// Claim capacity for one durable event, before the event is built.
    ///
    /// Fails when the store cannot hold the kind. A spawn that cannot reserve
    /// its `closed` must not open a PTY at all.
    #[must_use = "a claim that is not awaited is a claim that was not taken"]
    fn reserve(
        &self,
        kind: DurableEventKind,
    ) -> EventFuture<'_, Result<Reservation, SessionEventError>>;

    /// Mark a claim committed: it stops blocking snapshots and keeps its
    /// capacity. Idempotent only in the sense that a second hold is refused
    /// rather than silently absorbed.
    #[must_use = "a hold that is not awaited has not marked the claim committed"]
    fn hold(&self, reservation: Reservation) -> EventFuture<'_, ()>;

    /// Give a claim back, because the event it was taken for will not happen.
    ///
    /// A spawn that fails after reserving `opened` and `closed` releases both;
    /// leaking a claim is how a store eventually refuses every write.
    #[must_use = "a release that is not awaited has not given the claim back"]
    fn release(&self, reservation: Reservation) -> EventFuture<'_, ()>;

    /// Publish one event, consuming a claim when one was taken for it.
    ///
    /// The event without a claim is a metadata event: replaceable, coalesced
    /// in memory, and lost on restart without a session being wrong. The claim
    /// is what makes the event durable, so its presence is the difference
    /// between the two and the caller must state which it meant.
    #[must_use = "an emit that is not awaited is an event that was never written"]
    fn emit<'a>(
        &'a self,
        event: &'a SessionEvent,
        reservation: Option<Reservation>,
    ) -> EventFuture<'a, Result<(), SessionEventError>>;
}

/// What every [`SessionEventSink`] method returns.
///
/// Named here rather than borrowed from `browser_commands` because this is not a
/// browser command's future: it is a database round trip, and a reader asking
/// what a claim costs should not have to walk through the command router to
/// find out. `Send` because `SessionManager` holds a sink across an await, and
/// `'a` because the event an `emit` borrows is not copied to make the future
/// ownable.
///
/// IT IS NOT AN INSTRUMENT, AND THAT IS WHY THE ATTRIBUTE IS ON THE METHODS.
/// A `#[must_use]` on a TYPE ALIAS is ignored by rustc — the attribute would sit
/// there looking like a guard and do nothing, which is the seventh instrument of
/// the night. The four methods below carry it instead, and that is the form
/// which fires.
///
/// `#[must_use]` HERE IS NOT COSMETIC. A caller that builds one of these and
/// does not await it gets a future that is constructed and immediately dropped,
/// so whatever the future was going to do silently does not happen — and
/// NOTHING SAYS SO. The type system cannot see it, clippy did not, `--all-targets`
/// compiled it, and the code reads correctly. It was found by a before/after
/// test baseline and not by review, which is the worst possible way to find
/// one.
///
/// The fourth instance of "type-checks and cannot do what you meant" in one
/// cascade, after `Arc::get_mut` on a type holding a `Weak` to itself, an
/// explicit `drop()` leaving a drop flag so a `MutexGuard` stayed live across an
/// await, and a `MutexGuard` across an await making the whole future `!Send`.
/// The first three cost a compile or a test run to find; this one is now
/// impossible to write.
pub type EventFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
