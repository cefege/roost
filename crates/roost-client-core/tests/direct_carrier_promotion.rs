#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The promotion, and the one commit that makes it the session's route.
//!
//! The order is the whole safety argument: the worker's view-state answer
//! installs the stream; a complete full for THAT stream completes the baseline;
//! the route is elected; the replica is the candidate's; and only then is the
//! old Sync view released. Every case here breaks one link and asserts that the
//! promotion does not happen, because a route elected on a grid that never
//! arrived is worse than a session that stayed on the fallback.
//!
//! `direct_carrier_staging.rs` covers how the attempt got this far.

mod direct_carrier_support;

use direct_carrier_support::*;

#[test]
fn a_view_answer_then_a_full_elects_the_route_and_retires_the_sync_view_afterwards() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));

    // The frame arrives BEFORE the answer. Without an expected stream
    // `valid_full` refuses it, so a carrier that streamed first could never earn
    // a baseline — the cycle would wait on itself again.
    let _ = core.handle(direct_frame(baseline()));
    assert!(
        core.store().routes.route(SESSION).is_none(),
        "a full with no expected stream is not a baseline, so nothing is elected"
    );

    let _ = core.handle(direct_frame(accepted_view_state()));
    assert!(
        core.store().routes.route(SESSION).is_none(),
        "an answer installs the stream; the baseline still has to arrive"
    );

    let effects = core.handle(direct_frame(baseline()));

    let route = core
        .store()
        .routes
        .route(SESSION)
        .expect("the route is elected");
    assert_eq!(
        route.token,
        direct_token(),
        "the elected route is the carrier that earned it"
    );
    let replica = core
        .store()
        .terminal(SESSION)
        .expect("the canonical replica");
    assert_eq!(
        replica.expected_stream_id(),
        Some(STREAM),
        "the promoted replica is the candidate's, not the Sync one"
    );
    assert!(
        replica.baseline_ready() && replica.canonical().is_some(),
        "the promoted replica holds the complete baseline"
    );
    assert!(
        sync_view_intents(&effects).contains(&(
            SESSION.to_owned(),
            VIEW.to_owned(),
            ViewIntent::Unpublish
        )),
        "the previous Sync id is released only after the commit, and on Sync; got {effects:?}"
    );
}

/// A promoted route has to KEEP painting, and this is the case that says so.
///
/// The baseline that arrived with the commit is not evidence that the route
/// works — it was folded into a replica that was then handed over wholesale. The
/// next frame is the first one that has to be admitted by the route the commit
/// installed, and a fold that only knows about staged candidates answers it with
/// a discarded frame. The reader sees a pane that painted exactly once and then
/// stopped, with the header still claiming a live loopback carrier.
#[test]
fn the_elected_route_keeps_folding_frames_after_the_commit() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));
    let _ = core.handle(direct_frame(accepted_view_state()));
    let _ = core.handle(direct_frame(baseline()));
    let painted = core
        .store()
        .terminal(SESSION)
        .map(|replica| replica.frame_revision())
        .unwrap_or_default();

    let _ = core.handle(direct_frame(continuation()));

    let replica = core
        .store()
        .terminal(SESSION)
        .expect("the canonical replica");
    assert_eq!(
        replica.frame_revision(),
        painted + 1,
        "a frame on the ELECTED route is admitted by the replica the commit installed"
    );
    assert!(
        core.store().routes.candidate(SESSION).is_none(),
        "and there is no candidate left to have answered it"
    );
}

