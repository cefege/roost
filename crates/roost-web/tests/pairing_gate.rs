//! Pins which half of the pairing surface draws, and at which paths. The
//! surface is mounted once, above the access gate, and it decides for itself
//! what to draw, so the decision is one pure rule that has to agree with the
//! router's own URL grammar. Symptoms this pins: an authorized reader opens
//! `/pair` and gets an empty page; the onboarding page paints over the
//! checking screen.
//! Ports the `/pair` branch of `apps/web/src/App.tsx:149-151` and the branch
//! in `Onboarding.tsx:109-154`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::store::BrowserAccessState;
use roost_web::components::pairing::draws_pairing_page;

#[test]
fn an_unpaired_reader_gets_the_requester_at_every_path() {
    for path in ["/", "/settings/devices", "/search", "/pair", "/pair?tv=1"] {
        assert!(
            draws_pairing_page(BrowserAccessState::Unauthorized, path),
            "the unauthorized branch must draw at {path}"
        );
    }
}

#[test]
fn an_authorized_reader_gets_the_ceremony_on_the_pair_route_however_it_is_spelled() {
    // The router renders `pathname` plus the query, so the bare spelling is
    // only one of the ways the ceremony route arrives.
    for path in ["/pair", "/pair/", "/pair?tv=1", "/pair#section"] {
        assert!(
            draws_pairing_page(BrowserAccessState::Authorized, path),
            "the ceremony must draw at {path}"
        );
    }
}

#[test]
fn an_authorized_reader_elsewhere_draws_no_page() {
    for path in ["/", "/settings/devices", "/browse", "/pairing", "/pai"] {
        assert!(
            !draws_pairing_page(BrowserAccessState::Authorized, path),
            "nothing but the code dialog may draw at {path}"
        );
    }
}

#[test]
fn a_browser_still_checking_draws_no_page_anywhere() {
    for path in ["/", "/pair", "/settings/devices"] {
        assert!(
            !draws_pairing_page(BrowserAccessState::Checking, path),
            "the checking screen owns {path}"
        );
    }
}
