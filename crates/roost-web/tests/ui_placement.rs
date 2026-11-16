//! Where floating panels go: the anchored menu flips to the side with room and
//! aligns inside its bounds. Exercises `roost_web::components::ui::placement`.

use roost_web::components::ui::placement::{
    ClientRect, EDGE_MARGIN_PX, Horizontal, Vertical, place_menu,
};

fn rect(left: f64, top: f64, width: f64, height: f64) -> ClientRect {
    ClientRect {
        left,
        top,
        width,
        height,
    }
}

const VIEWPORT: ClientRect = ClientRect {
    left: 0.0,
    top: 0.0,
    width: 1400.0,
    height: 900.0,
};

#[test]
fn a_composer_picker_rises_into_the_room_above_it_and_caps_there() {
    let deck = rect(350.0, 40.0, 1050.0, 860.0);
    let picker = rect(460.0, 840.0, 120.0, 28.0);
    let placement = place_menu(picker, deck, deck, true);
    assert_eq!(placement.vertical, Vertical::Bottom(900.0 - 840.0 + 4.0));
    assert_eq!(placement.max_height, 840.0 - 40.0 - 8.0 - 4.0);
    // Left half of the deck: anchored at the picker's left, in deck px.
    assert_eq!(placement.horizontal, Horizontal::Left(110.0));
}

#[test]
fn a_short_space_above_flips_the_menu_below() {
    let picker = rect(900.0, 60.0, 120.0, 28.0);
    let placement = place_menu(picker, VIEWPORT, VIEWPORT, true);
    assert!(matches!(placement.vertical, Vertical::Top(_)));
    assert_eq!(placement.max_height, 900.0 - 88.0 - 12.0);
}

#[test]
fn the_preferred_side_wins_when_it_is_comfortable() {
    let picker = rect(900.0, 300.0, 120.0, 28.0);
    let placement = place_menu(picker, VIEWPORT, VIEWPORT, true);
    assert!(matches!(placement.vertical, Vertical::Bottom(_)));
}

#[test]
fn a_trigger_on_the_right_aligns_right_edges_inside_the_margin() {
    let picker = rect(1350.0, 300.0, 60.0, 28.0);
    let placement = place_menu(picker, VIEWPORT, VIEWPORT, false);
    assert_eq!(placement.horizontal, Horizontal::Right(8.0));
}

#[test]
fn inset_triggers_keep_wide_popups_inside_both_viewport_margins() {
    let viewport = rect(0.0, 0.0, 320.0, 740.0);
    for trigger_left in [17.0, 271.0] {
        let trigger = rect(trigger_left, 620.0, 32.0, 32.0);
        let placement = place_menu(trigger, viewport, viewport, true);
        let popup_width = 320.0_f64.min(placement.max_width);
        let popup_left = match placement.horizontal {
            Horizontal::Left(left) => left,
            Horizontal::Right(right) => viewport.right() - right - popup_width,
        };
        assert!(popup_left >= viewport.left + EDGE_MARGIN_PX);
        assert!(popup_left + popup_width <= viewport.right() - EDGE_MARGIN_PX);
    }
}
