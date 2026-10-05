#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The input route a promotion claims and a lost direct route reclaims.
//!
//! A worker writes a peer's input only under the route epoch it acknowledged
//! for that connection, so every case here is a batch that must NOT go out
//! before the acknowledgement and must go out, stamped, after it: held input
//! released with no epoch is refused as `terminal input route changed`, and
//! held input released early on the old route crosses the promotion fence.

mod direct_carrier_support;
mod route_claim_support;

use direct_carrier_support::*;
use roost_client_core::InputOutcome;
use route_claim_support::*;

#[test]
fn a_peer_promotion_holds_input_until_the_worker_acknowledges_the_route() {
    let (mut core, effects) = promotable_peer();
    let claims = peer_claims(&effects);
    assert_eq!(
        claims.len(),
        1,
        "one claim on the candidate; got {effects:?}"
    );
    let (request_id, revision, worker_epoch) = claims[0].clone();
    assert_eq!((revision, worker_epoch.as_str()), (1, PROCESS_EPOCH));
    assert!(
        core.store().routes.route(SESSION).is_none(),
        "the candidate is not the route until the worker says the input route moved"
    );

    let held = typed(&mut core, b"ls\r");
    assert!(
        peer_inputs(&held).is_empty() && sync_inputs(&held).is_empty(),
        "a keystroke typed during the claim waits; got {held:?}"
    );
    assert_eq!(core.store().input.outstanding(SESSION).len(), 1);

    let committed = core.handle(on_peer(answer(&request_id, revision, true, "")));
    assert_eq!(
        core.store().routes.route(SESSION).map(|route| &route.token),
        Some(&peer_token()),
        "the acknowledgement commits the candidate"
    );
    assert_eq!(
        peer_inputs(&committed),
        vec![(b"ls\r".to_vec(), ROUTE_EPOCH.to_owned())],
        "the held batch goes out on the peer stamped with the acknowledged epoch"
    );

    let fresh = typed(&mut core, b"pwd\r");
    assert_eq!(
        peer_inputs(&fresh),
        vec![(b"pwd\r".to_vec(), ROUTE_EPOCH.to_owned())],
        "and so does every batch after it"
    );
}

#[test]
fn a_stale_revision_is_claimed_once_more_above_the_workers_latest() {
    let (mut core, effects) = promotable_peer();
    let (request_id, revision, _) = peer_claims(&effects)[0].clone();

    let retried = core.handle(on_peer(answer(
        &request_id,
        revision,
        false,
        "stale_route_revision",
    )));
    let claims = peer_claims(&retried);
    assert_eq!(claims.len(), 1, "one retry; got {retried:?}");
    let (retry_id, retry_revision, _) = claims[0].clone();
    assert_ne!(retry_id, request_id, "a retry is a new request");
    assert_eq!(
        retry_revision,
        revision + 4,
        "one above the latest the worker holds"
    );

    let refused = core.handle(on_peer(answer(
        &retry_id,
        retry_revision,
        false,
        "stale_route_revision",
    )));
    assert!(
        peer_claims(&refused).is_empty(),
        "a second stale answer is not retried again; got {refused:?}"
    );
    assert!(
        core.store().routes.route(SESSION).is_none()
            && core.store().routes.candidate(SESSION).is_none(),
        "the refused promotion is abandoned and the fallback keeps the session"
    );
}

#[test]
fn a_started_sync_batch_drains_before_the_route_is_claimed() {
    let mut core = core_with_a_pane();
    let sent = typed(&mut core, b"x");
    assert_eq!(
        sync_inputs(&sent).len(),
        1,
        "Sync carries the first keystroke"
    );
    let input_seq = core.store().input.outstanding(SESSION)[0].input_seq;

    let staged = core.handle(ClientEvent::CarrierReady(peer_carrier()));
    let _ = minted(&mut core, mint_attempt(&staged), Some(WIRE));
    let _ = core.handle(on_peer(accepted_view_state()));
    let ready = core.handle(on_peer(baseline()));
    assert!(
        peer_claims(&ready).is_empty(),
        "the claim waits for the Sync batch it would otherwise fence; got {ready:?}"
    );
    assert!(core.store().input.is_holding(SESSION));

    let generation = core
        .store()
        .sync_terminal_token()
        .unwrap()
        .socket_generation;
    let settled = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::InputResult {
            session_id: SESSION.to_owned(),
            input_seq,
            generation: DOMAIN_GENERATION,
            outcome: InputOutcome::Accepted {
                input_seq,
                written_bytes: 1,
            },
        },
    });
    assert_eq!(
        peer_claims(&settled).len(),
        1,
        "the result that drains the old route sends the claim, with no sweep between; got {settled:?}"
    );
}

