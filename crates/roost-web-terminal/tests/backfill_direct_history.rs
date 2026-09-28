//! Direct history carrier selection, isolated from renderer paging: history
//! uses only the exact elected route, and the coordinator is asked once only
//! after a direct read explicitly fails or exceeds its limit. Ports
//! `apps/web/tests/scrollbackDirectHistory.test.ts`.

use roost_client_core::terminal::routes::SessionRoute;
use roost_client_core::terminal::token::{TerminalToken, TerminalTransport};
use roost_web_terminal::backfill::{
    DirectHistoryFailure, DirectHistoryOutcome, DirectReadError, direct_history_outcome,
    elected_direct_route,
};

fn direct_token(domain_generation: u64) -> TerminalToken {
    TerminalToken::direct(
        9,
        TerminalTransport::Loopback,
        "direct-history-worker-fp",
        "direct-history-worker",
        domain_generation,
    )
}

fn route() -> SessionRoute {
    SessionRoute {
        connection_id: "direct-history-socket".to_string(),
        token: direct_token(4),
    }
}

#[test]
fn uses_the_exact_elected_direct_connection_before_coordinator_history() {
    let route = route();
    let elected = elected_direct_route(Some(&route), Some(&direct_token(4)));
    assert_eq!(elected, Some(&route));
    assert_eq!(
        direct_history_outcome(&route, elected, DirectReadError::parse("")),
        DirectHistoryOutcome::Use
    );
}

#[test]
fn a_route_whose_current_generation_token_differs_reads_from_the_coordinator() {
    let route = route();
    assert_eq!(
        elected_direct_route(Some(&route), Some(&direct_token(5))),
        None
    );
    assert_eq!(elected_direct_route(Some(&route), None), None);
    assert_eq!(elected_direct_route(None, Some(&direct_token(4))), None);
}

#[test]
fn falls_back_once_when_the_direct_history_request_fails() {
    let route = route();
    assert_eq!(
        direct_history_outcome(&route, Some(&route), Some(DirectReadError::Transport)),
        DirectHistoryOutcome::FallBackToCoordinator
    );
    // A route that moved while the read was out is not a transport problem to
    // mask: the read fails rather than answer for another carrier's session.
    let moved = SessionRoute {
        connection_id: "another-socket".to_string(),
        ..route.clone()
    };
    for elected_now in [None, Some(&moved)] {
        assert_eq!(
            direct_history_outcome(&route, elected_now, Some(DirectReadError::Transport)),
            DirectHistoryOutcome::Fail(DirectHistoryFailure::RouteChanged)
        );
    }
}

#[test]
fn falls_back_once_when_direct_history_exceeds_its_transport_limit() {
    let route = route();
    let error = DirectReadError::parse("scrollback response exceeds direct transport limit");
    assert_eq!(error, Some(DirectReadError::Overlimit));
    assert_eq!(
        direct_history_outcome(&route, Some(&route), error),
        DirectHistoryOutcome::FallBackToCoordinator
    );
}

#[test]
fn does_not_mask_a_direct_history_rejection_as_coordinator_fallback() {
    let route = route();
    let error = DirectReadError::parse("direct reader unavailable");
    let outcome = direct_history_outcome(&route, Some(&route), error);
    assert_eq!(
        outcome,
        DirectHistoryOutcome::Fail(DirectHistoryFailure::Rejected)
    );
}
