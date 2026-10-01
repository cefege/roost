//! The workbench chrome's decisions: the size-class boundary, the route
//! context text, the activity rail's active states, and the coordinator
//! staleness window. Native, because every one of them is a function of values
//! the component already holds.
//!
//! These live in a sibling `tests/` binary rather than an inline module because
//! the module they exercise has to stay under the file cap with its own header
//! and its own reasons; the assertions are about the shipped behaviour, not
//! about where they are written.

use roost_web::components::layout::shell_metrics::*;

#[test]
fn a_phone_in_landscape_is_still_compact() {
    // The short side decides, so rotating does not hand a phone the desktop
    // rail, which its width alone would.
    assert_eq!(classify(844, 390), SizeClass::Compact);
    assert_eq!(classify(390, 844), SizeClass::Compact);
}

#[test]
fn a_tablet_short_side_keeps_the_desktop_rail() {
    assert_eq!(classify(744, 1024), SizeClass::Desktop);
    assert_eq!(classify(1024, 744), SizeClass::Desktop);
}

#[test]
fn the_compact_boundary_is_exclusive() {
    assert_eq!(classify(COMPACT_MAX_PX, 900), SizeClass::Desktop);
    assert_eq!(classify(COMPACT_MAX_PX - 1, 900), SizeClass::Compact);
}

#[test]
fn every_terminal_route_highlights_sessions() {
    for pathname in ["/", "/s/abc", "/t/fp123/src", "/w/ws1", "/w/ws1/t/ch1"] {
        assert_eq!(
            active_destination(pathname),
            Some(Destination::Sessions),
            "{pathname} is a session destination"
        );
    }
}

#[test]
fn a_settings_pane_still_highlights_settings() {
    assert_eq!(
        active_destination("/settings/machines"),
        Some(Destination::Settings)
    );
}

#[test]
fn a_path_that_matches_no_destination_highlights_nothing() {
    // v2's rail greys out every item on an unmatched path. Defaulting to the
    // first item would claim a reader is looking at sessions when they are not.
    assert_eq!(active_destination("/pair"), None);
    assert_eq!(active_destination("/design"), None);
}

#[test]
fn files_owns_both_the_file_route_and_the_browser() {
    assert_eq!(
        active_destination("/file/fp/a.txt"),
        Some(Destination::Files)
    );
    assert_eq!(active_destination("/browse"), Some(Destination::Files));
    assert_eq!(active_destination("/browse/fp1"), Some(Destination::Files));
}

#[test]
fn a_destinations_href_round_trips_through_the_route_grammar() {
    for destination in Destination::ALL {
        let route = roost_web::routes::Route::parse(&destination.href());
        assert_eq!(
            route,
            roost_web::routes::Route::parse(&route.to_path()),
            "{destination:?} does not round trip"
        );
    }
}

#[test]
fn a_session_title_beats_every_fallback() {
    assert_eq!(
        workbench_title("/s/abc", Some("  build the thing  "), Some("/home/ada/src")),
        "build the thing"
    );
}

#[test]
fn an_empty_session_title_falls_back_to_the_folder_basename() {
    assert_eq!(
        workbench_title("/t/fp/src", Some("   "), Some("/home/ada/src/")),
        "src"
    );
}

#[test]
fn a_session_with_no_title_and_no_folder_reads_as_the_home_marker() {
    assert_eq!(workbench_title("/s/abc", None, Some("")), "~");
}

#[test]
fn a_terminal_route_that_resolved_to_no_session_reads_as_terminal() {
    assert_eq!(workbench_title("/s/gone", None, None), "Terminal");
}

#[test]
fn a_fixed_surface_reads_as_itself_regardless_of_sessions() {
    // A reader with a live terminal open who navigates to Settings must not
    // see the terminal's title in the title bar.
    for (pathname, expected) in [
        ("/search", "Search"),
        ("/search/deep", "Search"),
        ("/file/fp/a.txt", "Files"),
        ("/settings", "Settings"),
        ("/help", "Help"),
        ("/", "Roost"),
    ] {
        assert_eq!(
            workbench_title(pathname, Some("a live terminal"), Some("/src")),
            expected,
            "{pathname}"
        );
    }
}

#[test]
fn a_long_session_title_is_cut_on_a_character_not_a_byte() {
    // Cutting on bytes would split a multi-byte glyph and render a replacement
    // character in the title bar.
    let title = "é".repeat(TITLE_MAX_CHARS + 10);
    let shown = workbench_title("/s/abc", Some(&title), None);
    assert_eq!(shown.chars().count(), TITLE_MAX_CHARS);
}

