#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Losing a carrier, and moving a session's panes back onto Sync.
//!
//! Three situations that all end the same way — the pane keeps painting, and the
//! worker stops holding a view for it: a carrier that only ever staged, a pane
//! that moves while an attempt is in flight, and an ELECTED route that dies. The
//! last is the expensive one, because the ids the dead worker held are ids the
//! coordinator has never heard of, so the rotation mints new ones and publishes
//! nothing on Sync until they arrive.
//!
//! `direct_carrier_staging.rs` covers the attempt these cases abandon.

mod direct_carrier_support;

use direct_carrier_support::*;

#[test]
fn a_candidate_only_carrier_loss_releases_the_view_it_published() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));

    let effects = core.handle(ClientEvent::CarrierLost {
        connection_id: "loopback-a".to_owned(),
    });

    assert_eq!(
        direct_publishes(&effects),
        vec![(SESSION.to_owned(), WIRE.to_owned())],
        "the worker's lease is released on the token it was taken on; got {effects:?}"
    );
    let release = effects.iter().find_map(|effect| match effect {
        Effect::SendDirect {
            command: DirectCommand::View { intent, .. },
            ..
        } => Some(*intent),
        _ => None,
    });
    assert_eq!(
        release,
        Some(ViewIntent::Unpublish),
        "and it is a removal, not another publish"
    );
    assert!(
        core.store().routes.candidate(SESSION).is_none(),
        "the attempt is gone with its connection"
    );
    assert!(
        core.store().terminal(SESSION).is_some_and(|replica| replica
            .view(VIEW)
            .is_some_and(|view| view.wire_view_id == VIEW)),
        "and the pane still holds the id Sync gave it, so the painted grid is untouched"
    );
}

#[test]
fn a_pane_that_moves_mid_stage_restages_with_the_new_intent() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));

    let effects = core.handle(ClientEvent::ViewResized {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
        cols: 100,
        rows: 40,
    });

    let restaged = effects.iter().find_map(|effect| match effect {
        Effect::MintTerminalViewId { attempt_id, .. } => Some(*attempt_id),
        _ => None,
    });
    assert_eq!(
        restaged,
        Some(2),
        "the attempt that snapshotted the old geometry is replaced by one that \
         will snapshot the new; got {effects:?}"
    );
}

#[test]
fn a_lost_direct_route_re_registers_its_panes_on_sync_under_fresh_ids() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));
    let _ = core.handle(direct_frame(accepted_view_state()));
    let _ = core.handle(direct_frame(baseline()));

    let lost = core.handle(ClientEvent::CarrierLost {
        connection_id: "loopback-a".to_owned(),
    });

    let rotation = core
        .store()
        .pending_sync_view_rotation
        .get(SESSION)
        .expect("the lost route's panes are re-registering");
    assert!(
        rotation.views.contains_key(VIEW),
        "under the PANE's own identity, which is what the renderer keeps"
    );
    let asked: Vec<u64> = lost
        .iter()
        .filter_map(|effect| match effect {
            Effect::MintTerminalViewId { attempt_id, .. } => Some(*attempt_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked,
        vec![rotation.attempt_id],
        "one fresh id is asked for, and it is the rotation's own attempt; got {lost:?}"
    );
    assert!(
        core.store()
            .terminal(SESSION)
            .is_some_and(|replica| replica.canonical().is_some()),
        "the painted grid survives the loss: the reader is not shown a blank pane"
    );
}

#[test]
fn a_rotation_that_mints_nothing_ends_and_keeps_the_painted_grid() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));
    let _ = core.handle(direct_frame(accepted_view_state()));
    let _ = core.handle(direct_frame(baseline()));
    let _ = core.handle(ClientEvent::CarrierLost {
        connection_id: "loopback-a".to_owned(),
    });

    let stale = core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: SESSION.to_owned(),
        attempt_id: 0,
        logical_view_id: VIEW.to_owned(),
        target: ViewIdTarget::SyncFallback,
        wire_view_id: Some(WIRE.to_owned()),
    });
    assert!(
        core.store()
            .pending_sync_view_rotation
            .contains_key(SESSION),
        "an answer for a rotation that has moved on changes nothing; got {stale:?}"
    );

    let effects = core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: SESSION.to_owned(),
        attempt_id: core
            .store()
            .pending_sync_view_rotation
            .get(SESSION)
            .map(|rotation| rotation.attempt_id)
            .expect("the rotation is waiting"),
        logical_view_id: VIEW.to_owned(),
        target: ViewIdTarget::SyncFallback,
        wire_view_id: None,
    });

    assert!(
        !core
            .store()
            .pending_sync_view_rotation
            .contains_key(SESSION),
        "an id that never came ends the rotation instead of being retried in a loop"
    );
    assert!(
        !sync_view_intents(&effects)
            .iter()
            .any(|(session, _, _)| session == SESSION),
        "and nothing is published on Sync carrying the dead carrier's id; got {effects:?}"
    );
    assert!(
        core.store()
            .terminal(SESSION)
            .is_some_and(|replica| replica.canonical().is_some()),
        "the painted grid is still the reader's last complete frame"
    );
}
