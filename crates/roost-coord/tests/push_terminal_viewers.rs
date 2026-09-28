//! Which devices the push dispatch treats as already watching a session: the
//! coordinator's `TerminalViewHub` answering `ActiveTerminalViewers`
//! (`push/viewers.rs`).
//!
//! Ports `activeTerminalViewerFingerprints` in
//! `apps/coord/src/terminal/view/terminal-view-hub.ts:113-117`, which has no v2
//! unit test of its own (v2 exercises it only through "suppresses only devices
//! actively viewing the session", ported in `push_transition_delivery.rs`). The
//! precedence is the case worth pinning: v2 reads `owner ?? hub`, so an owner
//! row answers ALONE, even an empty one, and every production session is
//! owner-mode.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_view_support;

use std::collections::BTreeSet;

use roost_coord::push::dispatch::ActiveTerminalViewers;
use roost_proto::WTerminalViewProjection;
use terminal_view_support::{
    FINGERPRINT, Harness, OTHER_FINGERPRINT, OTHER_VIEW, SESSION, VIEW, ViewShape,
};

/// What the push dispatch reads for the harness's session.
fn watching(harness: &Harness) -> BTreeSet<String> {
    ActiveTerminalViewers::active_viewer_fingerprints(harness.hub.as_ref(), &harness.session)
}

/// The owner's published membership: `viewers` as `(fingerprint, parked)`.
fn projection(viewers: &[(&str, bool)]) -> WTerminalViewProjection {
    WTerminalViewProjection {
        session_id: SESSION.to_owned(),
        viewers: viewers
            .iter()
            .map(|(fingerprint, parked)| roost_proto::PbTerminalViewInput {
                fingerprint: (*fingerprint).to_owned(),
                view_id: OTHER_VIEW.to_owned(),
                cols: 90,
                rows: 30,
                parked: *parked,
                constrains: !*parked,
                __buffa_unknown_fields: Default::default(),
            })
            .collect(),
        effective_cols: 90,
        effective_rows: 30,
        stream_id: "owner-stream".to_owned(),
        __buffa_unknown_fields: Default::default(),
    }
}

#[test]
fn a_session_nobody_views_suppresses_nobody() {
    // The opposite reading would silently suppress every push in the fleet.
    assert!(watching(&Harness::unowned()).is_empty());
}

#[test]
fn a_device_viewing_through_the_coordinator_registry_is_watching() {
    let harness = Harness::unowned();
    let browser = harness.browser("socket-1", FINGERPRINT, &[SESSION]);
    harness.view(&browser, ViewShape::new(VIEW, 80, 24, 1), 0);
    assert_eq!(watching(&harness), BTreeSet::from([FINGERPRINT.to_owned()]));
}

#[test]
fn an_owner_row_answers_alone_parked_viewers_included_even_when_empty() {
    // Start from a device the coordinator's own registry holds, so a fallback
    // or a union would both be visible.
    let harness = Harness::unowned();
    let browser = harness.browser("socket-1", FINGERPRINT, &[SESSION]);
    harness.view(&browser, ViewShape::new(VIEW, 80, 24, 1), 0);
    harness.hub.register_owner(&harness.worker);

    // A parked viewer has the terminal open in a background tab: it has seen
    // the session, and v2 counts every viewer the owner admitted.
    harness
        .hub
        .apply_owner_projection(&harness.worker, &projection(&[(OTHER_FINGERPRINT, true)]));
    assert_eq!(
        watching(&harness),
        BTreeSet::from([OTHER_FINGERPRINT.to_owned()]),
        "the owner row answers, and the coordinator registry is not consulted"
    );

    harness
        .hub
        .apply_owner_projection(&harness.worker, &projection(&[]));
    assert!(
        watching(&harness).is_empty(),
        "an empty owner row still answers: nobody is watching"
    );
}