#[test]
fn a_stale_success_while_the_tab_is_visible_reads_as_unreachable() {
    let state = coordinator_state(
        true,
        CoordinatorHealth {
            offline: false,
            last_attempt_failed: false,
            last_success_ms: Some(1_000),
            page_visible: true,
            now_ms: 1_000 + COORD_STALE_MS + 1,
        },
    );
    assert_eq!(state, CoordinatorState::Unreachable);
    assert_eq!(state.status(), "offline");
}

#[test]
fn a_hidden_tab_is_not_judged_stale() {
    // A tab the reader cannot see failing is not a fact they can act on, and
    // a red status bar in a background tab is an alarm with no referent.
    let state = coordinator_state(
        true,
        CoordinatorHealth {
            offline: false,
            last_attempt_failed: false,
            last_success_ms: Some(1_000),
            page_visible: false,
            now_ms: 1_000 + COORD_STALE_MS * 100,
        },
    );
    assert_eq!(state, CoordinatorState::Synced);
}

#[test]
fn no_success_yet_reads_as_syncing_even_with_an_identity() {
    let state = coordinator_state(
        true,
        CoordinatorHealth {
            offline: false,
            last_attempt_failed: false,
            last_success_ms: None,
            page_visible: true,
            now_ms: 5_000,
        },
    );
    assert_eq!(state, CoordinatorState::Syncing);
}

#[test]
fn an_identity_without_a_health_snapshot_reads_as_syncing() {
    let state = coordinator_state(
        false,
        CoordinatorHealth {
            offline: false,
            last_attempt_failed: false,
            last_success_ms: None,
            page_visible: true,
            now_ms: 5_000,
        },
    );
    assert_eq!(state, CoordinatorState::Syncing);
}

#[test]
fn a_failed_attempt_reads_as_unreachable_however_old_the_last_success() {
    let state = coordinator_state(
        true,
        CoordinatorHealth {
            offline: false,
            last_attempt_failed: true,
            last_success_ms: Some(9_999),
            page_visible: true,
            now_ms: 10_000,
        },
    );
    assert_eq!(state, CoordinatorState::Unreachable);
}

#[test]
fn a_live_link_that_just_spoke_reads_as_synced() {
    // The whole status item hangs off this: an open link, a recent frame and a
    // known identity is the one combination that may print `Synced`.
    let health = coordinator_health_from_link(Some(120), 10_000, false, true);
    assert_eq!(health.last_success_ms, Some(9_880));
    assert!(!health.last_attempt_failed);
    assert_eq!(coordinator_state(true, health), CoordinatorState::Synced);
}

#[test]
fn a_closed_link_reads_as_unreachable_however_recent_the_last_frame_was() {
    // `idle_ms` is `Some` exactly while a socket is open, so its absence IS the
    // outage — and there is no last frame left to soften it.
    let health = coordinator_health_from_link(None, 10_000, false, true);
    assert!(health.last_attempt_failed);
    assert_eq!(health.last_success_ms, None);
    assert_eq!(
        coordinator_state(true, health),
        CoordinatorState::Unreachable
    );
}

#[test]
fn an_open_link_with_no_identity_yet_reads_as_syncing() {
    let health = coordinator_health_from_link(Some(0), 10_000, false, true);
    assert_eq!(coordinator_state(false, health), CoordinatorState::Syncing);
}

#[test]
fn a_browser_with_no_route_names_itself_not_the_coordinator() {
    // The link may be perfectly open while the machine has no network, and
    // telling an operator to restart the coordinator would send them at the one
    // thing that is working.
    let health = coordinator_health_from_link(Some(0), 10_000, true, true);
    assert_eq!(coordinator_state(true, health), CoordinatorState::Offline);
    assert_eq!(CoordinatorState::Offline.label(), "Offline");
}

#[test]
fn a_silent_link_stale_enough_reads_as_unreachable_while_the_tab_is_watching() {
    // `now_ms` must be LATER than the idle it claims: `last_success_ms` is
    // `now - idle`, so an idle of 10 001 ms measured at 10 000 puts the last
    // success before the clock starts and the saturating subtraction clamps the
    // difference to 0 — which reads as perfectly fresh rather than stale.
    let health =
        coordinator_health_from_link(Some(COORD_STALE_MS as u64 + 1), 100_000, false, true);
    assert_eq!(
        coordinator_state(true, health),
        CoordinatorState::Unreachable
    );
}

#[test]
fn the_same_silent_link_is_believed_while_the_tab_is_hidden() {
    let health =
        coordinator_health_from_link(Some(COORD_STALE_MS as u64 * 100), 10_000, false, false);
    assert_eq!(coordinator_state(true, health), CoordinatorState::Synced);
}

#[test]
fn a_session_context_joins_the_folder_only_when_it_adds_something() {
    assert_eq!(session_context("build", Some("/src")), "build · /src");
    assert_eq!(session_context("build", Some("build")), "build");
    assert_eq!(session_context("build", Some("")), "build");
    assert_eq!(session_context("build", None), "build");
}
