//! How a phone swipe ends: a release short of the commit line springs back at
//! any release speed, a cancelled drag never commits, every settle has a
//! bounded drop deadline, and a landed slide is dropped once its route
//! follows. Pins `release_swipe`, `cancel_swipe`, `route_outlives_swipe` and
//! `SwipeRelease::drop_deadline_ms`.

use roost_web::components::deck::deck_swipe::{
    LANDING_GRACE_MS, SETTLE_SLACK_MS, SettleTarget, Swipe, SwipeCompletion, SwipeMode, SwipePhase,
    arm_swipe, cancel_swipe, release_swipe, route_outlives_swipe, track_swipe,
};
use roost_web::components::deck::deck_swipe_style::swipe_style_for;

const WIDTH: f64 = 400.0;

fn tabs() -> [String; 2] {
    ["a".to_owned(), "b".to_owned()]
}

/// A drag armed on `active` toward the next tab and tracked to `offset`.
fn dragged(active: &str, offset: f64) -> Swipe {
    let armed = arm_swipe(-10.0, &tabs(), Some(active), None).expect("armed on a tab");
    track_swipe(&armed, offset, WIDTH)
}

/// The over-swipe past the last tab, released before the commit line with no
/// flick, sprang back in the decision but froze on screen; the decision must
/// spring back at every speed that is not a forward flick.
#[test]
fn a_short_pull_past_the_last_tab_springs_back_at_any_release_speed() {
    let pull = dragged("b", -100.0);
    assert_eq!(pull.mode, SwipeMode::NewTerminal);
    for velocity in [0.0, -0.3, 0.4] {
        let release = release_swipe(&pull, -100.0, velocity, WIDTH).expect("tracking");
        assert_eq!(release.completion, SwipeCompletion::Cancelled, "{velocity}");
        assert_eq!(release.settling.settle_target, Some(SettleTarget::Cancel));
        assert_eq!(release.settling.offset, 0.0);
        assert_eq!(release.delay_ms, 125 + SETTLE_SLACK_MS);
        assert_eq!(release.drop_deadline_ms(), release.delay_ms);
        assert_eq!(
            swipe_style_for(Some(&release.settling), "b", WIDTH).get("transform"),
            Some("translateX(0px) scale(1)"),
            "the card settles back to full size"
        );
    }
    let untravelled = release_swipe(&dragged("b", 0.0), 0.0, 0.0, WIDTH).expect("tracking");
    assert_eq!(untravelled.completion, SwipeCompletion::Cancelled);
    assert_eq!(untravelled.delay_ms, SETTLE_SLACK_MS);
    let far = release_swipe(&dragged("b", -170.0), -170.0, 0.0, WIDTH).expect("tracking");
    assert_eq!(
        far.completion,
        SwipeCompletion::NewTerminal,
        "past the line"
    );
}

#[test]
fn a_cancelled_drag_springs_back_however_far_it_travelled() {
    for (active, mode) in [("a", SwipeMode::Slide), ("b", SwipeMode::NewTerminal)] {
        let far = dragged(active, -300.0);
        assert_eq!(far.mode, mode);
        let cancelled = cancel_swipe(&far, WIDTH).expect("tracking");
        assert_eq!(cancelled.completion, SwipeCompletion::Cancelled);
        assert_eq!(cancelled.settling.settle_target, Some(SettleTarget::Cancel));
        assert_eq!(
            (cancelled.settling.offset, cancelled.settling.settle_ms),
            (0.0, Some(375))
        );
        assert!(
            cancel_swipe(&cancelled.settling, WIDTH).is_none(),
            "a settle is not cancelled twice"
        );
    }
}

/// A landed slide stays painted until the route shows the neighbour, and is
/// dropped after the landing grace if the route never does.
#[test]
fn every_settle_has_a_bounded_drop_deadline() {
    let slide = release_swipe(&dragged("a", -300.0), -300.0, 0.0, WIDTH).expect("tracking");
    assert_eq!(
        slide.completion,
        SwipeCompletion::SelectNeighbor("b".to_owned())
    );
    assert!(slide.completion.waits_for_route());
    assert_eq!(slide.drop_deadline_ms(), slide.delay_ms + LANDING_GRACE_MS);
    let bloom = release_swipe(&dragged("b", -300.0), -300.0, 0.0, WIDTH).expect("tracking");
    assert!(!bloom.completion.waits_for_route());
    assert_eq!(bloom.drop_deadline_ms(), bloom.delay_ms);
}

#[test]
fn the_route_outlives_a_landed_slide_or_a_drag_it_moved_away_from() {
    let drag = dragged("a", -150.0);
    assert!(!route_outlives_swipe(&drag, "a"));
    assert!(
        route_outlives_swipe(&drag, "elsewhere"),
        "the route moved mid-drag"
    );
    let landing = release_swipe(&drag, -300.0, 0.0, WIDTH)
        .expect("tracking")
        .settling;
    assert_eq!(landing.phase, SwipePhase::Settle);
    assert!(
        !route_outlives_swipe(&landing, "a"),
        "the select has not reached the route yet"
    );
    assert!(
        route_outlives_swipe(&landing, "b"),
        "the route shows the neighbour"
    );
    let springing = cancel_swipe(&drag, WIDTH).expect("tracking").settling;
    assert!(
        !route_outlives_swipe(&springing, "b"),
        "a spring-back ends on its own timer"
    );
}
