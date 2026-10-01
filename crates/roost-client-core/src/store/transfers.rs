//! Transfers: the upload and download cards, their progress, and their
//! deadlines. One stack for both directions, because the card stack is one
//! stack.
//!
//! The rule this module exists to make unbreakable: **PROGRESS MAY GO
//! BACKWARDS.** A chunked upload that reports 99% and restarts from zero is
//! succeeding, and a store that reads a decrease as failure shows a transfer
//! that fails while it works. So [`set_transfer_progress`] treats a decrease as
//! a restart — the card stays `running`, the rate sample is re-seeded, and the
//! ETA reports unknown until the restarted run has samples of its own. It is a
//! transition inside `running`, not an edge into a failure.
//!
//! Two more refusals, both from the same principle — a late callback must not
//! rewrite a card that has moved on:
//!
//! - a progress tick for a card that is not in the map is ignored, so a callback
//!   that lands after the user dismissed the card cannot resurrect it;
//! - a progress tick for a card in a terminal state is ignored, so the last tick
//!   of a finished upload cannot walk its byte count back down.
//!
//! Speed is an exponential moving average over per-tick samples, seeded on the
//! first tick so it measures actual byte flow rather than the time since the
//! card appeared. The samples are bookkeeping beside the cards, not cards: they
//! change without a `revision`, exactly as v2 kept them in a non-reactive map
//! (`apps/web/src/store/transfers.ts:3-5`).
//!
//! Ported from `apps/web/src/store/transfers.ts`; the deviations are the
//! `running` restart, a `stalled` state for a card that stopped advancing where
//! v2 had none, an `Ambiguous` state for a write whose acknowledgement was
//! lost — a card v2 showed as a plain error, and which this crate must not let
//! a reader mistake for one — `Option` in place of v2's `-1`/`0` sentinels, and
//! deadlines as data because this crate has no timer.

use crate::store::Store;

pub mod record;

use self::record::RateSample;

/// How long a successful or deduplicated card stays before it removes itself.
///
/// v2's own number, at `transfers.ts:90-98`, and it applies to the same two
/// states: an ERROR card does not take this window, because the failure text is
/// what the user came to read. See [`record::TransferState::self_dismisses`].
pub const TRANSFER_DISMISS_AFTER_MS: u64 = 2_000;

/// How long a `running` card may go without advancing before the sweep calls it
/// stalled. Long enough that a slow but live upload is never called stalled.
///
/// v2 has no such state, so there is no v2 number: the sweep needs SOME bound,
/// and one that fires on a healthy transfer would report a working upload as
/// broken.
pub const TRANSFER_STALL_AFTER_MS: u64 = 15_000;

/// Ticks closer together than this produce no rate.
///
/// v2's `MIN_DELTA_S = 0.05` (`transfers.ts:42`) in milliseconds: the
/// instantaneous figure off a 5 ms sample is noise, and an EMA fed noise drifts
/// high.
const MIN_RATE_DELTA_MS: u64 = 50;

/// The weight on the newest instantaneous rate. v2's `EMA_ALPHA`
/// (`transfers.ts:41`).
const EMA_ALPHA: f64 = 0.4;

pub use record::{NewTransfer, Transfer, TransferDirection, TransferStack, TransferState};

/// What one progress tick decided, before it is written to a card.
struct ProgressUpdate {
    bytes_done: u64,
    bytes_total: u64,
    speed_bps: Option<f64>,
    eta_s: Option<u64>,
    advanced: bool,
    restarted: bool,
}

/// Put a card on the stack, or replace the one this id already has.
///
/// A replacement resets the card completely: a second upload of the same
/// attachment id starts from zero, and a card that had already settled gets its
/// own dismissal window. Returns the card it displaced, so a host can release
/// the preview it minted for that one.
pub fn add_transfer(store: &mut Store, new: NewTransfer) -> Option<Transfer> {
    let id = new.id;
    let previous = store.transfers.transfers.get(&id).cloned();
    let transfer = Transfer {
        id: id.clone(),
        name: new.name,
        direction: new.direction,
        bytes_done: 0,
        bytes_total: new.bytes_total,
        speed_bps: None,
        eta_s: None,
        state: new.state,
        err: None,
        preview_url: new.preview_url,
        dismiss_at_ms: if new.state.self_dismisses() {
            Some(new.now_ms.saturating_add(TRANSFER_DISMISS_AFTER_MS))
        } else {
            None
        },
        last_progress_ms: if new.state.is_terminal() {
            None
        } else {
            Some(new.now_ms)
        },
    };
    if previous.is_some() {
        // The displaced card's samples describe a run that is over.
        store.transfers.samples.remove(&id);
    }
    store.transfers.transfers.insert(id, transfer);
    store.note_change();
    previous
}