#[test]
fn the_same_pane_addresses_its_worker_by_the_minted_id_after_the_promotion() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));
    let _ = core.handle(direct_frame(accepted_view_state()));
    let _ = core.handle(direct_frame(baseline()));

    let resized = core.handle(ClientEvent::ViewResized {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
        cols: 100,
        rows: 40,
    });
    let closed = core.handle(ClientEvent::ViewClosed {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
    });

    assert_eq!(
        direct_publishes(&resized),
        vec![(SESSION.to_owned(), WIRE.to_owned())],
        "a resize is the same pane, so it goes out on the id the worker holds; got {resized:?}"
    );
    assert!(
        !direct_publishes(&resized)
            .iter()
            .any(|(_, view_id)| view_id == VIEW),
        "the pane's own id is never republished to a worker that already refused it"
    );
    assert_eq!(
        direct_publishes(&closed),
        vec![(SESSION.to_owned(), WIRE.to_owned())],
        "the removal names the id the worker holds, read before the record went; \
         got {closed:?}"
    );
    let removal = closed.iter().find_map(|effect| match effect {
        Effect::SendDirect {
            command: DirectCommand::View { intent, .. },
            ..
        } => Some(*intent),
        _ => None,
    });
    assert_eq!(
        removal,
        Some(ViewIntent::Unpublish),
        "a closed pane is removed from the worker, not left leased"
    );
}

#[test]
fn a_view_state_for_an_unpublished_id_changes_nothing() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));

    let _ = core.handle(direct_frame(SyncFrame::ViewState {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
        generation: SOCKET_GENERATION,
        accepted: true,
        stream_id: STREAM.to_owned(),
        effective_cols: COLS,
        effective_rows: ROWS,
    }));

    let effects = core.handle(direct_frame(baseline()));
    assert!(
        core.store().routes.route(SESSION).is_none(),
        "an answer for the pane's own id did not install a stream, so the full is \
         still not a baseline"
    );
    assert!(
        direct_publishes(&effects).is_empty(),
        "and nothing new went out; got {effects:?}"
    );
}

#[test]
fn a_frame_on_another_generation_does_not_elect() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));
    let _ = core.handle(direct_frame(accepted_view_state()));

    let other = TerminalToken::direct(
        2,
        TerminalTransport::Loopback,
        WORKER,
        PROCESS_EPOCH,
        DOMAIN_GENERATION,
    );
    let _ = core.handle(ClientEvent::DirectFrameReceived {
        token: other,
        frame: baseline(),
    });

    assert!(
        core.store().routes.route(SESSION).is_none(),
        "a full from a socket the attempt is not folded on cannot elect a route"
    );
}

#[test]
fn a_direct_input_result_settles_only_the_lane_that_carried_it() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));
    let _ = core.handle(direct_frame(accepted_view_state()));
    let _ = core.handle(direct_frame(baseline()));

    let _ = core.handle(ClientEvent::TerminalInput {
        session_id: SESSION.to_owned(),
        view_id: Some(VIEW.to_owned()),
        bytes: b"ls\r".to_vec(),
    });
    let input_seq = core
        .store()
        .input
        .outstanding(SESSION)
        .first()
        .expect("the batch was admitted")
        .input_seq;

    // A result on a generation that did not carry the batch settles nothing.
    let other_socket = TerminalToken::direct(
        2,
        TerminalTransport::Loopback,
        WORKER,
        PROCESS_EPOCH,
        DOMAIN_GENERATION,
    );
    let wrong_socket = core.handle(ClientEvent::DirectFrameReceived {
        token: other_socket,
        frame: SyncFrame::InputResult {
            session_id: SESSION.to_owned(),
            input_seq,
            generation: 2,
            outcome: roost_client_core::InputOutcome::Accepted {
                input_seq,
                written_bytes: 2,
            },
        },
    });
    assert!(
        wrong_socket.is_empty(),
        "a result from another socket is not this batch's answer; got {wrong_socket:?}"
    );
    assert_eq!(
        core.store().input.outstanding(SESSION).len(),
        1,
        "and the batch is still outstanding, which is the truth"
    );

    let settled = core.handle(direct_frame(SyncFrame::InputResult {
        session_id: SESSION.to_owned(),
        input_seq,
        generation: SOCKET_GENERATION,
        outcome: roost_client_core::InputOutcome::Accepted {
            input_seq,
            written_bytes: 2,
        },
    }));
    assert!(
        settled.is_empty(),
        "the settle is a store write, not a command; got {settled:?}"
    );
    assert!(
        core.store().input.outstanding(SESSION).is_empty(),
        "the batch the direct carrier wrote is answered by the direct carrier"
    );
}
