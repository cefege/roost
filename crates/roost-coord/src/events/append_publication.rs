//! The half of the append path that runs **after** the transaction resolved:
//! publish, retain, claim, and the route-before-bus order.
//!
//! This is a separate file from `append` because the boundary between them *is*
//! the invariant. `docs/phase3-coord-contract.md` §3.2 records how v2 enforces it
//! structurally rather than by convention: the transaction body contains no
//! publish call at all, and the only publisher is module-private, with its two
//! call sites after the awaited transaction has resolved
//! (`apps/coord/src/events/pending-event-publications.ts:267,231,258`). Here
//! `publish_committed_event` is private to this module and `resolve_publication`
//! is the only thing that calls it, so the ordering is verifiable by reading one
//! signature.
//!
//! WHAT THE CLAIM IS FOR. The durable row commits, and *then* the
//! socket-generation and revocation fence is consulted. If the fence says this
//! connection is stale, or the publish itself fails, the event is durable and
//! nobody was told. The retained effect is what the worker's retry -- which is
//! guaranteed, because it never got an ACK -- claims and publishes later.
//!
//! ROUTE BEFORE BUS, IN ONE FUNCTION. `publish_committed_event` applies the
//! durable channel index, *then* publishes to the session bus, *then* the
//! workspace cascade. A cell frame that routes before the event that named its
//! channel is a frame nobody can place; the symptom is a terminal that never
//! paints rather than an ordering error.
//!
//! A PRIVATE EVENT PUBLISHES NOTHING AND INDEXES NOTHING, and the early return is
//! the first statement of the publisher rather than a check repeated at each call
//! site.

use roost_protocol::wire::{SessionEvent, WorkerFp, WorkspaceDelta, WorkspaceId};

use crate::events::append::{AppendError, Caller, LiveEffects, Reservation};
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::SessionBusMessage;
use crate::events::pending_publications::{
    ClaimOutcome, PendingPublicationStore, RetainedPublication,
};
use crate::events::visibility::kind_is_public;

/// A committed event and everything its publication needs.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedEventPublication {
    /// The committed event, post-normalization and post-tombstone-filter.
    pub event: SessionEvent,
    /// The **authenticated** caller's fingerprint, never one the event claimed.
    /// `None` for a coordinator-side producer, and `None` means "bind nothing":
    /// guessing the worker from the route cache could bind on a worker that has
    /// already been replaced.
    pub authenticated_worker_fp: Option<WorkerFp>,
    /// The durable `events.id`, stamped onto the bus message as the replay order.
    pub event_id: u64,
    /// The exact JSON the transaction persisted, kept for the byte-for-byte
    /// comparison a dedupe replay performs.
    pub event_json: String,
    /// Workspaces this close orphaned. Published after commit, and retained so a
    /// later claim republishes them: without that, a browser that missed the
    /// `workspaceBus` deletion keeps a phantom workspace in its sidebar until the
    /// next seeded snapshot.
    pub cascade_orphan_ids: Vec<WorkspaceId>,
    /// Sessions a snapshot found force-closed. Reaped after commit, never before.
    pub snapshot_reap_ids: Vec<String>,
}

impl CommittedEventPublication {
    /// The form the bounded store holds while a publication is lost.
    fn retained(&self, dashboard_id: &str) -> Result<RetainedPublication, AppendError> {
        Ok(RetainedPublication {
            event_kind: self.event.kind_name().to_owned(),
            event_id: i64::try_from(self.event_id).map_err(|_| {
                AppendError::EventIdOutOfRange {
                    id: i64::try_from(self.event_id).unwrap_or(i64::MAX),
                }
            })?,
            event_json: self.event_json.clone(),
            dashboard_id: dashboard_id.to_owned(),
            cascade_orphan_ids: self.cascade_orphan_ids.clone(),
            snapshot_reap_ids: self.snapshot_reap_ids.clone(),
        })
    }

    /// Rebuild the publishable effect from a claimed one.
    ///
    /// The event is decoded from the **stored JSON** rather than taken from the
    /// replaying caller's own value, so what gets published is byte-for-byte
    /// what the first delivery committed -- the same bytes `claim` just compared
    /// against. `authenticated_worker_fp` is the claiming worker, and it is the
    /// same value the first delivery used: a slot is keyed by that fingerprint,
    /// and revocation drops every slot a fingerprint holds.
    fn from_claimed(
        claimed: &RetainedPublication,
        worker_fp: &WorkerFp,
    ) -> Result<Self, AppendError> {
        let event: SessionEvent =
            serde_json::from_str(&claimed.event_json).map_err(|error| {
                AppendError::UndecodableRetained {
                    event_id: claimed.event_id,
                    reason: error.to_string(),
                }
            })?;
        let event_id =
            u64::try_from(claimed.event_id).map_err(|_| AppendError::EventIdOutOfRange {
                id: claimed.event_id,
            })?;
        Ok(Self {
            event,
            authenticated_worker_fp: Some(worker_fp.clone()),
            event_id,
            event_json: claimed.event_json.clone(),
            cascade_orphan_ids: claimed.cascade_orphan_ids.clone(),
            snapshot_reap_ids: claimed.snapshot_reap_ids.clone(),
        })
    }
}

/// What the publication step decided.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicationResolution {
    /// The effect that was published, when one was.
    pub published_effect: Option<CommittedEventPublication>,
    /// A dedupe replay carried a different payload for the same `client_seq`.
    ///
    /// This is a **protocol violation** and not a merge: the worker is not
    /// replaying what it sent, so the socket is closed rather than two payloads
    /// being combined into an event neither delivery described
    /// (`docs/phase3-coord-contract.md` §3.6).
    pub replay_rejected: bool,
}

