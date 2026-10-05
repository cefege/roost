//! A lane whose input route the worker no longer honours, and the keystroke
//! that claims it back.
//!
//! The worker fences a session's input to the connection its last claim came
//! over, and refuses every other batch as `terminal input route changed`. A
//! Sync socket that redials, or a document that reloads in the same tab, leaves
//! that route on a connection this document no longer speaks on — the pane
//! reads Coordinator and takes no keystroke. Each case here is a batch that
//! must not go out epoch-less into that fence, and must go out, stamped, once
//! Sync has claimed the route back.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod direct_carrier_support;
mod route_claim_support;

use direct_carrier_support::*;
use roost_client_core::InputOutcome;
use roost_client_core::client::carriers::DirectGrant;
use roost_client_core::sync::SyncDomain;
use route_claim_support::*;

/// The id the host mints for the pane when Sync takes the session back.
const SYNC_WIRE: &str = "55555555-5555-4555-8555-555555555555";

/// A client whose peer route was lost, whose pane Sync took back, and whose
/// input route Sync claimed: the lane sends on Sync under `ROUTE_EPOCH`.
fn reclaimed_on_sync() -> ClientCore {
    let (mut core, effects) = promotable_peer();
    let (request_id, revision, _) = peer_claims(&effects)[0].clone();
    let _ = core.handle(on_peer(answer(&request_id, revision, true, "")));
    let lost = core.handle(ClientEvent::CarrierLost {
        connection_id: "peer-a".to_owned(),
    });
    let _ = core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: SESSION.to_owned(),
        attempt_id: mint_attempt(&lost),
        logical_view_id: VIEW.to_owned(),
        target: ViewIdTarget::SyncFallback,
        wire_view_id: Some(SYNC_WIRE.to_owned()),
    });
    let claimed = sync_claims(&core.handle(ClientEvent::Sweep { now_ms: 1 }));
    let (request_id, revision, _) = claimed[0].clone();
    let _ = on_sync(&mut core, answer(&request_id, revision, true, ""));
    let sent = typed(&mut core, b"a");
    assert_eq!(
        sync_inputs(&sent),
        vec![(b"a".to_vec(), ROUTE_EPOCH.to_owned())],
        "Sync carries input under the epoch it claimed"
    );
    core
}

/// Answer the one Sync claim among `effects`, and return what it released.
fn accept_sync_claim(core: &mut ClientCore, effects: &[Effect]) -> Vec<Effect> {
    let claims = sync_claims(effects);
    assert_eq!(claims.len(), 1, "one Sync claim; got {effects:?}");
    let (request_id, revision, worker_epoch) = claims[0].clone();
    assert_eq!(worker_epoch, PROCESS_EPOCH);
    on_sync(core, answer(&request_id, revision, true, ""))
}

#[test]
fn a_sync_redial_claims_the_route_back_before_the_next_keystroke_is_written() {
    let mut core = reclaimed_on_sync();
    let _ = core.handle(ClientEvent::SyncLinkClosed {
        generation: sync_generation(&core),
        close_code: Some(1006),
        close_reason: String::new(),
    });
    open_ready_link(&mut core, "socket-b");

    let held = typed(&mut core, b"b");
    assert!(
        sync_inputs(&held).is_empty(),
        "the worker heard the claim over the closed socket, so an epoch-less batch on the new one is refused; got {held:?}"
    );
    let released = accept_sync_claim(&mut core, &held);
    assert_eq!(
        sync_inputs(&released),
        vec![(b"b".to_vec(), ROUTE_EPOCH.to_owned())],
        "the keystroke goes out on the new socket under the epoch it claimed"
    );
}

#[test]
fn a_terminal_domain_reset_on_the_live_socket_keeps_the_route_epoch() {
    let mut core = reclaimed_on_sync();
    let reset = on_sync(
        &mut core,
        SyncFrame::DomainReset {
            domain: SyncDomain::Terminal,
            generation: 2,
            reason: "domain_overflow".to_owned(),
            subscribed: true,
        },
    );
    let _ = direct_carrier_support::support::hydration::answer_hydrations(&mut core, &reset);
    assert!(core.store().sync.domain_is_ready(SyncDomain::Terminal));

    let sent = typed(&mut core, b"c");
    assert_eq!(
        sync_inputs(&sent),
        vec![(b"c".to_vec(), ROUTE_EPOCH.to_owned())],
        "a domain reset is not a new connection, so the epoch still stands"
    );
}

