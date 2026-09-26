//! The owner-mode relay: a browser's view decision for an owned session is
//! forwarded, the owner's answer is applied, and a closed socket reaches the
//! owner that has to park its records.
//!
//! Ported from `apps/coord/tests/terminal/view/terminal-view-hub.test.ts` and
//! the relay path of `terminal-view-owner-relay.ts`.

mod terminal_view_support;

use terminal_view_support::{
    decisions, owner_state, watching, Harness, Recorded, Relayed, FINGERPRINT, OTHER_SESSION,
    SESSION, VIEW,
};

use roost_proto::{TerminalViewStatus, WTerminalViewProjection};

/// A fixed instant every test in this file places its events at.
const T0: u64 = 1_000_000;

/// A stream id the owner minted, in the uuid shape the wire requires.
const STREAM: &str = "5e6f7081-92a3-44b5-8f20-4b5c6d7e8f90";

/// An authorized view command for an owned session is forwarded to the owner
/// and answered by the owner, not by the coordinator.
#[test]
fn a_view_command_for_an_owned_session_is_relayed_to_its_owner() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);

    harness.view(&browser, VIEW, 120, 50, 1, true, T0);

    assert_eq!(
        harness.transport.relayed(),
        vec![Relayed::View {
            worker_fp: harness.worker.clone(),
            socket_id: "socket-a".to_owned(),
            view_id: VIEW.to_owned(),
        }],
        "the command crossed to the owner verbatim"
    );
    assert!(
        browser.sink.states().is_empty(),
        "the coordinator did not answer a decision the owner owns"
    );
}

/// A relay the transport refuses is told to the browser as UNAVAILABLE, and
/// still creates no local membership: admitting it here would make the
/// coordinator a second minimizer for a session the worker already sizes.
#[test]
fn a_refused_relay_tells_the_browser_the_terminal_is_unavailable() {
    let harness = Harness::new();
    harness
        .hub
        .set_owner_transport(terminal_view_support::RecordingTransport::dropping());
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);

    harness.view(&browser, VIEW, 120, 50, 1, true, T0);

    assert_eq!(
        decisions(&browser.sink.states()),
        vec![(TerminalViewStatus::Unavailable, 0, 0)],
        "the browser is told the terminal is gone, not that its geometry is refused"
    );
    assert_eq!(
        harness.hub.view_stats(&harness.session),
        roost_coord::terminal_view::ViewStats::default(),
        "no local membership was created to paper over the refusal"
    );
}

/// A view command for a session the socket was not admitted to is refused
/// BEFORE it is forwarded, so a browser cannot reach a session it never held.
#[test]
fn a_relay_refuses_a_session_the_socket_never_held() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);

    harness.hub.handle_view_command(
        &browser.socket_id,
        &roost_proto::TerminalViewCommand {
            session_id: OTHER_SESSION.to_owned(),
            view_id: VIEW.to_owned(),
            cols: 120,
            rows: 50,
            revision: 1,
            active: true,
            domain_generation: 1,
            __buffa_unknown_fields: Default::default(),
        },
        T0,
    );

    assert!(
        harness.transport.relayed().is_empty(),
        "an unauthorized session never crosses the link"
    );
    assert_eq!(
        decisions(&browser.sink.states()).last().map(|state| state.0),
        Some(TerminalViewStatus::Rejected),
        "and the browser is refused by name"
    );
}

/// The owner's answer installs the stream expectation before the browser sees
/// the frame, and only the decision that ATTACHES a socket seeds it: a lease
/// heartbeat re-declares the same view, and seeding on those would push a
/// duplicate full on every beat.
#[test]
fn an_owner_view_state_installs_the_stream_before_the_browser_sees_it() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);

    harness.hub.apply_owner_view_state(
        &harness.worker,
        "socket-a",
        &owner_state(VIEW, SESSION, 1, true, STREAM, 60, 50),
    );
    let effects = browser.sink.effects();
    let expect_at = effects
        .iter()
        .position(|effect| matches!(effect, Recorded::Expected { .. }))
        .expect("the stream was installed");
    let state_at = effects
        .iter()
        .position(|effect| matches!(effect, Recorded::State { .. }))
        .expect("the browser was answered");
    assert!(
        expect_at < state_at,
        "a replica told after the browser would fold the first cells against the old baseline: {effects:?}"
    );
    assert_eq!(
        watching(&effects),
        vec![(SESSION.to_owned(), true)],
        "the socket started watching"
    );
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, Recorded::Seeded(_)))
            .count(),
        1,
        "attaching seeds exactly one baseline"
    );

    // The next heartbeat for the same view is a renewal, not an attach.
    harness.hub.apply_owner_view_state(
        &harness.worker,
        "socket-a",
        &owner_state(VIEW, SESSION, 2, true, STREAM, 60, 50),
    );
    let effects = browser.sink.effects();
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, Recorded::Seeded(_)))
            .count(),
        1,
        "a renewal does not seed a second full: {effects:?}"
    );
}

