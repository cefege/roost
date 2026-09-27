//! The acknowledged layout apply: the fences, the settle-once rule, and what
//! happens to a reservation nobody answers.
//!
//! The rule this file exists for is settle-exactly-once. An apply that settles
//! twice is how a layout tears: the caller is told the tab applied a document
//! the tab has already replaced, and a browser that answers a replayed frame
//! would be believed twice. Every other test here exists so that a "settle if
//! the correlation matches" port -- the shape a tidier rewrite produces -- fails.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod ui_state_fixture;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use roost_coord::ui_state::layout_apply::{
    LayoutApplyRequest, UI_LAYOUT_APPLY_TIMEOUT_MS, UI_LAYOUT_TARGET_GONE_REASON,
    UiLayoutApplyOwner, UiLayoutApplyPublication, UiLayoutApplyResolution, UiLayoutApplyTarget,
};
use roost_coord::ui_state::rejected_reason::{
    UI_LAYOUT_REJECTED_REASON_MAX_LENGTH, sanitized_rejected_reason,
};
use roost_proto::{UiApplyLayoutOutcome, UiApplyLayoutResult};

fn target(fingerprint: &str, tab_id: &str, socket_id: &str) -> UiLayoutApplyTarget {
    UiLayoutApplyTarget {
        fingerprint: fingerprint.to_owned(),
        tab_id: tab_id.to_owned(),
        socket_id: socket_id.to_owned(),
    }
}

fn applied(correlation_id: &str) -> UiApplyLayoutResult {
    UiApplyLayoutResult {
        correlation_id: correlation_id.to_owned(),
        outcome: UiApplyLayoutOutcome::Applied.into(),
        ..Default::default()
    }
}

fn rejected(correlation_id: &str, reason: &str) -> UiApplyLayoutResult {
    UiApplyLayoutResult {
        correlation_id: correlation_id.to_owned(),
        outcome: UiApplyLayoutOutcome::Rejected.into(),
        reason: Some(reason.to_owned()),
        ..Default::default()
    }
}

/// A published apply, and the reservation awaiting it.
struct Reserved {
    publication: UiLayoutApplyPublication,
    pending: roost_coord::ui_state::layout_apply::PendingLayoutApply,
}

fn reserve(
    owner: &UiLayoutApplyOwner,
    target: &UiLayoutApplyTarget,
) -> Result<Reserved, roost_coord::ui_state::layout_apply::UiLayoutApplyCapacityError> {
    let mut published: Option<UiLayoutApplyPublication> = None;
    let request = owner.request_apply(&target.fingerprint, &target.tab_id, |publication| {
        published = Some(publication.clone());
    })?;
    let LayoutApplyRequest::Pending(pending) = request else {
        return Err(roost_coord::ui_state::layout_apply::UiLayoutApplyCapacityError);
    };
    let publication = published.expect("a reserved apply is published");
    Ok(Reserved {
        publication,
        pending,
    })
}

fn owner_with_clock(now_ms: Arc<Mutex<i64>>) -> UiLayoutApplyOwner {
    let clock = Arc::new(move || *now_ms.lock().expect("the test clock"));
    UiLayoutApplyOwner::with_clock(clock)
}

#[tokio::test]
async fn a_result_settles_its_reservation_once_and_a_duplicate_settles_nothing() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let base = target("fingerprint-a", "tab-a", "socket-a");
    let _guard = owner
        .register_target(base.clone())
        .expect("a target registers");

    let reserved = reserve(&owner, &base).expect("the reservation is admitted");
    let correlation_id = reserved.publication.correlation_id.clone();
    let answer = applied(&correlation_id);

    assert!(
        owner.accept_result(&base, &answer),
        "the first answer settles the reservation"
    );
    assert!(
        !owner.accept_result(&base, &answer),
        "the SAME answer a second time settles nothing"
    );
    assert_eq!(owner.stats().pending, 0, "one reservation, settled once");

    let resolution = reserved.pending.await_resolution().await;
    assert_eq!(resolution.outcome, UiApplyLayoutOutcome::Applied);
    assert_eq!(resolution.correlation_id, correlation_id);
    assert_eq!(resolution.reason, None);
}