/// The worker refuses a batch as `terminal input route changed` only before it
/// writes a byte, so the batch is held, not lost: the refusal itself claims Sync
/// back, and the batch goes out under the claimed epoch ahead of anything typed
/// after it.
#[test]
fn a_batch_refused_as_route_changed_waits_for_the_claim_and_is_sent_again_first() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::DirectGrantMinted { grant: grant() });
    let first = typed(&mut core, b"x");
    assert_eq!(
        sync_inputs(&first),
        vec![(b"x".to_vec(), String::new())],
        "a lane that never claimed sends epoch-less"
    );
    let input_seq = core.store().input.outstanding(SESSION)[0].input_seq;
    let refused = refuse(&mut core, input_seq, "terminal input route changed");

    assert_eq!(
        core.store().input.outstanding(SESSION)[0].input_seq,
        input_seq,
        "the refused batch is still outstanding, so no outcome reached the pane"
    );
    assert!(core.store().input.is_holding(SESSION));
    let claims = sync_claims(&refused);
    assert_eq!(
        claims.len(),
        1,
        "the refusal claims Sync back without waiting for a keystroke or a sweep; got {refused:?}"
    );

    let held = typed(&mut core, b"y");
    assert!(
        sync_inputs(&held).is_empty(),
        "a keystroke typed while the claim is out waits behind it; got {held:?}"
    );
    let (request_id, revision, _) = claims[0].clone();
    let released = on_sync(&mut core, answer(&request_id, revision, true, ""));
    assert_eq!(
        sync_input_seqs(&released),
        vec![
            (input_seq, b"x".to_vec(), ROUTE_EPOCH.to_owned()),
            (input_seq + 1, b"y".to_vec(), ROUTE_EPOCH.to_owned()),
        ],
        "the refused batch goes out first, under its own sequence and the claimed epoch"
    );
}

#[test]
fn a_refusal_for_another_reason_still_settles_as_rejected() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::DirectGrantMinted { grant: grant() });
    let _ = typed(&mut core, b"x");
    let input_seq = core.store().input.outstanding(SESSION)[0].input_seq;
    let refused = refuse(
        &mut core,
        input_seq,
        "terminal transport did not accept input",
    );

    assert!(sync_claims(&refused).is_empty(), "got {refused:?}");
    assert!(core.store().input.outstanding(SESSION).is_empty());
    assert!(!core.store().input.is_holding(SESSION));
    assert!(!core.store().input.is_blocked(SESSION));
}

/// The worker's refusal of `input_seq` on the live Sync socket.
fn refuse(core: &mut ClientCore, input_seq: u64, reason: &str) -> Vec<Effect> {
    let generation = core
        .store()
        .sync_terminal_token()
        .unwrap()
        .domain_generation;
    on_sync(
        core,
        SyncFrame::InputResult {
            session_id: SESSION.to_owned(),
            input_seq,
            generation,
            outcome: InputOutcome::Rejected {
                input_seq,
                reason: reason.to_owned(),
            },
        },
    )
}

/// Every `(input_seq, bytes, epoch)` written on Sync.
fn sync_input_seqs(effects: &[Effect]) -> Vec<(u64, Vec<u8>, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(SyncCommand::TerminalInput {
                input_seq,
                bytes,
                input_route_epoch,
                ..
            }) => Some((*input_seq, bytes.clone(), input_route_epoch.clone())),
            _ => None,
        })
        .collect()
}

/// The credential a reloaded document mints for its pane, naming the worker
/// process a reclaim has to name.
fn grant() -> DirectGrant {
    DirectGrant {
        grant_id: "grant-a".to_owned(),
        secret: "secret-a".to_owned(),
        worker_fp: WORKER.to_owned(),
        worker_epoch: PROCESS_EPOCH.to_owned(),
        tab_id: "tab-1".to_owned(),
        device_fingerprint: "device-a".to_owned(),
        session_ids: [SESSION.to_owned()].into_iter().collect(),
        peer_supported: false,
        input_route_supported: true,
        stun_urls: Vec::new(),
        expires_at_ms: u64::MAX,
    }
}