#[test]
fn a_lost_peer_route_reclaims_sync_before_released_input_is_written() {
    let (mut core, effects) = promotable_peer();
    let (request_id, revision, _) = peer_claims(&effects)[0].clone();
    let _ = core.handle(on_peer(answer(&request_id, revision, true, "")));

    let _ = core.handle(ClientEvent::CarrierLost {
        connection_id: "peer-a".to_owned(),
    });
    let held = typed(&mut core, b"y");
    assert!(
        sync_inputs(&held).is_empty(),
        "Sync input with no epoch is refused while the worker's route names the peer; got {held:?}"
    );

    let swept = core.handle(ClientEvent::Sweep { now_ms: 1 });
    let claim = swept.iter().find_map(|effect| match effect {
        Effect::SendSync(SyncCommand::TerminalInputRouteClaim {
            request_id,
            revision,
            worker_epoch,
            ..
        }) => Some((request_id.clone(), *revision, worker_epoch.clone())),
        _ => None,
    });
    let (sync_request, sync_revision, worker_epoch) =
        claim.unwrap_or_else(|| panic!("Sync claims the route back; got {swept:?}"));
    assert_eq!(worker_epoch, PROCESS_EPOCH);
    assert!(sync_revision > revision, "the reclaim is a newer revision");

    let generation = core
        .store()
        .sync_terminal_token()
        .unwrap()
        .socket_generation;
    let released = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: answer(&sync_request, sync_revision, true, ""),
    });
    assert_eq!(
        sync_inputs(&released),
        vec![(b"y".to_vec(), ROUTE_EPOCH.to_owned())],
        "the held batch goes out on Sync under the epoch Sync claimed"
    );
}

#[test]
fn a_straggling_sync_frame_after_the_promotion_leaves_input_on_the_peer() {
    let (mut core, effects) = promotable_peer();
    let (request_id, revision, _) = peer_claims(&effects)[0].clone();
    let _ = core.handle(on_peer(answer(&request_id, revision, true, "")));

    let generation = core
        .store()
        .sync_terminal_token()
        .unwrap()
        .socket_generation;
    let _ = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: continuation(),
    });
    assert_eq!(
        core.store()
            .terminal(SESSION)
            .and_then(|replica| replica.generation()),
        Some(&peer_token()),
        "the coordinator's late Sync frame does not take the replica back"
    );

    let typed_after = typed(&mut core, b"ls\r");
    assert_eq!(
        peer_inputs(&typed_after),
        vec![(b"ls\r".to_vec(), ROUTE_EPOCH.to_owned())],
        "the next keystroke still leaves on the claimed peer route; got {typed_after:?}"
    );
    assert!(sync_inputs(&typed_after).is_empty());
}

#[test]
fn a_closed_sync_socket_ends_its_fallback_claim_so_the_next_socket_claims_at_once() {
    let (mut core, effects) = promotable_peer();
    let (request_id, revision, _) = peer_claims(&effects)[0].clone();
    let _ = core.handle(on_peer(answer(&request_id, revision, true, "")));
    let _ = core.handle(ClientEvent::CarrierLost {
        connection_id: "peer-a".to_owned(),
    });
    let first = sync_claims(&core.handle(ClientEvent::Sweep { now_ms: 1 }));
    assert_eq!(first.len(), 1, "Sync claims the route back");

    let generation = core
        .store()
        .sync_terminal_token()
        .unwrap()
        .socket_generation;
    let _ = core.handle(ClientEvent::SyncLinkClosed {
        generation,
        close_code: Some(1006),
        close_reason: String::new(),
    });
    open_ready_link(&mut core, "socket-b");
    let retried = sync_claims(&core.handle(ClientEvent::Sweep { now_ms: 300 }));
    assert_eq!(
        retried.len(),
        1,
        "the claim the closed socket carried ended with it, so the new socket claims well before the 8 s answer deadline"
    );
    assert_ne!(retried[0].0, first[0].0, "a fresh claim, not the dead one");
}
