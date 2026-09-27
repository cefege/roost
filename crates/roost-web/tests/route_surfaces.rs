//! The router's contract with the URL grammar: every path the Playwright specs
//! navigate to, and what each one resolves to. Native, because the decision is
//! `Route::parse` on a string and the document is not part of it.
//!
//! The paths here are COPIED FROM THE SPECS, not from the grammar. A test that
//! navigated with `Route::to_path` would pass whatever the grammar did, which is
//! the one thing this file exists to catch: a grammar that changed shape and a
//! spec that did not.

use roost_web::app::{Surface, surface_for};
use roost_web::routes::Route;

fn served(path: &str) -> bool {
    matches!(surface_for(&Route::parse(path)), Surface::Served(_))
}

fn not_served_path(path: &str) -> Option<String> {
    match surface_for(&Route::parse(path)) {
        Surface::NotServed { path: named } => Some(named),
        _ => None,
    }
}

fn not_found_path(path: &str) -> Option<String> {
    match surface_for(&Route::parse(path)) {
        Surface::NotFound { path: named } => Some(named),
        _ => None,
    }
}

#[test]
fn the_root_is_the_only_served_surface() {
    assert!(served("/"));
    for path in [
        "/s/8f2b1c40-0000-4000-8000-000000000000",
        "/t/a1b2c3d4e5f60718/src",
        "/w/ws-1",
        "/w/ws-1/t/ch-1",
        "/settings",
        "/settings/machines",
        "/pair",
        "/help",
        "/design",
        "/file/a1b2c3d4e5f60718/etc/hosts",
        "/browse",
        "/browse/a1b2c3d4e5f60718",
        "/search",
    ] {
        assert!(!served(path), "{path} must not be served yet");
    }
}

#[test]
fn every_grammar_route_the_specs_navigate_to_is_recognised() {
    // A path the grammar does not know resolves to NotFound, which is a
    // different answer from NotServed and a different bug.
    for path in [
        "/s/8f2b1c40-0000-4000-8000-000000000000",
        "/t/a1b2c3d4e5f60718/src",
        "/w/ws-1",
        "/w/ws-1/t/ch-1",
        "/settings",
        "/settings/machines",
        "/pair",
        "/help",
        "/design",
        "/file/a1b2c3d4e5f60718/etc/hosts",
        "/browse",
        "/browse/a1b2c3d4e5f60718",
        "/search",
        "/search?q=terminal",
    ] {
        assert!(
            not_served_path(path).is_some(),
            "{path} is a grammar route and must resolve to NotServed"
        );
    }
}

#[test]
fn the_not_served_panel_names_the_path_the_spec_navigated_to() {
    // The panel is shown to a reader who followed a bookmark, so it has to carry
    // the URL they followed rather than a canonical form of it.
    assert_eq!(
        not_served_path("/t/a1b2c3d4e5f60718/src").as_deref(),
        Some("/t/a1b2c3d4e5f60718/src")
    );
    assert_eq!(
        not_served_path("/w/ws-1/t/ch-1").as_deref(),
        Some("/w/ws-1/t/ch-1")
    );
    assert_eq!(
        not_served_path("/settings/machines").as_deref(),
        Some("/settings/machines")
    );
}

#[test]
fn a_legacy_workspace_bookmark_keeps_its_channel() {
    // The legacy `/w/:id/t/:channelId` form is one route with an optional
    // channel. A panel that shortened it to `/w/ws-1` would show a reader a
    // path they did not follow.
    let route = Route::parse("/w/ws-1/t/ch-1");
    assert_eq!(route.to_path(), "/w/ws-1/t/ch-1");
}

#[test]
fn a_terminal_folder_path_survives_a_round_trip_through_the_grammar() {
    // `/t/:workerFp/*folderPath` is a splat. A grammar that truncated it to one
    // segment would send a reader to the machine's home rather than the folder
    // they bookmarked.
    let path = "/t/a1b2c3d4e5f60718/home/ada/src/roost";
    assert_eq!(Route::parse(path).to_path(), path);
}

#[test]
fn a_worker_with_no_folder_is_not_a_terminal_route() {
    // The grammar's own rule, and the reason it exists: `/t/:workerFp` with no
    // folder names a MACHINE and nothing to run in it, so it is not the terminal
    // route. v2's pattern is a splat, which can be empty; v3's grammar decided
    // otherwise and `tests/routes.rs` pins that decision. Asserting it here too
    // means a change to either file has to be made twice, on purpose.
    assert_eq!(not_found_path("/t/a1b2c3d4e5f60718").as_deref(), Some("/t/a1b2c3d4e5f60718"));
}

#[test]
fn an_unrecognised_path_is_a_not_found_and_names_itself() {
    assert_eq!(not_found_path("/nope").as_deref(), Some("/nope"));
    assert_eq!(not_found_path("/setttings").as_deref(), Some("/setttings"));
    assert_eq!(not_found_path("/s").as_deref(), Some("/s"));
    assert_eq!(not_found_path("/w/ws-1/t").as_deref(), Some("/w/ws-1/t"));
}

#[test]
fn a_grammar_route_never_also_reads_as_a_not_found() {
    // The two panels are different facts. A path that resolved to both would mean
    // the match was not total, and a reader would get whichever arm ran last.
    for path in ["/", "/s/abc", "/settings", "/browse/fp", "/search"] {
        assert!(not_found_path(path).is_none(), "{path}");
        assert!(not_served_path(path).is_some() || served(path), "{path}");
    }
}

#[test]
fn a_trailing_slash_and_a_query_reach_the_same_surface_as_the_bare_path() {
    // A spec that reloads a deep link with a trailing slash must not land on a
    // different surface than the one it first reached.
    let bare = surface_for(&Route::parse("/settings/machines"));
    assert_eq!(surface_for(&Route::parse("/settings/machines/")), bare);
    assert_eq!(surface_for(&Route::parse("/settings/machines?pane=2")), bare);
}

#[test]
fn a_percent_encoded_segment_survives_the_round_trip_as_a_value() {
    // A folder with a literal `%` in it must not decode to an empty segment and
    // resolve as a different route.
    let route = Route::parse("/t/a1b2c3d4e5f60718/100%25");
    assert!(matches!(route, Route::Terminal { .. }), "{route:?}");
}
