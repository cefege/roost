//! The URL grammar's own rules, exercised through the public `Route` surface.
//!
//! The grammar is the one authority for what a path means, so every rule it
//! encodes — the optional segment, the catch-all folder, the legacy workspace
//! form, and the refusal to fall through to home — is pinned here rather than
//! at a call site, where a change to the grammar would show up as a component
//! test that happened to stop passing.
//!
//! Mirrors `crates/roost-web/src/routes.rs`.

use roost_web::Route;
use roost_web::routes::agent_href;

#[test]
fn the_root_is_the_workbench_and_a_trailing_slash_is_the_same_page() {
    assert_eq!(Route::parse("/"), Route::Home);
    assert_eq!(Route::parse(""), Route::Home);
    assert_eq!(Route::parse("/?tab=1"), Route::Home);
}

#[test]
fn a_session_path_carries_the_session_and_nothing_else() {
    assert_eq!(
        Route::parse("/s/abc123"),
        Route::Session {
            session_id: "abc123".to_string()
        }
    );
    // A second segment is not a session id; it is a different URL, and
    // answering it with the first session's page is how a mistyped link
    // opens the wrong terminal.
    assert!(matches!(
        Route::parse("/s/abc123/extra"),
        Route::Unknown { .. }
    ));
}

#[test]
fn a_terminal_path_keeps_the_whole_folder_below_the_worker() {
    // The folder is a catch-all: a path segment is legal in a folder name and
    // truncating at the first one sends the terminal somewhere else.
    assert_eq!(
        Route::parse("/t/aa11/src/deep/nested/folder"),
        Route::Terminal {
            worker_fp: "aa11".to_string(),
            folder_path: "src/deep/nested/folder".to_string()
        }
    );
}

#[test]
fn a_legacy_workspace_url_resolves_to_the_channel_it_names() {
    // v2 kept `/w/:workspaceId/t/:channelId` so a bookmark minted before
    // the session route still opens the terminal it was pointing at. The
    // bare `/w/:workspaceId` form has no channel, and must NOT borrow the
    // next segment as one.
    assert_eq!(
        Route::parse("/w/ws1/t/ch9"),
        Route::Workspace {
            workspace_id: "ws1".to_string(),
            channel_id: Some("ch9".to_string())
        }
    );
    assert_eq!(
        Route::parse("/w/ws1"),
        Route::Workspace {
            workspace_id: "ws1".to_string(),
            channel_id: None
        }
    );
    // `/w/ws1/t` and `/w/ws1/x/ch9` name no channel; treating either as a
    // workspace link would open whatever session happened to be newest.
    assert!(matches!(Route::parse("/w/ws1/t"), Route::Unknown { .. }));
    assert!(matches!(
        Route::parse("/w/ws1/x/ch9"),
        Route::Unknown { .. }
    ));
    assert!(matches!(Route::parse("/w"), Route::Unknown { .. }));
}

#[test]
fn a_settings_pane_is_optional_and_nothing_more() {
    assert_eq!(Route::parse("/settings"), Route::Settings { pane: None });
    assert_eq!(
        Route::parse("/settings/machines"),
        Route::Settings {
            pane: Some("machines".to_string())
        }
    );
    assert!(matches!(
        Route::parse("/settings/a/b"),
        Route::Unknown { .. }
    ));
}

#[test]
fn browse_is_optional_too_and_never_guesses_a_worker() {
    assert_eq!(Route::parse("/browse"), Route::Browse { worker_fp: None });
    assert_eq!(
        Route::parse("/browse/aa11"),
        Route::Browse {
            worker_fp: Some("aa11".to_string())
        }
    );
}

#[test]
fn agent_route_round_trips_and_rejects_extra_segments() {
    assert_eq!(
        Route::parse("/a/abc"),
        Route::Agent {
            conversation_id: "abc".to_string()
        }
    );
    assert_eq!(
        Route::parse("/a/abc/x"),
        Route::Unknown {
            path: "/a/abc/x".to_string()
        }
    );
    assert_eq!(agent_href("abc"), "/a/abc");
    assert_eq!(
        Route::parse(&agent_href("abc")),
        Route::Agent {
            conversation_id: "abc".to_string()
        }
    );
}

#[test]
fn an_unknown_path_is_named_rather_than_falling_through_to_home() {
    let parsed = Route::parse("/nope");
    assert_eq!(
        parsed,
        Route::Unknown {
            path: "/nope".to_string()
        }
    );
    // The round trip is what makes a not-found page able to show the URL it
    // could not answer.
    assert_eq!(parsed.to_path(), "/nope");
}

#[test]
fn a_file_path_needs_both_a_worker_and_a_path() {
    assert_eq!(
        Route::parse("/file/aa11/etc/hosts"),
        Route::File {
            worker_fp: "aa11".to_string(),
            path: "etc/hosts".to_string()
        }
    );
    assert!(matches!(Route::parse("/file/aa11"), Route::Unknown { .. }));
}

#[test]
fn every_route_with_captures_round_trips_through_its_own_path() {
    // A link built from a route and a link typed by a reader have to be the
    // same string, or a copied URL stops working.
    let routes = [
        Route::Home,
        Route::Session {
            session_id: "abc".to_string(),
        },
        Route::Terminal {
            worker_fp: "aa11".to_string(),
            folder_path: "src/deep".to_string(),
        },
        Route::Workspace {
            workspace_id: "ws1".to_string(),
            channel_id: None,
        },
        Route::Workspace {
            workspace_id: "ws1".to_string(),
            channel_id: Some("ch9".to_string()),
        },
        Route::Settings { pane: None },
        Route::Settings {
            pane: Some("machines".to_string()),
        },
        Route::Pair,
        Route::Help,
        Route::Design,
        Route::File {
            worker_fp: "aa11".to_string(),
            path: "etc/hosts".to_string(),
        },
        Route::Browse { worker_fp: None },
        Route::Browse {
            worker_fp: Some("aa11".to_string()),
        },
        Route::Search,
    ];
    for route in routes {
        assert_eq!(Route::parse(&route.to_path()), route, "{route:?}");
    }
}
