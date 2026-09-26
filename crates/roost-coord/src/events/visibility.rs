//! The one predicate separating a durable-but-private event from a public one.
//!
//! Owned by the coordinator for now, and *intended* to move to `roost-protocol`
//! beside `fold_event`. See the note at the bottom of this file: the five
//! consumers of this predicate span three crates, and a coordinator-local copy
//! is one implementation too many — but moving it is not this file's to do, and
//! duplicating it silently would be worse than leaving it here with its
//! consumers named.
//!
//! WHY ONE PREDICATE AND NOT FIVE CHECKS. The source header
//! (`apps/coord/src/events/session-event-visibility.ts:1-6`): "Durable queries,
//! live publication, and frame construction all depend on this predicate so
//! browser lanes cannot diverge." Five call sites, one decision:
//!
//! - the three durable reads filter it (`apps/coord/src/events/event-query.ts:19,37,55`);
//! - the publisher returns early on a private event
//!   (`apps/coord/src/events/pending-event-publications.ts:268`);
//! - the session row mapper omits the three agent columns entirely
//!   (`apps/coord/src/events/event-projection.ts:22-24`).
//!
//! WHAT "PRIVATE" MEANS HERE. `agent_reference` is durable and recoverable by
//! the owning worker, and invisible to every browser through both the log reads
//! and the in-process bus. It is not a redaction: it is an opaque recovery
//! reference that would otherwise leak a filesystem path into a dashboard.

/// The single durable event kind that never reaches a browser.
///
/// Ported from `apps/coord/src/events/session-event-visibility.ts:7`.
pub const PRIVATE_SESSION_EVENT_KIND: &str = "agent_reference";

/// Whether an event of this kind is safe to publish to browsers.
///
/// Takes the `kind` discriminator rather than a decoded event on purpose: the
/// five consumers hold four different things -- an `rswoosh_protocol` event, a
/// row's `kind` column, a decoded protobuf oneof -- and keying all of them on
/// one string is what stops the lanes diverging. The event union's own
/// exhaustiveness is `roost-protocol`'s job, not this file's.
#[must_use]
pub fn kind_is_public(event_kind: &str) -> bool {
    event_kind != PRIVATE_SESSION_EVENT_KIND
}

// ── intended home, and why it is not there yet ──────────────────────────────
//
// `fold_event` lives in `roost-protocol` (`crates/roost-protocol/src/wire/event.rs`)
// because every component of every front end folds the same events, and a second
// fold is the defect that module exists to prevent. This predicate is the same
// shape of problem one level up: five consumers, three crates, one decision.
//
// When it moves, the consumers to update together are:
//   * `roost-coord`'s three durable readers and its publisher;
//   * `roost-client-core`'s Sync frame constructor, which must also refuse to
//     build a `FirehoseFrame` from a private kind;
//   * `roost-protocol`'s own `fold_event`, whose public projection currently
//     treats `agent_reference` as an explicit no-op
//     (`packages/protocol/src/wire/event.ts:128-137`) and should read this
//     predicate instead of restating the name.