/// Live progress for one card.
///
/// A DECREASE IS A RESTART, not a failure and not an error: the card stays
/// running, its rate sample is re-seeded, and its ETA reports unknown until the
/// restarted run has samples of its own. A restart in the middle of a stalled
/// card revives it, because a card whose byte count moved backwards is
/// demonstrably still being written.
///
/// Returns whether the card changed.
pub fn set_transfer_progress(
    store: &mut Store,
    id: &str,
    bytes_done: u64,
    bytes_total: Option<u64>,
    now_ms: u64,
) -> bool {
    let Some(update) = measure(store, id, bytes_done, bytes_total, now_ms) else {
        return false;
    };
    let Some(transfer) = store.transfers.transfers.get_mut(id) else {
        return false;
    };
    transfer.bytes_done = update.bytes_done;
    transfer.bytes_total = update.bytes_total;
    transfer.speed_bps = update.speed_bps;
    transfer.eta_s = update.eta_s;
    if update.restarted || (update.advanced && transfer.state == TransferState::Stalled) {
        transfer.state = TransferState::Running;
    }
    transfer.last_progress_ms = Some(now_ms);
    store.note_change();
    true
}

/// Work out what a tick means, without writing the card.
///
/// The measurement half of [`set_transfer_progress`], which owns the card write
/// and the single `note_change` that goes with it. This takes `&mut Store` to
/// reach the sample map and is not a mutation of its own.
///
/// Split from the write so the state rule that follows a measurement cannot be
/// half-applied, and so the two refusals — no such card, and a settled card —
/// are decided before any borrow of the card is live.
fn measure(
    store: &mut Store,
    id: &str,
    bytes_done: u64,
    bytes_total: Option<u64>,
    now_ms: u64,
) -> Option<ProgressUpdate> {
    let transfer = store.transfers.transfers.get(id)?;
    if transfer.state.is_terminal() {
        // The last tick of a finished upload must not walk its bytes back.
        return None;
    }
    let total = bytes_total.unwrap_or(transfer.bytes_total);
    let restarted = bytes_done < transfer.bytes_done;
    let advanced = bytes_done > transfer.bytes_done;
    // A restart invalidates the rate: the bytes that produced it belong to a run
    // that is over, and averaging them with the new run reports a speed that was
    // true of neither.
    let previous = if restarted {
        None
    } else {
        store.transfers.samples.get(id).copied()
    };

    let (speed_bps, eta_s) = match previous {
        None => {
            store.transfers.samples.insert(
                id.to_owned(),
                RateSample {
                    at_ms: now_ms,
                    bytes: bytes_done,
                    speed_bps: 0.0,
                },
            );
            (None, None)
        }
        Some(prior) if now_ms.saturating_sub(prior.at_ms) < MIN_RATE_DELTA_MS => {
            // Too soon since the last sample: count the bytes and leave the
            // sample alone, so the next tick's rate covers a longer window
            // instead of a shorter one.
            let speed_bps = (prior.speed_bps > 0.0).then_some(prior.speed_bps);
            (speed_bps, eta_from(speed_bps, total, bytes_done))
        }
        Some(prior) => {
            let delta_s = now_ms.saturating_sub(prior.at_ms) as f64 / 1000.0;
            let instantaneous = (bytes_done as f64 - prior.bytes as f64) / delta_s;
            let speed_bps = if prior.speed_bps > 0.0 {
                EMA_ALPHA * instantaneous + (1.0 - EMA_ALPHA) * prior.speed_bps
            } else {
                instantaneous
            }
            .max(0.0);
            store.transfers.samples.insert(
                id.to_owned(),
                RateSample {
                    at_ms: now_ms,
                    bytes: bytes_done,
                    speed_bps,
                },
            );
            let speed_bps = Some(speed_bps);
            (speed_bps, eta_from(speed_bps, total, bytes_done))
        }
    };
    Some(ProgressUpdate {
        bytes_done,
        bytes_total: total,
        speed_bps,
        eta_s,
        advanced,
        restarted,
    })
}