#[tokio::test]
async fn a_result_from_any_other_fence_is_refused_and_the_reservation_survives() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let base = target("fingerprint-a", "tab-a", "socket-a");
    let _guard = owner
        .register_target(base.clone())
        .expect("a target registers");
    let reserved = reserve(&owner, &base).expect("the reservation is admitted");
    let correlation_id = reserved.publication.correlation_id.clone();

    for wrong in [
        target("fingerprint-b", "tab-a", "socket-a"),
        target("fingerprint-a", "tab-b", "socket-a"),
        target("fingerprint-a", "tab-a", "socket-b"),
    ] {
        assert!(
            !owner.accept_result(&wrong, &applied(&correlation_id)),
            "a result from {}:{}:{} must not settle another target's reservation",
            wrong.fingerprint,
            wrong.tab_id,
            wrong.socket_id
        );
    }
    assert!(
        !owner.accept_result(&base, &applied("a-correlation-this-owner-never-issued")),
        "an unknown correlation settles nothing"
    );
    for unprovable in [
        UiApplyLayoutOutcome::Unspecified,
        UiApplyLayoutOutcome::TargetGone,
    ] {
        let mut answer = applied(&correlation_id);
        answer.outcome = unprovable.into();
        assert!(
            !owner.accept_result(&base, &answer),
            "{unprovable:?} is not an outcome a target may prove"
        );
    }
    assert_eq!(
        owner.stats().pending,
        1,
        "the reservation is still awaiting"
    );

    assert!(owner.accept_result(&base, &applied(&correlation_id)));
    assert_eq!(
        reserved.pending.await_resolution().await.outcome,
        UiApplyLayoutOutcome::Applied
    );
}

#[tokio::test]
async fn a_reservation_nobody_answers_is_dropped_when_its_deadline_passes() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let base = target("fingerprint-a", "tab-a", "socket-a");
    let _guard = owner
        .register_target(base.clone())
        .expect("a target registers");
    let reserved = reserve(&owner, &base).expect("the reservation is admitted");

    *now.lock().expect("the test clock") += UI_LAYOUT_APPLY_TIMEOUT_MS - 1;
    assert_eq!(
        owner.stats().pending,
        1,
        "a reservation one millisecond short of its deadline is still awaiting"
    );

    // The next call into the owner is what reaps, exactly as a handler's own
    // await would: the drop has to be observable, not merely eventual.
    *now.lock().expect("the test clock") += 1;
    assert!(
        !owner.accept_result(&base, &applied(&reserved.publication.correlation_id)),
        "a result that arrives after the deadline settles nothing"
    );
    assert_eq!(owner.stats().pending, 0, "the dropped reservation is gone");
    assert_eq!(owner.stats().targets, 1, "the target itself is untouched");
}

#[tokio::test]
async fn the_caller_of_an_unanswered_apply_is_told_the_target_is_gone() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let base = target("fingerprint-a", "tab-a", "socket-a");
    let _guard = owner
        .register_target(base.clone())
        .expect("a target registers");
    let reserved = reserve(&owner, &base).expect("the reservation is admitted");

    *now.lock().expect("the test clock") += UI_LAYOUT_APPLY_TIMEOUT_MS;
    let resolution = reserved.pending.await_resolution().await;
    assert_eq!(resolution.outcome, UiApplyLayoutOutcome::TargetGone);
    assert_eq!(
        resolution.reason.as_deref(),
        Some(UI_LAYOUT_TARGET_GONE_REASON)
    );
    assert_eq!(owner.stats().pending, 0);
}

#[tokio::test]
async fn an_apply_for_a_tab_with_no_live_socket_resolves_without_publishing() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let publications = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&publications);

    let request = owner
        .request_apply("fingerprint-a", "tab-a", move |_publication| {
            counted.fetch_add(1, Ordering::SeqCst);
        })
        .expect("a missing target is not a capacity failure");

    let LayoutApplyRequest::TargetGone(resolution) = request else {
        panic!("a tab with no registered socket must resolve immediately");
    };
    assert_eq!(resolution.outcome, UiApplyLayoutOutcome::TargetGone);
    assert_eq!(
        publications.load(Ordering::SeqCst),
        0,
        "nothing is published when there is no socket to publish to"
    );
}

