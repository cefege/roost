//! The compact deck bar swipe: the commit decision, the settle timing, the end
//! affordances, the card-dismiss pair, and every style the deck paints from a
//! live swipe. Ports `apps/web/tests/deckSwipe.test.ts`.

use roost_client_core::store::layout::PaneRect;
use roost_web::components::deck::deck_swipe::{
    NEW_BLOOM_MS, SettleTarget, Swipe, SwipeCompletion, SwipeDirection, SwipeMode, SwipePhase,
    arm_swipe, bar_neighbor_id, card_swipe_alpha, end_mode, new_fab_progress, release_swipe,
    settle_duration_ms, should_commit_switch, should_dismiss_card,
};
use roost_web::components::deck::deck_swipe_style::{
    PEEK_RADIUS_PX, PEEK_SCALE_MIN, PeekCard, new_fab_scale, new_fab_style, new_peek_style,
    peek_card, swipe_offsets_px, swipe_style_for,
};
use roost_web::components::deck::inline_style::InlineStyle;

use SwipeDirection::{Next, Previous};

/// A tracking slide half a 400px screen to the left, toward "nxt".
fn track() -> Swipe {
    Swipe {
        phase: SwipePhase::Track,
        current_id: "cur".to_owned(),
        neighbor_id: Some("nxt".to_owned()),
        dir: Next,
        offset: -200.0,
        mode: SwipeMode::Slide,
        settle_target: None,
        settle_ms: None,
    }
}

fn settled(target: SettleTarget) -> Swipe {
    Swipe {
        phase: SwipePhase::Settle,
        settle_target: Some(target),
        ..track()
    }
}

fn pull(offset: f64) -> Swipe {
    Swipe {
        mode: SwipeMode::NewTerminal,
        neighbor_id: None,
        offset,
        ..track()
    }
}

#[test]
fn should_commit_switch_needs_the_armed_direction_and_distance_or_a_flick() {
    assert!(
        should_commit_switch(-170.0, 0.0, Next, 400.0),
        "170 >= 160 (40%)"
    );
    assert!(!should_commit_switch(-159.0, 0.0, Next, 400.0));
    assert!(
        should_commit_switch(-60.0, -0.7, Next, 400.0),
        "60 >= 48 and 0.7 >= 0.6"
    );
    assert!(
        !should_commit_switch(-40.0, -2.0, Next, 400.0),
        "under the travel floor"
    );
    assert!(
        !should_commit_switch(-60.0, 0.7, Next, 400.0),
        "a backward flick"
    );
    assert!(
        !should_commit_switch(200.0, 0.0, Next, 400.0),
        "a reversed release"
    );
    assert!(should_commit_switch(60.0, 0.7, Previous, 400.0));
    assert!(!should_commit_switch(60.0, -0.7, Previous, 400.0));
}

#[test]
fn settle_duration_is_constant_speed_per_screen_width() {
    assert_eq!(settle_duration_ms(400.0, 400.0), 500);
    assert_eq!(settle_duration_ms(200.0, 400.0), 250);
    assert_eq!(settle_duration_ms(100.0, 400.0), 125);
    assert_eq!(settle_duration_ms(0.0, 400.0), 0);
    assert_eq!(settle_duration_ms(100.0, 0.0), 0, "zero-width guard");
}

#[test]
fn an_end_of_list_swipe_becomes_its_affordance() {
    assert_eq!(end_mode(Next, true), SwipeMode::Slide);
    assert_eq!(end_mode(Previous, true), SwipeMode::Slide);
    assert_eq!(end_mode(Next, false), SwipeMode::NewTerminal);
    assert_eq!(end_mode(Previous, false), SwipeMode::Workspace);
}

#[test]
fn new_fab_progress_fills_to_the_commit_distance() {
    assert_eq!(new_fab_progress(0.0, 400.0), 0.0);
    assert_eq!(new_fab_progress(-160.0, 400.0), 1.0);
    assert_eq!(new_fab_progress(-80.0, 400.0), 0.5);
    assert_eq!(new_fab_progress(-320.0, 400.0), 1.0, "clamped");
    assert_eq!(new_fab_progress(80.0, 400.0), 0.5, "sign-independent");
    assert_eq!(new_fab_progress(-100.0, 0.0), 0.0, "zero-width guard");
}

