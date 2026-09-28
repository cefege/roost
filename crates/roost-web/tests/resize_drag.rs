//! Resize-drag ownership and the pointer resize lifecycle. Ports
//! `apps/web/tests/resizeDrag.test.ts` against
//! `roost_web::motion::resize_drag::{ResizeDragOwners, PointerResizeSession}` —
//! the token and settle rules the wasm32 listener adapter only executes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::motion::resize_drag::{
    MAX_RESIZE_DRAG_OWNERS, PointerResizeSession, ResizeDragOwners, ResizeSettlement,
};

#[test]
fn overlapping_owners_suppress_until_the_final_release() {
    let mut owners = ResizeDragOwners::default();
    let first = owners.begin();
    let second = owners.begin();
    assert!(owners.is_dragging());
    owners.release(first);
    assert!(owners.is_dragging());
    owners.release(second);
    assert!(!owners.is_dragging());
    let next = owners.begin();
    assert!(owners.is_dragging());
    owners.release(next);
    assert!(!owners.is_dragging());
}

#[test]
fn a_reset_retires_stale_owners_without_letting_them_release_a_new_one() {
    let mut owners = ResizeDragOwners::default();
    let stale_first = owners.begin();
    let stale_second = owners.begin();
    owners.reset();
    assert!(!owners.is_dragging());
    let current = owners.begin();
    owners.release(stale_first);
    owners.release(stale_second);
    assert!(owners.is_dragging());
    owners.release(current);
    assert!(!owners.is_dragging());
}

#[test]
fn the_owner_past_the_cap_retires_the_generation_and_prior_releases_are_stale() {
    let mut owners = ResizeDragOwners::default();
    let stale: Vec<_> = (0..MAX_RESIZE_DRAG_OWNERS)
        .map(|_| owners.begin())
        .collect();
    let current = owners.begin();
    for token in stale {
        owners.release(token);
    }
    assert!(owners.is_dragging());
    owners.release(current);
    assert!(!owners.is_dragging());
}

#[test]
fn moves_coalesce_into_one_frame_and_other_pointers_are_ignored() {
    let mut session = PointerResizeSession::new(7, 5);
    assert!(!session.sample(99, 100));
    assert!(session.sample(7, 20));
    assert!(!session.sample(7, 30));
    assert!(!session.ends_on(99));
    assert!(session.ends_on(7));
}

#[test]
fn pointer_up_cancels_the_queued_frame_and_commits_the_latest_sample_once() {
    let mut session = PointerResizeSession::new(7, 5);
    session.sample(7, 20);
    session.sample(7, 30);
    assert_eq!(
        session.finish(true),
        Some(ResizeSettlement {
            cancel_frame: true,
            commit: Some(30)
        })
    );
    assert_eq!(session.finish(true), None);
    assert_eq!(session.finish(false), None);
    assert!(!session.ends_on(7));
}

#[test]
fn a_flushed_frame_applies_the_latest_sample_and_clears_the_queue() {
    let mut session = PointerResizeSession::new(1, 0);
    session.sample(1, 41);
    assert_eq!(session.frame(), Some(41));
    assert_eq!(
        session.finish(true),
        Some(ResizeSettlement {
            cancel_frame: false,
            commit: Some(41)
        })
    );
}

#[test]
fn disposal_mid_drag_aborts_the_queued_geometry_and_stays_idempotent() {
    let mut session = PointerResizeSession::new(7, 5);
    session.sample(7, 64);
    assert_eq!(
        session.finish(false),
        Some(ResizeSettlement {
            cancel_frame: true,
            commit: None
        })
    );
    assert_eq!(session.frame(), None);
    assert!(!session.sample(7, 70));
    assert_eq!(session.finish(true), None);
}

#[test]
fn two_gestures_keep_independent_owners() {
    let mut owners = ResizeDragOwners::default();
    let first = owners.begin();
    let second = owners.begin();
    owners.release(first);
    assert!(owners.is_dragging());
    owners.release(second);
    assert!(!owners.is_dragging());
}
