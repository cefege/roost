//! The effective geometry: which viewers bind a session's PTY, at what size,
//! and when a dead one stops binding it.
//!
//! The guards named in `docs/FAILURE-INDEX.md` ("A session stays clipped to a
//! viewer that is no longer looking") and the smoke spec
//! `terminal-multiview-geometry.spec.ts` are pinned here, because the whole
//! symptom is a terminal that reflows when a second browser opens or a first
//! one dies.

mod terminal_view_support;

use terminal_view_support::{
    decisions, Harness, FINGERPRINT, OTHER_FINGERPRINT, OTHER_VIEW, SESSION, VIEW,
};

use roost_proto::TerminalViewStatus;
use roost_protocol::viewport::{TERMINAL_VIEW_LEASE_MS, TERMINAL_VIEW_PARK_GRACE_MS};

/// A fixed instant every test in this file places its events at.
const T0: u64 = 1_000_000;

/// Two viewers claiming crossed axes run the PTY at the per-axis minimum of
/// their own claims, because the minimum is taken independently per axis.
#[test]
fn crossed_viewers_run_the_pty_at_the_per_axis_minimum() {
    let harness = Harness::unowned();
    let wide = harness.browser("socket-wide", FINGERPRINT, &[SESSION]);
    let narrow = harness.browser("socket-narrow", OTHER_FINGERPRINT, &[SESSION]);

    harness.view(&wide, VIEW, 120, 50, 1, true, T0);
    harness.view(&narrow, OTHER_VIEW, 60, 80, 1, true, T0);

    assert_eq!(
        harness.effective(T0),
        Some((60, 50)),
        "cols come from the narrow viewer and rows from the short one"
    );
}

/// A viewer's departure recomputes the effective geometry: a second browser
/// leaving is exactly the event that used to leave the survivor clipped.
#[test]
fn a_viewers_departure_recomputes_the_effective_geometry() {
    let harness = Harness::unowned();
    let wide = harness.browser("socket-wide", FINGERPRINT, &[SESSION]);
    let narrow = harness.browser("socket-narrow", OTHER_FINGERPRINT, &[SESSION]);

    harness.view(&wide, VIEW, 120, 50, 1, true, T0);
    harness.view(&narrow, OTHER_VIEW, 60, 80, 1, true, T0);
    assert_eq!(harness.effective(T0), Some((60, 50)), "both are binding");

    harness.release(&narrow, OTHER_VIEW, 2, T0 + 10);

    assert_eq!(
        harness.effective(T0 + 10),
        Some((120, 50)),
        "the survivor runs at its own size, not the departed viewer's minimum"
    );
    let states = decisions(&narrow.sink.states());
    assert_eq!(
        states.last(),
        Some(&(TerminalViewStatus::Accepted, 0, 0)),
        "a released view is acknowledged with no geometry: it has nothing left to paint"
    );
}

/// A dead viewer's lease stops it pinning the PTY. This is the behaviour the
/// smoke spec "a viewer whose socket dies stops pinning the PTY long before its
/// lease" asserts end to end, pinned here at the unit the decision is made in.
#[test]
fn an_expired_lease_stops_pinning_the_pty() {
    let harness = Harness::unowned();
    let wide = harness.browser("socket-wide", FINGERPRINT, &[SESSION]);
    let narrow = harness.browser("socket-narrow", OTHER_FINGERPRINT, &[SESSION]);
    harness.view(&wide, VIEW, 120, 50, 1, true, T0);
    harness.view(&narrow, OTHER_VIEW, 60, 80, 1, true, T0);
    let died_at = T0 + 100;
    harness.hub.close_socket("socket-narrow", died_at);

    assert_eq!(
        harness.effective(died_at + TERMINAL_VIEW_PARK_GRACE_MS - 1),
        Some((60, 50)),
        "inside the grace the record is still a member and still binds"
    );

    let after_grace = died_at + TERMINAL_VIEW_PARK_GRACE_MS;
    harness.hub.sweep(after_grace);
    assert_eq!(
        harness.effective(after_grace),
        Some((120, 50)),
        "the dead viewer stopped binding once its grace lapsed"
    );
    let stats = harness.hub.view_stats(&harness.session);
    assert_eq!(
        (stats.active, stats.parked),
        (1, 1),
        "the geometry moved because the grace lapsed, not because the record went"
    );

    let after_lease = died_at + TERMINAL_VIEW_LEASE_MS + 1;
    harness.hub.sweep(after_lease);
    let stats = harness.hub.view_stats(&harness.session);
    assert_eq!(
        (stats.active, stats.parked),
        (1, 0),
        "the lapsed lease reaped the claim a second later"
    );
    assert_eq!(
        harness.effective(after_lease),
        Some((120, 50)),
        "reaping a record nobody was holding changes nothing"
    );
}