#[test]
fn the_peel_and_the_fab_scale_clamp_to_the_armed_point() {
    assert_eq!(
        peek_card(0.0),
        PeekCard {
            scale: 1.0,
            shift_frac: 0.0,
            radius: 0.0
        }
    );
    assert_eq!(
        peek_card(1.0),
        PeekCard {
            scale: PEEK_SCALE_MIN,
            shift_frac: -0.05,
            radius: PEEK_RADIUS_PX
        }
    );
    assert_eq!(peek_card(2.0), peek_card(1.0));
    assert_eq!(new_fab_scale(0.0), 0.5);
    assert_eq!(new_fab_scale(1.0), 1.0);
    assert_eq!(new_fab_scale(-1.0), 0.5);
    assert_eq!(new_fab_scale(3.0), 1.0);
}

#[test]
fn a_card_dismisses_on_travel_or_a_same_way_flick() {
    assert!(!should_dismiss_card(143.0, 0.0));
    assert!(should_dismiss_card(144.0, 0.0));
    assert!(should_dismiss_card(30.0, 0.6));
    assert!(
        !should_dismiss_card(30.0, -0.6),
        "a flick opposite the drag"
    );
    assert!(!should_dismiss_card(10.0, 0.9), "below the travel floor");
    assert_eq!(card_swipe_alpha(0.0), 1.0);
    assert!((card_swipe_alpha(144.0) - 0.2).abs() < 1e-10);
    assert_eq!(card_swipe_alpha(1000.0), 0.2);
}

#[test]
fn the_slots_follow_the_finger_then_land_a_width_off() {
    assert_eq!(swipe_offsets_px(&track(), 400.0), (-200.0, 200.0));
    assert_eq!(
        swipe_offsets_px(&settled(SettleTarget::Commit), 400.0),
        (-400.0, 0.0)
    );
    assert_eq!(
        swipe_offsets_px(&settled(SettleTarget::Cancel), 400.0),
        (0.0, 400.0)
    );
    let backward = Swipe {
        dir: Previous,
        offset: 200.0,
        ..track()
    };
    assert_eq!(swipe_offsets_px(&backward, 400.0), (200.0, -200.0));
}

#[test]
fn only_the_swiped_slots_move_and_a_slide_tracks_without_transition() {
    let rest = swipe_style_for(None, "cur", 400.0);
    assert_eq!(rest.get("transform"), Some("none"));
    assert_eq!(rest.get("transition"), Some("none"));
    assert_eq!(swipe_style_for(Some(&track()), "other", 400.0), rest);
    let current = rest.clone().with("transform", "translateX(-200px)");
    assert_eq!(swipe_style_for(Some(&track()), "cur", 400.0), current);
    let neighbour = rest.clone().with("transform", "translateX(200px)");
    assert_eq!(swipe_style_for(Some(&track()), "nxt", 400.0), neighbour);
    let settling = Swipe {
        settle_ms: Some(250),
        ..settled(SettleTarget::Commit)
    };
    let style = swipe_style_for(Some(&settling), "cur", 400.0);
    assert_eq!(style.get("transform"), Some("translateX(-400px)"));
    assert!(
        style
            .get("transition")
            .is_some_and(|transition| transition.contains("250ms"))
    );
    let drawer = Swipe {
        mode: SwipeMode::Workspace,
        neighbor_id: None,
        ..track()
    };
    assert_eq!(
        swipe_style_for(Some(&drawer), "cur", 400.0),
        rest,
        "the drawer moves instead"
    );
}

#[test]
fn a_new_terminal_pull_peels_the_current_card() {
    let style = swipe_style_for(Some(&pull(-160.0)), "cur", 400.0);
    assert_eq!(style.get("transform"), Some("translateX(-20px) scale(0.9)"));
    assert_eq!(style.get("border-radius"), Some("28px"));
    assert_eq!(style.get("overflow"), Some("hidden"));
    assert_eq!(style.get("transition"), Some("none"));
    assert_eq!(
        swipe_style_for(Some(&pull(0.0)), "cur", 400.0).get("box-shadow"),
        Some("none")
    );
    assert_eq!(
        swipe_style_for(Some(&pull(-200.0)), "nxt", 400.0),
        swipe_style_for(None, "nxt", 400.0),
        "no neighbour slot"
    );
}

#[test]
fn only_a_compact_slide_brings_the_neighbours_bar() {
    assert_eq!(bar_neighbor_id(Some(&track()), true), Some("nxt"));
    assert_eq!(bar_neighbor_id(Some(&track()), false), None);
    assert_eq!(bar_neighbor_id(Some(&pull(-200.0)), true), None);
    assert_eq!(bar_neighbor_id(None, true), None);
}

const RECT: PaneRect = PaneRect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 800.0,
};

#[test]
fn the_peek_and_fab_are_inert_outside_a_measured_pull() {
    let none = InlineStyle::new().with("display", "none");
    assert_eq!(new_peek_style(None, Some(RECT), 400.0, 48.0), none);
    assert_eq!(new_fab_style(Some(&track()), Some(RECT), 400.0, 48.0), none);
    assert_eq!(new_peek_style(Some(&pull(-200.0)), None, 400.0, 48.0), none);
    assert_eq!(
        new_fab_style(Some(&pull(-200.0)), Some(RECT), 0.0, 48.0),
        none
    );
}

