//! Membership rules: which declaration is accepted, which is refused, and what
//! each refusal costs the client.
//!
//! Ported from the v2 guards `terminal-view-hub.test.ts` and
//! `terminal-view-registry-membership.test.ts`. The questions here are the ones
//! where a wrong answer is silent: a stale revision accepted, a session moved
//! under a live handle, a revoked device keeping a claim.

mod terminal_view_support;

use terminal_view_support::{
    decisions, watching, Harness, Recorded, FINGERPRINT, OTHER_FINGERPRINT, OTHER_SESSION, SESSION,
    VIEW,
};

use roost_proto::{TerminalViewCommand, TerminalViewStatus};

/// A fixed instant every test in this file places its events at.
const T0: u64 = 1_000_000;

/// The harness's session, as a command field.
const SESSION_ID: &str = SESSION;

fn command(view_id: &str, cols: u32, rows: u32, revision: u64, active: bool) -> TerminalViewCommand {
    TerminalViewCommand {
        view_id: view_id.to_owned(),
        session_id: SESSION_ID.to_owned(),
        cols,
        rows,
        revision,
        active,
        domain_generation: 1,
        __buffa_unknown_fields: Default::default(),
    }
}

/// A well-formed uuid that is not a session the socket was admitted to.
const UNADMITTED: &str = "4d5e6f70-8192-43a4-8e1f-3a4b5c6d7e8f";

/// A declaration outside the trust boundary is refused with the reason named,
/// and leaves membership untouched.
#[test]
fn a_declaration_outside_the_trust_boundary_is_refused_by_name() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);

    harness.hub.handle_view_command(
        &browser.socket_id,
        &TerminalViewCommand {
            view_id: "not-a-uuid".to_owned(),
            ..command(VIEW, 100, 40, 1, true)
        },
        T0,
    );
    harness.hub.handle_view_command(
        &browser.socket_id,
        &command(UNADMITTED, 100, 40, 1, true),
        T0,
    );
    harness
        .hub
        .handle_view_command(&browser.socket_id, &command(VIEW, 0, 0, 0, true), T0);

    let reasons: Vec<(TerminalViewStatus, u32, u32)> = decisions(&browser.sink.states());
    assert_eq!(reasons.len(), 3, "every refusal answers: {reasons:?}");
    assert!(
        reasons
            .iter()
            .all(|(status, cols, rows)| *status == TerminalViewStatus::Rejected
                && *cols == 0
                && *rows == 0),
        "a refused command carries no geometry: {reasons:?}"
    );
    assert_eq!(
        harness.hub.view_stats(&harness.session),
        roost_coord::terminal_view::ViewStats::default(),
        "no refusal created membership"
    );
}

/// A stale revision is refused, and the record keeps the revision it had.
#[test]
fn a_stale_revision_is_refused_and_the_record_keeps_its_revision() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);
    harness.view(&browser, VIEW, 100, 40, 5, true, T0);

    harness.view(&browser, VIEW, 100, 40, 4, true, T0 + 10);

    let states = decisions(&browser.sink.states());
    assert_eq!(
        states.last().map(|state| state.0),
        Some(TerminalViewStatus::Rejected),
        "a replay below the record's revision is refused: {states:?}"
    );
    assert_eq!(
        harness.effective(T0 + 10),
        Some((100, 40)),
        "and the record still describes the geometry it admitted"
    );
}

/// A same-revision heartbeat whose geometry CONFLICTS is refused, but its lease
/// is still renewed: the command proves the socket is alive, and dropping the
/// renewal would park every other session that socket was watching.
#[test]
fn a_conflicting_heartbeat_is_refused_but_renews_the_lease() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID, OTHER_SESSION]);
    harness.view(&browser, VIEW, 100, 40, 5, true, T0);

    harness.view(&browser, VIEW, 90, 40, 5, true, T0 + 1_000);
    assert_eq!(
        decisions(&browser.sink.states()).last().map(|s| s.0),
        Some(TerminalViewStatus::Rejected),
        "the conflicting geometry is refused"
    );
    assert_eq!(
        harness.effective(T0 + 1_000),
        Some((100, 40)),
        "and the admitted geometry is unchanged"
    );

    // A sweep just before the renewed deadline must not reap it.
    harness.hub.sweep(T0 + 1_000 + 14_000);
    assert_eq!(
        harness.hub.view_stats(&harness.session).active,
        1,
        "the refused command still proved liveness"
    );
}

/// A view cannot change sessions under a live handle: a client that renames its
/// handle would otherwise strand the old record on the old session forever.
#[test]
fn a_view_cannot_change_sessions() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID, OTHER_SESSION]);
    harness.view(&browser, VIEW, 100, 40, 1, true, T0);

    harness.hub.handle_view_command(
        &browser.socket_id,
        &TerminalViewCommand {
            session_id: OTHER_SESSION.to_owned(),
            ..command(VIEW, 100, 40, 2, true)
        },
        T0 + 10,
    );

    assert_eq!(
        decisions(&browser.sink.states()).last().map(|s| s.0),
        Some(TerminalViewStatus::Rejected),
        "moving a live handle to another session is refused"
    );
    assert_eq!(
        harness.hub.view_stats(&harness.session).active,
        1,
        "the record stayed on the session it was admitted for"
    );
}