#[tokio::test]
async fn a_rejected_answer_carries_a_bounded_control_free_reason() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let base = target("fingerprint-a", "tab-a", "socket-a");
    let _guard = owner
        .register_target(base.clone())
        .expect("a target registers");
    let raw = format!(" rejected\n\u{0}{}", "x".repeat(400));
    let reserved = reserve(&owner, &base).expect("the reservation is admitted");

    assert!(owner.accept_result(&base, &rejected(&reserved.publication.correlation_id, &raw)));
    let resolution: UiLayoutApplyResolution = reserved.pending.await_resolution().await;
    assert_eq!(resolution.outcome, UiApplyLayoutOutcome::Rejected);
    let reason = resolution.reason.expect("a rejection carries a reason");
    assert_eq!(reason.chars().count(), UI_LAYOUT_REJECTED_REASON_MAX_LENGTH);
    assert!(
        !reason.chars().any(char::is_control),
        "a control character reached the caller"
    );

    let empty = reserve(&owner, &base).expect("a second reservation is admitted");
    assert!(owner.accept_result(
        &base,
        &rejected(&empty.publication.correlation_id, "\u{0}\n")
    ));
    assert_eq!(
        empty.pending.await_resolution().await.reason.as_deref(),
        Some("layout apply rejected"),
        "a reason with nothing readable in it falls back rather than reporting empty"
    );
}

#[tokio::test]
async fn a_replaced_socket_settles_the_old_reservation_and_keeps_the_new_one() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let base = target("fingerprint-a", "tab-a", "socket-a");
    let old_guard = owner
        .register_target(base.clone())
        .expect("a target registers");
    let old_reservation = reserve(&owner, &base).expect("the reservation is admitted");

    let replacement = target("fingerprint-a", "tab-a", "socket-new");
    let new_guard = owner
        .register_target(replacement.clone())
        .expect("a redialled socket replaces the old one");
    assert_eq!(
        old_reservation.pending.await_resolution().await.outcome,
        UiApplyLayoutOutcome::TargetGone,
        "the socket that can no longer answer must not hold the caller open"
    );

    let new_reservation = reserve(&owner, &replacement).expect("the new socket is reservable");
    drop(old_guard);
    assert_eq!(
        owner.stats().targets,
        1,
        "a disposer for a replaced target must not remove its replacement"
    );
    assert!(owner.accept_result(
        &replacement,
        &applied(&new_reservation.publication.correlation_id)
    ));
    assert_eq!(
        new_reservation.pending.await_resolution().await.outcome,
        UiApplyLayoutOutcome::Applied
    );

    drop(new_guard);
    assert_eq!(owner.stats().targets, 0);
}

#[tokio::test]
async fn the_same_tab_id_on_another_device_cannot_receive_or_answer_an_apply() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_clock(Arc::clone(&now));
    let victim = target("fingerprint-a", "tab-a", "socket-a");
    let attacker = target("fingerprint-b", "tab-a", "socket-b");
    let _victim_guard = owner
        .register_target(victim.clone())
        .expect("the victim registers");
    let _attacker_guard = owner
        .register_target(attacker.clone())
        .expect("the attacker registers");
    let reserved = reserve(&owner, &victim).expect("the victim is reservable");

    assert_eq!(
        reserved.publication.target.socket_id, "socket-a",
        "the reservation is pinned to the requested device's socket"
    );
    assert!(
        !owner.accept_result(&attacker, &applied(&reserved.publication.correlation_id)),
        "a colliding tab id on another device cannot answer"
    );
    assert!(owner.accept_result(&victim, &applied(&reserved.publication.correlation_id)));
}

#[test]
fn a_reason_that_is_only_format_characters_is_reduced_to_the_fallback() {
    assert_eq!(
        sanitized_rejected_reason(Some("\u{200b}\u{202e}\u{feff}")),
        "layout apply rejected"
    );
    assert_eq!(
        sanitized_rejected_reason(None),
        "layout apply rejected",
        "a target that sends no reason at all is not an empty reason"
    );
    assert_eq!(
        sanitized_rejected_reason(Some("a\u{200b}b")),
        "a b",
        "a dropped format character becomes a word break, not a join"
    );
}
