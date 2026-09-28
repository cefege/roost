//! The tab header names a carrier only when the canonical replica owns a
//! baseline on it: a candidate that merely registered is never shown. Ports
//! `apps/web/tests/localTransportIndicator.test.ts` "reports only an elected
//! carrier that owns a baseline".

use roost_client_core::store::terminal_transport::{
    confirmed_transport, presentation_for, transport_attribute,
};
use roost_client_core::terminal::token::{TerminalToken, TerminalTransport};

fn loopback() -> TerminalToken {
    TerminalToken::direct(1, TerminalTransport::Loopback, "worker-fp", "worker-epoch", 0)
}

fn sync() -> TerminalToken {
    TerminalToken::sync(2, "sync-socket", "sync-epoch", 3)
}

#[test]
fn only_an_elected_carrier_that_owns_a_baseline_is_reported() {
    let direct = loopback();
    // No baseline yet: waiting, even with the route elected.
    assert_eq!(confirmed_transport(false, Some(&direct), Some(&direct)), None);
    assert_eq!(presentation_for(None).label, "Waiting");
    // A baseline on a direct generation whose route is not elected: waiting.
    assert_eq!(confirmed_transport(true, Some(&direct), None), None);
    // A route elected on a DIFFERENT generation does not confirm this one.
    let other = TerminalToken::direct(9, TerminalTransport::Loopback, "worker-fp", "worker-epoch", 0);
    assert_eq!(confirmed_transport(true, Some(&direct), Some(&other)), None);
    // Sync needs no election.
    let coordinator = sync();
    assert_eq!(
        confirmed_transport(true, Some(&coordinator), None),
        Some(TerminalTransport::Sync)
    );
    assert_eq!(presentation_for(Some(TerminalTransport::Sync)).label, "Coordinator");
    // No generation at all: waiting.
    assert_eq!(confirmed_transport(true, None, None), None);
}

#[test]
fn an_elected_direct_route_with_a_baseline_names_its_kind() {
    let direct = loopback();
    assert_eq!(
        confirmed_transport(true, Some(&direct), Some(&direct)),
        Some(TerminalTransport::Loopback)
    );
    let peer = TerminalToken::direct(4, TerminalTransport::Peer, "worker-fp", "epoch", 1);
    let kind = confirmed_transport(true, Some(&peer), Some(&peer));
    assert_eq!(kind, Some(TerminalTransport::Peer));
    let chip = presentation_for(kind);
    assert_eq!((chip.label, chip.kind_attribute()), ("WebRTC", "webrtc"));
    assert_eq!(presentation_for(None).kind_attribute(), "unconfirmed");
    assert_eq!(transport_attribute(TerminalTransport::Loopback), "loopback");
}