/// Publish, retain, or claim: the whole post-commit decision.
///
/// The order of the questions is v2's (`pending-event-publications.ts:221-265`).
/// A fresh commit is published or retained. A dedupe with nothing retained is
/// released and forgotten, which is the ordinary path after a crash. A dedupe
/// with something retained claims it, re-checks the fence -- because the claim
/// took time and a generation can lapse across it -- publishes, and completes.
pub(crate) fn resolve_publication<'a>(
    committed_effect: Option<CommittedEventPublication>,
    deduplicated: bool,
    replay_event_json: &str,
    caller: &Caller,
    store: Option<&mut PendingPublicationStore>,
    live: LivePublication<'a>,
) -> Result<PublicationResolution, AppendError> {
    let LivePublication {
        buses,
        live_effects,
        can_publish,
    } = live;
    let reservation = caller.reservation();
    // The store is only reachable through a reservation: a caller with no
    // `(worker_fp, client_seq)` has no slot to reserve, retain into, or claim.
    let mut store = store.filter(|_| reservation.is_some());

    if let Some(effect) = committed_effect {
        if !fence_allows(can_publish) {
            retain(&mut store, reservation, &effect, caller)?;
            return Ok(unpublished());
        }
        if let Err(error) = publish_committed_event(&effect, buses, live_effects) {
            retain(&mut store, reservation, &effect, caller)?;
            return Err(error);
        }
        release(&mut store, reservation);
        return Ok(PublicationResolution {
            published_effect: Some(effect),
            replay_rejected: false,
        });
    }

    let Some(reservation) = reservation else {
        return Ok(unpublished());
    };
    let Some(store) = store else {
        return Ok(unpublished());
    };
    if !deduplicated {
        store.release(reservation.worker_fp.as_str(), reservation.client_seq);
        return Ok(unpublished());
    }
    if !fence_allows(can_publish) {
        store.release(reservation.worker_fp.as_str(), reservation.client_seq);
        return Ok(unpublished());
    }

    match store.claim(
        reservation.worker_fp.as_str(),
        reservation.client_seq,
        replay_event_json,
    ) {
        ClaimOutcome::PayloadMismatch => Ok(PublicationResolution {
            published_effect: None,
            replay_rejected: true,
        }),
        ClaimOutcome::Nothing => {
            store.release(reservation.worker_fp.as_str(), reservation.client_seq);
            Ok(unpublished())
        }
        ClaimOutcome::Claimed(claimed) => {
            let effect = CommittedEventPublication::from_claimed(&claimed, &reservation.worker_fp)?;
            if !fence_allows(can_publish) {
                // Put it back: the effect is still unpublished, and the next
                // retry must be able to claim it again.
                store.retain(
                    reservation.worker_fp.as_str(),
                    reservation.client_seq,
                    claimed,
                );
                return Ok(unpublished());
            }
            if let Err(error) = publish_committed_event(&effect, buses, live_effects) {
                store.retain(
                    reservation.worker_fp.as_str(),
                    reservation.client_seq,
                    claimed,
                );
                return Err(error);
            }
            store.release(reservation.worker_fp.as_str(), reservation.client_seq);
            Ok(PublicationResolution {
                published_effect: Some(effect),
                replay_rejected: false,
            })
        }
    }
}

/// The three things a publication needs from the process, named once.
///
/// They travel together because they are the three answers to "where does a
/// committed event go", and a caller that supplies two of them and improvises the
/// third is the bug this grouping exists to make impossible.
#[derive(Clone, Copy)]
pub(crate) struct LivePublication<'a> {
    /// The buses the effect is published to.
    pub(crate) buses: &'a Buses,
    /// The durable channel index and the orphan reap.
    pub(crate) live_effects: &'a dyn LiveEffects,
    /// The generation and revocation fence.
    pub(crate) can_publish: Option<&'a dyn Fn() -> bool>,
}

/// The one publisher. Private, so the call sites above are the only two.
fn publish_committed_event(
    effect: &CommittedEventPublication,
    buses: &Buses,
    live_effects: &dyn LiveEffects,
) -> Result<(), AppendError> {
    if !kind_is_public(effect.event.kind_name()) {
        return Ok(());
    }
    live_effects.index_durable_channel(&effect.event, effect.authenticated_worker_fp.as_ref());
    buses
        .session_bus
        .publish(SessionBusMessage::committed(effect.event.clone(), effect.event_id));
    for id in &effect.cascade_orphan_ids {
        buses
            .workspace_bus
            .publish(WorkspaceDelta::Deleted { id: id.clone() });
    }
    Ok(())
}

fn unpublished() -> PublicationResolution {
    PublicationResolution {
        published_effect: None,
        replay_rejected: false,
    }
}

/// The generation and revocation fence. Absent means "no fence", which is the
/// coordinator's own direct callers and every test.
fn fence_allows(can_publish: Option<&dyn Fn() -> bool>) -> bool {
    can_publish.is_none_or(|fence| fence())
}

fn retain(
    store: &mut Option<&mut PendingPublicationStore>,
    reservation: Option<Reservation>,
    effect: &CommittedEventPublication,
    caller: &Caller,
) -> Result<(), AppendError> {
    let (Some(store), Some(reservation)) = (store.as_deref_mut(), reservation) else {
        return Ok(());
    };
    store.retain(
        reservation.worker_fp.as_str(),
        reservation.client_seq,
        effect.retained(&caller.dashboard_id)?,
    );
    Ok(())
}

fn release(
    store: &mut Option<&mut PendingPublicationStore>,
    reservation: Option<Reservation>,
) {
    let (Some(store), Some(reservation)) = (store.as_deref_mut(), reservation) else {
        return;
    };
    store.release(reservation.worker_fp.as_str(), reservation.client_seq);
}
