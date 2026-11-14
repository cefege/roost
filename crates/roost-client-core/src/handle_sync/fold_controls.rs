//! The pair, audit and control-lane folds: pair requests and the paired-browser
//! notice, live audit rows, UI commands for the bridge, a relocation notice,
//! and the answers to transport probes.
//!
//! Called by `apply_frame` only. Ported from `apps/web/src/store/sync-frame.ts`
//! (`pairRequestDelta` 285-350, `auditRow` 161-176, `uiCommand` 334-340),
//! `apps/web/src/lib/pairedBrowserNotice.ts`, and the `_dispatchSyncV2Control`
//! consumer in `sync-terminal-control-probe.ts:99-119`. Route-claim answers are
//! `handle_sync::promotion`'s.

use std::collections::BTreeSet;

use crate::effect::Effect;
use crate::store::Store;
use crate::store::mutations::{PairRequest, delete_pair_request};
use crate::store::sync_feeds::{ANNOUNCED_PAIRINGS_MAX, AUDIT_ROW_RING_MAX, UI_COMMAND_QUEUE_MAX};
use crate::store::toasts::{
    ToastId, ToastKind, ToastOptions, ToastSource, pair_request_toast_id, raise_toast,
    remove_pair_request_toast,
};
use crate::sync::SyncDomain;
use crate::sync::inbound::{AuditEntry, CoordinatorRelocation, PairRequestChange, PairedBrowser};

use super::close_failed::close_failed_sync_link;

/// One pair-request change. A snapshot REPLACES the set, so a removal missed
/// while disconnected cannot linger.
pub(super) fn fold_pair_request(
    store: &mut Store,
    change: &PairRequestChange,
    delivery_seq: u64,
    now_ms: u64,
) {
    match change {
        PairRequestChange::Pending(request) => {
            let previous = store.pair_requests.get(&request.ephemeral_id);
            let is_new = previous.is_none();
            if previous != Some(request) {
                store
                    .pair_requests
                    .insert(request.ephemeral_id.clone(), request.clone());
                // Only absent → present raises: a changed request already
                // carded must not bring back a card the operator dismissed.
                if is_new {
                    raise_pair_request_toast(store, request, now_ms);
                }
                store.note_change();
            }
            tracing::debug!(target: "sync", ephemeral_id = %request.ephemeral_id, "pair request pending");
        }
        PairRequestChange::Removed { ephemeral_id } => {
            delete_pair_request(store, ephemeral_id);
            if remove_pair_request_toast(&mut store.toasts, ephemeral_id) {
                store.note_change();
            }
        }
        PairRequestChange::Snapshot(pending) => replace_pair_requests(store, pending, now_ms),
        PairRequestChange::Completed(browser) => {
            let removed = store.pair_requests.remove(&browser.ephemeral_id).is_some();
            let uncarded = remove_pair_request_toast(&mut store.toasts, &browser.ephemeral_id);
            let announced = announce_paired_browser(store, browser, delivery_seq, now_ms);
            if removed || uncarded || announced {
                store.note_change();
            }
            tracing::info!(target: "sync", ephemeral_id = %browser.ephemeral_id, announced, "pairing completed");
        }
    }
}

/// Replace the pending set with an authoritative one — a Sync snapshot or the
/// hydration answer — raising a card for each request not held before and
/// dropping the card of each request that vanished.
pub(super) fn replace_pair_requests(store: &mut Store, pending: &[PairRequest], now_ms: u64) {
    let keep: BTreeSet<&str> = pending
        .iter()
        .map(|request| request.ephemeral_id.as_str())
        .collect();
    let mut vanished = Vec::new();
    store.pair_requests.retain(|ephemeral_id, _| {
        let kept = keep.contains(ephemeral_id.as_str());
        if !kept {
            vanished.push(ephemeral_id.clone());
        }
        kept
    });
    let mut changed = !vanished.is_empty();
    for ephemeral_id in &vanished {
        remove_pair_request_toast(&mut store.toasts, ephemeral_id);
    }
    for request in pending {
        let previous = store.pair_requests.get(&request.ephemeral_id);
        let is_new = previous.is_none();
        if previous != Some(request) {
            store
                .pair_requests
                .insert(request.ephemeral_id.clone(), request.clone());
            if is_new {
                raise_pair_request_toast(store, request, now_ms);
            }
            changed = true;
        }
    }
    if changed {
        store.note_change();
    }
    tracing::info!(target: "sync", pending = pending.len(), "pair request snapshot replaced the set");
}