fn eta_from(speed_bps: Option<f64>, total: u64, done: u64) -> Option<u64> {
    let speed = speed_bps?;
    if speed <= 0.0 || total == 0 || done >= total {
        return None;
    }
    Some(((total - done) as f64 / speed).ceil() as u64)
}

/// Move a card to another state, if that is a legal edge from where it is.
///
/// A refused edge is a caller bug and is logged as one: `attachments.ts:138-149`
/// and `downloadWorkerFile.ts:31-65` are the only two callers, and neither
/// reaches a terminal card. Returns whether the card changed.
pub fn mark_transfer_state(
    store: &mut Store,
    id: &str,
    state: TransferState,
    err: Option<String>,
    now_ms: u64,
) -> bool {
    let Some(current) = store
        .transfers
        .transfers
        .get(id)
        .map(|transfer| transfer.state)
    else {
        return false;
    };
    if current == state && err.is_none() {
        return false;
    }
    if !current.may_follow(state) {
        tracing::warn!(
            target: "store",
            transfer = %id,
            from = current.as_str(),
            to = state.as_str(),
            "refused transfer transition"
        );
        return false;
    }
    let Some(transfer) = store.transfers.transfers.get_mut(id) else {
        return false;
    };
    transfer.state = state;
    transfer.err = err;
    // A settled card's speed and ETA describe a run that is over.
    transfer.speed_bps = None;
    transfer.eta_s = None;
    transfer.dismiss_at_ms = if state.self_dismisses() {
        Some(now_ms.saturating_add(TRANSFER_DISMISS_AFTER_MS))
    } else {
        None
    };
    transfer.last_progress_ms = if state.is_terminal() {
        None
    } else {
        Some(now_ms)
    };
    store.transfers.samples.remove(id);
    store.note_change();
    tracing::debug!(
        target: "store",
        transfer = %id,
        state = state.as_str(),
        "transfer state"
    );
    true
}

/// Remove one card, by hand or because its dismissal deadline passed.
pub fn remove_transfer(store: &mut Store, id: &str) -> bool {
    if !store.transfers.transfers.contains_key(id) {
        return false;
    }
    store.transfers.drop_card(id);
    store.note_change();
    true
}

/// Run the two transfer deadlines: call stalled cards stalled, and remove the
/// settled cards whose dismissal has come.
///
/// Called by the sweep. Returns whether any card changed, so a caller can skip a
/// repaint it has already accounted for.
pub fn sweep_transfers(store: &mut Store, now_ms: u64) -> bool {
    let stalled: Vec<String> = store
        .transfers
        .transfers
        .values()
        .filter(|transfer| {
            transfer.state == TransferState::Running
                && transfer
                    .last_progress_ms
                    .is_some_and(|last| now_ms.saturating_sub(last) >= TRANSFER_STALL_AFTER_MS)
        })
        .map(|transfer| transfer.id.clone())
        .collect();
    let due: Vec<String> = store
        .transfers
        .transfers
        .values()
        .filter(|transfer| {
            transfer
                .dismiss_at_ms
                .is_some_and(|deadline| now_ms >= deadline)
        })
        .map(|transfer| transfer.id.clone())
        .collect();
    if stalled.is_empty() && due.is_empty() {
        return false;
    }
    for id in &stalled {
        if let Some(transfer) = store.transfers.transfers.get_mut(id) {
            transfer.state = TransferState::Stalled;
        }
    }
    for id in &due {
        store.transfers.drop_card(id);
    }
    store.note_change();
    tracing::debug!(
        target: "store",
        stalled = stalled.len(),
        removed = due.len(),
        "transfer deadlines"
    );
    true
}

/// Remove every card, at a credential boundary.
///
/// A card names a file and a path belonging to the account that has just gone
/// away.
pub fn clear_transfers_for_account_boundary(store: &mut Store) {
    if clear_all(&mut store.transfers) {
        store.note_change();
    }
}

/// The clear itself, WITHOUT bumping `revision`, for a boundary that is clearing
/// several slices as one mutation. See `toasts::raise_toast` for why the inner
/// form exists.
pub fn clear_all(stack: &mut TransferStack) -> bool {
    let had_any = !stack.transfers.is_empty();
    let ids: Vec<String> = stack.transfers.keys().cloned().collect();
    for id in &ids {
        stack.drop_card(id);
    }
    had_any
}