/// A view the owner releases stops the socket's watch, and the replica is told
/// so before the next delta.
#[test]
fn an_owner_release_stops_the_sockets_watch() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);
    harness.hub.apply_owner_view_state(
        &harness.worker,
        "socket-a",
        &owner_state(VIEW, SESSION, 1, true, STREAM, 60, 50),
    );

    harness.hub.apply_owner_view_state(
        &harness.worker,
        "socket-a",
        &owner_state(VIEW, SESSION, 2, false, "", 0, 0),
    );

    assert_eq!(
        watching(&browser.sink.effects()).last(),
        Some(&(SESSION.to_owned(), false)),
        "the socket stopped watching the session its only view left"
    );
}

/// A view state from a worker that does not own the session is dropped: it
/// would otherwise install a stream the coordinator never asked for.
#[test]
fn a_view_state_from_a_worker_that_does_not_own_the_session_is_dropped() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);
    let stranger = roost_protocol::wire::WorkerFp::try_from(terminal_view_support::WORKER_FP)
        .unwrap();

    harness.hub.apply_owner_view_state(
        &stranger,
        "socket-a",
        &owner_state(VIEW, SESSION, 1, true, STREAM, 60, 50),
    );

    assert!(
        browser.sink.effects().is_empty(),
        "nothing reached the browser from a worker with no claim on the session"
    );
}

/// A closed socket is announced to every owner it reached, which is what lets
/// the owner's own registry park those views.
#[test]
fn a_closed_socket_reaches_the_owner_it_was_relayed_to() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);
    harness.view(&browser, VIEW, 120, 50, 1, true, T0);

    harness.hub.close_socket("socket-a", T0 + 10);

    assert_eq!(
        harness.transport.relayed().last(),
        Some(&Relayed::Closed {
            worker_fp: harness.worker.clone(),
            socket_id: "socket-a".to_owned(),
        }),
        "the owner was told, so its records park instead of pinning its PTY"
    );
}

/// The owner's published membership is the coordinator's answer for a respawn:
/// it is stored verbatim, so a respawn never re-derives a second geometry.
#[test]
fn the_owners_published_effective_is_what_a_respawn_reads() {
    use roost_coord::coord_core::seams::TerminalViewLifecycle;

    let harness = Harness::new();
    harness.hub.apply_owner_projection(
        &harness.worker,
        &WTerminalViewProjection {
            session_id: SESSION.to_owned(),
            viewers: vec![roost_proto::PbTerminalViewInput {
                fingerprint: FINGERPRINT.to_owned(),
                view_id: VIEW.to_owned(),
                cols: 90,
                rows: 30,
                parked: false,
                constrains: true,
                __buffa_unknown_fields: Default::default(),
            }],
            effective_cols: 90,
            effective_rows: 30,
            stream_id: STREAM.to_owned(),
            __buffa_unknown_fields: Default::default(),
        },
    );

    let geometry = harness
        .hub
        .effective_geometry(&harness.session)
        .expect("the owner published an effective geometry");
    assert_eq!(
        (geometry.cols, geometry.rows),
        (90, 30),
        "the coordinator reports the owner's own size, not a re-derived one"
    );
    assert_eq!(
        harness.effective(T0),
        None,
        "and it kept no local membership to disagree with"
    );
    assert_eq!(
        harness
            .hub
            .owner_for_session(SESSION)
            .map(|owner| owner == harness.worker),
        Some(true),
        "and it still knows who owns the session"
    );
}

/// Retiring a worker releases its ownership, its row and its sessions'
/// membership, so nothing is left pointing at a machine that is gone.
#[test]
fn retiring_a_worker_releases_its_ownership_and_its_sessions() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);
    harness.hub.apply_owner_projection(
        &harness.worker,
        &WTerminalViewProjection {
            session_id: SESSION.to_owned(),
            viewers: vec![roost_proto::PbTerminalViewInput {
                fingerprint: FINGERPRINT.to_owned(),
                view_id: VIEW.to_owned(),
                cols: 90,
                rows: 30,
                parked: false,
                constrains: true,
                __buffa_unknown_fields: Default::default(),
            }],
            effective_cols: 90,
            effective_rows: 30,
            stream_id: STREAM.to_owned(),
            __buffa_unknown_fields: Default::default(),
        },
    );
    let _ = browser;

    use roost_coord::coord_core::seams::TerminalViewLifecycle;
    harness
        .hub
        .notify_worker_retired(&harness.worker, std::slice::from_ref(&harness.session));

    assert_eq!(harness.hub.owner_for_session(SESSION), None);
    assert_eq!(
        harness.hub.effective_geometry(&harness.session),
        None,
        "a retired worker's published geometry is not an answer any more"
    );
}

/// A worker that says it does not own its views is authoritative for its own
/// fingerprint: a build that downgraded would otherwise keep having its
/// sessions relayed to a link that no longer speaks the relay.
#[test]
fn a_hello_without_the_capability_releases_ownership() {
    let harness = Harness::new();
    assert!(harness.hub.owner_for_session(SESSION).is_some());

    harness.hub.clear_owner(&harness.worker);

    assert_eq!(
        harness.hub.owner_for_session(SESSION),
        None,
        "the session is no longer owned, so its registry is the only minimizer"
    );
}