/// The sticky "wants to pair" card for one pending request, with the button
/// that opens the approvals. The caller owns the revision bump.
fn raise_pair_request_toast(store: &mut Store, request: &PairRequest, now_ms: u64) {
    raise_toast(
        &mut store.toasts,
        pair_request_toast_id(&request.ephemeral_id),
        format!("{} wants to pair with Roost", request.announcement_label()),
        ToastKind::Warn,
        ToastOptions::with_ttl(None).with_pair_review_action(),
        now_ms,
    );
}

/// Raise "New browser paired" once per pairing however many frames report it
/// (v2 `announcePairedBrowser`). Returns whether a card was raised; the caller
/// owns the one revision bump for the whole change.
fn announce_paired_browser(
    store: &mut Store,
    browser: &PairedBrowser,
    delivery_seq: u64,
    now_ms: u64,
) -> bool {
    if store.announced_pairings.contains(&browser.ephemeral_id) {
        return false;
    }
    if store.announced_pairings.len() >= ANNOUNCED_PAIRINGS_MAX {
        store.announced_pairings.pop_front();
    }
    store
        .announced_pairings
        .push_back(browser.ephemeral_id.clone());
    let id = ToastId::new(
        ToastSource::Sync {
            domain: SyncDomain::Pair.as_str(),
            kind: "pair_request_delta",
            delivery_seq,
        },
        browser.ephemeral_id.clone(),
    );
    let message = format!("New browser paired: {}", browser.announcement_label());
    raise_toast(
        &mut store.toasts,
        id,
        message,
        ToastKind::Ok,
        ToastOptions::plain(),
        now_ms,
    );
    true
}

/// One live audit row, newest first, deduplicated by id as the audit pane does
/// (`AuditLogPane.tsx:123-129`), keeping the newest [`AUDIT_ROW_RING_MAX`].
pub(super) fn fold_audit_row(store: &mut Store, row: &AuditEntry) {
    if store.audit_rows.iter().any(|held| held.id == row.id) {
        tracing::trace!(target: "sync", audit_id = row.id, "audit row already held");
        return;
    }
    store.audit_rows.push_front(row.clone());
    store.audit_rows.truncate(AUDIT_ROW_RING_MAX);
    store.note_change();
    tracing::trace!(target: "sync", audit_id = row.id, "audit row folded");
}

/// Queue one UI command for the UI bridge, dropping the oldest past the bound.
pub(super) fn fold_ui_command(store: &mut Store, command: &roost_proto::UiCommandFrame) {
    if store.ui_commands.len() >= UI_COMMAND_QUEUE_MAX {
        store.ui_commands.pop_front();
        tracing::warn!(target: "sync", "ui command queue full; oldest command dropped");
    }
    store.ui_commands.push_back(command.clone());
    store.note_change();
    tracing::debug!(
        target: "sync",
        target_tab_id = %command.target_tab_id,
        correlation_id = %command.correlation_id,
        "ui command queued for the bridge"
    );
}

/// A relocation notice closes the link. v2 has no handler for this control:
/// `handleV2Control`'s default returns false and `_consumeSyncFrame` throws
/// "unknown v2 control" into `_closeFailedSyncLink`
/// (`sync-inbound.ts:233-235`, `sync-inbound.ts:62-64`).
pub(super) fn fold_coordinator_relocation(
    store: &mut Store,
    generation: u64,
    relocation: &CoordinatorRelocation,
    out: &mut Vec<Effect>,
) {
    close_failed_sync_link(
        store,
        generation,
        format!(
            "coordinator_relocation {} has no v2 handler (target {})",
            relocation.handoff_id, relocation.target_url
        ),
        out,
    );
}