/// Another socket cannot take a handle that a LIVE socket still owns. The
/// viewer key is per tab, so this is a reconnect racing its own predecessor.
#[test]
fn a_live_handle_cannot_be_taken_by_another_socket() {
    let harness = Harness::unowned();
    let first = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);
    let second = harness.browser_with_tab("socket-b", FINGERPRINT, "tab-1", &[SESSION_ID]);
    harness.view(&first, VIEW, 100, 40, 1, true, T0);

    harness.view(&second, VIEW, 100, 40, 9, true, T0 + 10);

    assert_eq!(
        decisions(&second.sink.states()).last().map(|s| s.0),
        Some(TerminalViewStatus::Rejected),
        "the second socket is told the handle is owned by a live one"
    );
    assert_eq!(
        harness.effective(T0 + 10),
        Some((100, 40)),
        "and the record still belongs to the first socket"
    );
}

/// Membership that leaves tells the host to stop feeding that socket, and
/// membership that arrives tells it to start.
#[test]
fn a_socket_is_told_when_it_starts_and_stops_watching_a_session() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);

    harness.view(&browser, VIEW, 100, 40, 1, true, T0);
    harness.release(&browser, VIEW, 2, T0 + 10);

    assert_eq!(
        watching(&browser.sink.effects()),
        vec![
            (SESSION_ID.to_owned(), true),
            (SESSION_ID.to_owned(), false)
        ],
        "one feed per socket-session pair, started and stopped"
    );
}

/// A revoked device loses its records, its claims and its sockets in one step,
/// and its sessions are re-minimized without it.
#[test]
fn a_revoked_device_loses_its_membership_and_its_claims() {
    let harness = Harness::unowned();
    let revoked = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);
    let survivor = harness.browser("socket-b", OTHER_FINGERPRINT, &[SESSION_ID]);
    harness.view(&revoked, VIEW, 60, 20, 1, true, T0);
    harness.view(&survivor, VIEW, 120, 50, 1, true, T0);
    harness.release(&revoked, VIEW, 2, T0 + 10);
    assert_eq!(harness.effective(T0 + 10), Some((120, 50)), "released already");

    // Re-admit at a small size, then revoke without an explicit release.
    harness.view(&revoked, VIEW, 40, 20, 3, true, T0 + 20);
    assert_eq!(
        harness.effective(T0 + 20),
        Some((40, 20)),
        "clipped again on both axes"
    );

    harness.hub.remove_fingerprint(FINGERPRINT, T0 + 30);

    assert_eq!(
        harness.effective(T0 + 30),
        Some((120, 50)),
        "the revoked device stopped binding the PTY"
    );
    assert_eq!(
        harness.hub.view_stats(&harness.session).active,
        1,
        "and only the survivor is left"
    );
}

/// A released view's claim outlives the record for one lease, so the same tab
/// can reclaim the handle; a lower revision than the claim is refused.
#[test]
fn a_released_claim_is_reclaimable_and_orders_revisions() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);
    harness.view(&browser, VIEW, 100, 40, 7, true, T0);
    harness.release(&browser, VIEW, 8, T0 + 10);

    // A fresh handle for the same view id, from the same tab: refused, because
    // the claim says revision 8 already happened.
    harness.view(&browser, VIEW, 100, 40, 8, true, T0 + 20);
    assert_eq!(
        decisions(&browser.sink.states()).last().map(|s| s.0),
        Some(TerminalViewStatus::Rejected),
        "a same-revision ACTIVE declaration cannot revive a released claim"
    );

    harness.view(&browser, VIEW, 100, 40, 9, true, T0 + 30);
    assert_eq!(
        harness.effective(T0 + 30),
        Some((100, 40)),
        "a higher revision reclaims the handle"
    );
}

/// A session close releases every record and every claim naming it, so a
/// recycled session id cannot inherit a dead viewer's geometry.
#[test]
fn closing_a_session_releases_its_records_and_claims() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);
    harness.view(&browser, VIEW, 100, 40, 1, true, T0);
    harness.release(&browser, VIEW, 2, T0 + 10);

    harness.hub.close_session(&harness.session, T0 + 20);

    assert_eq!(
        harness.hub.view_stats(&harness.session),
        roost_coord::terminal_view::ViewStats::default(),
        "no record survives the close"
    );
    assert_eq!(
        harness.effective(T0 + 20),
        None,
        "and the session has no effective geometry any more"
    );
    harness.view(&browser, VIEW, 100, 40, 1, true, T0 + 30);
    assert_eq!(
        harness.effective(T0 + 30),
        Some((100, 40)),
        "the same handle is admissible again, because the claim went with it"
    );
}

/// A resync is served only for a live record the socket actually owns; a
/// stranger's resync is silently dropped rather than answered.
#[test]
fn a_resync_is_served_only_for_the_records_its_socket_owns() {
    let harness = Harness::unowned();
    let owner = harness.browser("socket-a", FINGERPRINT, &[SESSION_ID]);
    let stranger = harness.browser("socket-b", OTHER_FINGERPRINT, &[SESSION_ID]);
    harness.view(&owner, VIEW, 100, 40, 1, true, T0);
    let resync = roost_proto::TerminalResyncCommand {
        view_id: VIEW.to_owned(),
        session_id: SESSION_ID.to_owned(),
        stream_id: "stream-1".to_owned(),
        grid_epoch: "epoch-1".to_owned(),
        seq: 42,
        domain_generation: 1,
        __buffa_unknown_fields: Default::default(),
    };

    harness.hub.handle_resync(&stranger.socket_id, &resync, T0 + 10);
    harness.hub.handle_resync(&owner.socket_id, &resync, T0 + 10);

    assert!(
        !stranger
            .sink
            .effects()
            .iter()
            .any(|effect| matches!(effect, Recorded::Resynced(_))),
        "a resync for another socket's handle is dropped"
    );
    assert_eq!(
        owner.sink
            .effects()
            .iter()
            .filter(|effect| matches!(effect, Recorded::Resynced(_)))
            .count(),
        1,
        "and the owning socket is served forward from its checkpoint"
    );
}