/// A live socket that stops heartbeating is closed by its own lease, and the
/// hub says so on that socket rather than silently dropping its other sessions.
#[test]
fn a_live_view_whose_lease_lapses_closes_its_socket() {
    let harness = Harness::unowned();
    let quiet = harness.browser("socket-quiet", FINGERPRINT, &[SESSION]);
    harness.view(&quiet, VIEW, 100, 40, 1, true, T0);

    harness.hub.sweep(T0 + TERMINAL_VIEW_LEASE_MS);

    let effects = quiet.sink.effects();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, terminal_view_support::Recorded::Expired(_))),
        "the host was told to close the socket that stopped heartbeating: {effects:?}"
    );
    assert_eq!(
        harness.hub.view_stats(&harness.session),
        roost_coord::terminal_view::ViewStats::default(),
        "the record is gone with the lease"
    );
}

/// With no viewer left the last effective geometry is HELD, never re-minted, so
/// a link that flaps cannot become a stream re-mint storm.
#[test]
fn a_solo_viewers_geometry_is_held_across_a_link_blip() {
    let harness = Harness::unowned();
    let solo = harness.browser("socket-solo", FINGERPRINT, &[SESSION]);
    harness.view(&solo, VIEW, 100, 40, 1, true, T0);
    let blip_at = T0 + 100;
    harness.hub.close_socket("socket-solo", blip_at);

    for step in 1..=(TERMINAL_VIEW_PARK_GRACE_MS / 100) {
        let at = blip_at + step * 100;
        harness.hub.sweep(at);
        assert_eq!(
            harness.effective(at),
            Some((100, 40)),
            "the held geometry survives the whole blip at {at}"
        );
    }
    assert_eq!(
        harness.effective(blip_at + TERMINAL_VIEW_PARK_GRACE_MS + 1_000),
        Some((100, 40)),
        "and it is still held after the record was reaped"
    );
}

/// A viewer that resized while its socket was gone rejoins the aggregate at the
/// size it is painting now, not at the size it left at.
#[test]
fn a_viewer_resized_while_offline_reclaims_its_record_at_the_new_size() {
    let harness = Harness::unowned();
    let first = harness.browser("socket-tab1", FINGERPRINT, &[SESSION]);
    harness.view(&first, VIEW, 200, 60, 1, true, T0);
    harness.hub.close_socket("socket-tab1", T0 + 100);

    // Same fingerprint, same tab, so the same viewer key: this is the reclaim
    // path, not a fresh admission that would collide with the parked record.
    let again = harness.browser_with_tab(
        "socket-tab1-redial",
        FINGERPRINT,
        "tab-1",
        &[SESSION],
    );
    harness.view(&again, VIEW, 80, 24, 2, true, T0 + 200);

    assert_eq!(
        harness.effective(T0 + 200),
        Some((80, 24)),
        "the offline resize is adopted now rather than after the lease reaps it"
    );
    assert_eq!(
        harness.hub.view_stats(&harness.session).parked,
        0,
        "the record moved to the redialled socket instead of being duplicated"
    );
}

/// An owned session is never admitted locally, so the coordinator cannot become
/// a second minimizer for a PTY its worker already sizes.
#[test]
fn an_owned_session_is_relayed_rather_than_admitted_locally() {
    let harness = Harness::new();
    let browser = harness.browser("socket-a", FINGERPRINT, &[SESSION]);

    harness.view(&browser, VIEW, 120, 50, 1, true, T0);

    assert_eq!(
        harness.hub.view_stats(&harness.session),
        roost_coord::terminal_view::ViewStats::default(),
        "no local membership for an owned session"
    );
    assert_eq!(
        harness.transport.relayed().len(),
        1,
        "the command went to the owner instead"
    );
    assert_eq!(
        harness.effective(T0),
        None,
        "and the coordinator holds no geometry of its own for it"
    );
}
