//! The link gesture rules: platform-specific physical modifiers, compact
//! arming, the total modifier level, and which presses bypass the PTY.
//! Test names follow v2's `apps/web/tests/renderer/terminal-links.test.ts`
//! and `terminal-links.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web_terminal::links::{
    LinkActivationGesture, LinkModifierKey, PressWithheld, is_link_activation_gesture,
    is_link_modifier_held, withhold_press,
};

fn gesture(button: i16, ctrl: bool, meta: bool) -> LinkActivationGesture {
    LinkActivationGesture {
        button,
        ctrl,
        meta,
        shift: false,
        alt: false,
    }
}

#[test]
fn physical_ctrl_and_meta_gestures_remain_platform_specific() {
    let (mac, other) = (
        LinkModifierKey::for_platform(true),
        LinkModifierKey::for_platform(false),
    );
    let (meta, ctrl) = (gesture(0, false, true), gesture(0, true, false));
    assert!(is_link_activation_gesture(&meta, false, mac));
    assert!(!is_link_activation_gesture(&ctrl, false, mac));
    assert!(is_link_activation_gesture(&ctrl, false, other));
    assert!(!is_link_activation_gesture(&meta, false, other));
    // Compact arming opens with no modifier; a right or shifted click never does.
    assert!(is_link_activation_gesture(
        &gesture(0, false, false),
        true,
        mac
    ));
    assert!(!is_link_activation_gesture(
        &gesture(2, false, false),
        true,
        mac
    ));
    assert!(!is_link_activation_gesture(
        &LinkActivationGesture {
            shift: true,
            ..ctrl
        },
        false,
        other
    ));
}

#[test]
fn the_modifier_level_is_total_and_an_event_without_modifier_fields_is_not_held() {
    assert!(!is_link_modifier_held(
        &LinkActivationGesture::default(),
        LinkModifierKey::Meta
    ));
    assert!(!is_link_modifier_held(
        &LinkActivationGesture::default(),
        LinkModifierKey::Control
    ));
    assert!(is_link_modifier_held(
        &gesture(0, false, true),
        LinkModifierKey::Meta
    ));
    assert!(!is_link_modifier_held(
        &gesture(0, true, false),
        LinkModifierKey::Meta
    ));
}

#[test]
fn armed_and_physical_modifier_terminal_links_bypass_pty_bytes_while_bare_clicks_forward() {
    let mac = LinkModifierKey::Meta;
    let withheld = |over, modifier: &LinkActivationGesture, armed| {
        withhold_press(over, modifier, armed, mac, false)
    };
    assert_eq!(
        withheld(true, &gesture(0, false, true), false),
        Some(PressWithheld::LinkActivation)
    );
    assert_eq!(withheld(true, &gesture(0, false, false), false), None);
    assert_eq!(
        withheld(true, &gesture(0, false, false), true),
        Some(PressWithheld::LinkActivation)
    );
    assert_eq!(withheld(false, &gesture(0, false, false), true), None);
    let middle = withhold_press(true, &gesture(1, false, false), true, mac, true);
    assert_eq!(middle, Some(PressWithheld::DeckMiddleButton));
}
