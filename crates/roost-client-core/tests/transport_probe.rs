#![allow(clippy::unwrap_used, clippy::expect_used)]

//! A worker control probe goes out on the route the session's input takes, and
//! only the connection that carried it can turn its answer into a round trip.
//!
//! The Sync half (an unsolicited answer, the coordinator's refusal) is in
//! `sync_decode_controls.rs`; this file is the elected loopback route.

mod direct_carrier_support;

use direct_carrier_support::*;
use roost_client_core::store::sync_feeds::ProbeRoute;
use roost_client_core::sync::inbound::TransportProbeResult;

/// A client whose pane's session is elected onto the loopback carrier.
fn elected_on_loopback() -> ClientCore {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));
    let _ = core.handle(direct_frame(accepted_view_state()));
    let _ = core.handle(direct_frame(baseline()));
    assert!(
        core.store().routes.route_matches(SESSION, &direct_token()),
        "the fixture elects the loopback route"
    );
    core
}

fn answer(request_id: &str) -> SyncFrame {
    SyncFrame::TransportProbeResult {
        result: TransportProbeResult {
            request_id: request_id.to_owned(),
            worker_fp: WORKER.to_owned(),
            worker_epoch: "worker-process".to_owned(),
        },
    }
}

#[test]
fn an_elected_loopback_route_is_probed_on_its_own_carrier() {
    let mut core = elected_on_loopback();

    let effects = core.handle(ClientEvent::TransportProbeRequested {
        session_id: SESSION.to_owned(),
        request_id: "probe-1".to_owned(),
    });

    let sent = effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::SendDirect {
                token,
                command: DirectCommand::TransportProbe { request_id, worker_fp, .. },
            } if *token == direct_token() && request_id == "probe-1" && worker_fp == WORKER
        )
    });
    assert!(sent, "the probe rides the elected carrier; got {effects:?}");
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::SendSync(_))),
        "and not the coordinator, which would measure a different path"
    );
}

#[test]
fn only_the_carrier_that_sent_the_probe_settles_it() {
    let mut core = elected_on_loopback();
    let _ = core.handle(ClientEvent::TransportProbeRequested {
        session_id: SESSION.to_owned(),
        request_id: "probe-1".to_owned(),
    });

    let mut replacement = direct_token();
    replacement.socket_generation += 1;
    let _ = core.handle(ClientEvent::DirectFrameReceived {
        token: replacement,
        frame: answer("probe-1"),
    });
    assert!(
        core.store().transport_probes.is_empty(),
        "an answer off another connection describes that connection, not this probe"
    );

    let _ = core.handle(direct_frame(answer("probe-1")));
    let sample = &core.store().transport_probes[WORKER];
    assert_eq!(sample.route, ProbeRoute::Direct(direct_token()));
    assert_eq!(sample.worker_epoch, "worker-process");
    assert!(core.store().pending_transport_probes.is_empty());
}
