//! The move back onto Sync after an elected direct route is lost.
//!
//! A pane's identity is the renderer's and never changes; the id its authority
//! holds is per transport. When a direct worker dies, the id it held belongs to
//! a machine that is no longer serving this session, and the coordinator has
//! never heard of it — so the rotation mints a NEW id per pane, keeps the painted
//! grid up, and publishes nothing on Sync at all until those answers arrive.
//!
//! Holds no timer. The retry is the next terminal-ready transition, because
//! minting again on every sweep would be a loop rather than a retry.

use std::collections::BTreeMap;

use crate::effect::{Effect, ViewIdTarget};
use crate::handle_sweep::publish_view_on_token;
use crate::store::view_rotation::RotationView;
use crate::store::{Store, SyncViewRotation};
use crate::terminal::TerminalToken;
use crate::terminal::view::ViewIntent;

use super::{
    MintedViewId, canonical_still_matches, is_mintable_view_id, wire_id_is_live_elsewhere,
};

/// An elected direct route is gone: mint fresh ids for the panes it held, and
/// keep painting what they last had until Sync answers.
pub fn begin_sync_view_rotation(
    store: &mut Store,
    session_id: &str,
    old_route_token: &TerminalToken,
    out: &mut Vec<Effect>,
) {
    let Some(replica) = store.terminal(session_id) else {
        return;
    };
    let views: BTreeMap<String, RotationView> = replica
        .views()
        .values()
        .filter(|view| view.intent != ViewIntent::Unpublish)
        .map(|view| {
            (
                view.view_id.clone(),
                RotationView {
                    intent: view.intent,
                    source_revision: view.revision,
                },
            )
        })
        .collect();
    if views.is_empty() {
        return;
    }
    let attempt_id = store.next_attempt_id;
    store.next_attempt_id += 1;
    let session = session_id.to_string();
    store.pending_sync_view_rotation.insert(
        session_id.to_string(),
        SyncViewRotation {
            attempt_id,
            old_route_token: old_route_token.clone(),
            views: views.clone(),
        },
    );
    store.note_change();
    tracing::info!(
        target: "route",
        session_id,
        attempt_id,
        views = views.len(),
        "a direct route was lost; its panes are re-registering on Sync under fresh ids"
    );
    for logical_view_id in views.into_keys() {
        out.push(Effect::MintTerminalViewId {
            session_id: session.clone(),
            attempt_id,
            logical_view_id,
            target: ViewIdTarget::SyncFallback,
        });
    }
}

/// Record a minted id on the Sync rotation, and publish that view on Sync.
pub(super) fn adopt_sync_view_id(
    store: &mut Store,
    minted: MintedViewId<'_>,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let MintedViewId {
        session_id,
        attempt_id,
        logical_view_id,
        wire_view_id,
        ..
    } = minted;
    let Some((intent, source_revision)) =
        rotation_snapshot(store, session_id, attempt_id, logical_view_id)
    else {
        return;
    };
    if !canonical_still_matches(store, session_id, logical_view_id, intent, source_revision) {
        forget_rotation_view(store, session_id, logical_view_id, "the pane moved");
        return;
    }
    let Some(wire_view_id) = usable_wire_id(wire_view_id) else {
        forget_rotation_view(
            store,
            session_id,
            logical_view_id,
            "the host minted no usable id",
        );
        return;
    };
    if wire_id_is_live_elsewhere(store, &wire_view_id) {
        forget_rotation_view(
            store,
            session_id,
            logical_view_id,
            "the minted view id collides",
        );
        return;
    }
    let adopted = store
        .terminal_mut_if_present(session_id)
        .and_then(|replica| replica.view_mut(logical_view_id))
        .is_some_and(|view| {
            view.adopt_wire_view_id(wire_view_id.clone());
            true
        });
    if !adopted {
        return;
    }
    forget_rotation_view(store, session_id, logical_view_id, "adopted");
    store.note_change();
    tracing::info!(
        target: "route",
        session_id,
        attempt_id,
        view_id = logical_view_id,
        wire_view_id = %wire_view_id,
        "a lost direct route's pane re-registered on Sync under a fresh id"
    );
    // Bound to the Sync socket explicitly, not through the publication target:
    // the target is a rule about which route is elected, and this is the one
    // place where the answer is already known to be Sync.
    if let Some(token) = store.sync_terminal_token() {
        publish_view_on_token(store, &token, session_id, logical_view_id, now_ms, out);
    }
}

/// A host answer that is a v4 UUID, or `None`.
fn usable_wire_id(wire_view_id: Option<&str>) -> Option<String> {
    wire_view_id
        .filter(|id| is_mintable_view_id(id))
        .map(str::to_string)
}

/// The rotation's own snapshot of one pane, when that rotation is still asking.
fn rotation_snapshot(
    store: &Store,
    session_id: &str,
    attempt_id: u64,
    logical_view_id: &str,
) -> Option<(ViewIntent, u64)> {
    let rotation = store.pending_sync_view_rotation.get(session_id)?;
    if rotation.attempt_id != attempt_id {
        tracing::debug!(
            target: "route",
            session_id,
            attempt_id,
            view_id = logical_view_id,
            "a view id answered a rotation that has moved on"
        );
        return None;
    }
    let view = rotation.views.get(logical_view_id)?;
    Some((view.intent, view.source_revision))
}

/// Drop one pane from a rotation, and the rotation with it when it was the last.
fn forget_rotation_view(store: &mut Store, session_id: &str, logical_view_id: &str, reason: &str) {
    let removed = store
        .pending_sync_view_rotation
        .get_mut(session_id)
        .is_some_and(|rotation| rotation.views.remove(logical_view_id).is_some());
    if !removed {
        return;
    }
    store.note_change();
    tracing::debug!(
        target: "route",
        session_id,
        view_id = logical_view_id,
        reason,
        "a pane left the Sync rotation"
    );
    if store
        .pending_sync_view_rotation
        .get(session_id)
        .is_some_and(|rotation| rotation.views.is_empty())
    {
        store.pending_sync_view_rotation.remove(session_id);
    }
}