#[test]
fn the_peek_fills_the_terminal_area_and_fades_with_progress() {
    let style = new_peek_style(Some(&pull(-80.0)), Some(RECT), 400.0, 48.0);
    assert_eq!(style.get("top"), Some("48px"));
    assert_eq!(style.get("height"), Some("752px"));
    assert_eq!(style.get("opacity"), Some("0.5"));
    assert_eq!(style.get("transition"), Some("none"));
    let commit = Swipe {
        phase: SwipePhase::Settle,
        settle_target: Some(SettleTarget::Commit),
        ..pull(-200.0)
    };
    assert_eq!(
        new_peek_style(Some(&commit), Some(RECT), 400.0, 48.0).get("opacity"),
        Some("1")
    );
    let cancel = Swipe {
        phase: SwipePhase::Settle,
        settle_target: Some(SettleTarget::Cancel),
        ..pull(-200.0)
    };
    let cancelled = new_peek_style(Some(&cancel), Some(RECT), 400.0, 48.0);
    assert_eq!(cancelled.get("opacity"), Some("0"));
    assert!(
        cancelled
            .get("transition")
            .is_some_and(|t| t.contains(&format!("{NEW_BLOOM_MS}ms")))
    );
}

#[test]
fn the_fab_rests_inside_the_right_edge_then_blooms_to_the_whole_area() {
    let style = new_fab_style(Some(&pull(-80.0)), Some(RECT), 400.0, 48.0);
    assert_eq!(style.get("left"), Some("324px"), "400 - 20 - 56");
    assert_eq!(style.get("top"), Some("396px"), "48 + 752/2 - 56/2");
    assert_eq!(style.get("width"), Some("56px"));
    assert_eq!(style.get("border-radius"), Some("50%"));
    assert_eq!(style.get("transform"), Some("scale(0.75)"));
    let commit = Swipe {
        phase: SwipePhase::Settle,
        settle_target: Some(SettleTarget::Commit),
        ..pull(-200.0)
    };
    let bloom = new_fab_style(Some(&commit), Some(RECT), 400.0, 48.0);
    assert_eq!(bloom.get("left"), Some("0px"));
    assert_eq!(bloom.get("top"), Some("48px"));
    assert_eq!(bloom.get("width"), Some("400px"));
    assert_eq!(bloom.get("height"), Some("752px"));
    assert_eq!(bloom.get("border-radius"), Some("0px"));
    assert_eq!(bloom.get("transform"), Some("scale(1)"));
}

#[test]
fn a_release_commits_to_the_neighbour_blooms_a_new_terminal_or_springs_back() {
    let tabs = ["a".to_owned(), "b".to_owned()];
    let armed = arm_swipe(-10.0, &tabs, Some("a"), None).expect("armed on a tab");
    assert_eq!(
        (armed.mode, armed.neighbor_id.as_deref()),
        (SwipeMode::Slide, Some("b"))
    );
    let far = release_swipe(
        &Swipe {
            offset: -300.0,
            ..armed.clone()
        },
        -300.0,
        0.0,
        400.0,
    )
    .expect("tracking");
    assert_eq!(
        far.completion,
        SwipeCompletion::SelectNeighbor("b".to_owned())
    );
    assert_eq!(
        (far.settling.offset, far.settling.settle_ms),
        (-400.0, Some(125))
    );
    let short = release_swipe(
        &Swipe {
            offset: -40.0,
            ..armed
        },
        -40.0,
        0.0,
        400.0,
    )
    .expect("tracking");
    assert_eq!(short.completion, SwipeCompletion::Cancelled);
    assert_eq!(short.settling.settle_target, Some(SettleTarget::Cancel));
    let end = arm_swipe(-10.0, &tabs, Some("b"), None).expect("armed at the end");
    assert_eq!(end.mode, SwipeMode::NewTerminal);
    let bloom = release_swipe(
        &Swipe {
            offset: -200.0,
            ..end
        },
        -200.0,
        0.0,
        400.0,
    )
    .expect("tracking");
    assert_eq!(
        (bloom.completion, bloom.delay_ms),
        (SwipeCompletion::NewTerminal, NEW_BLOOM_MS + 20)
    );
    assert!(arm_swipe(-10.0, &tabs, Some("elsewhere"), None).is_none());
    assert!(
        arm_swipe(-10.0, &tabs, Some("a"), Some(&bloom.settling)).is_none(),
        "a settle is running"
    );
}
