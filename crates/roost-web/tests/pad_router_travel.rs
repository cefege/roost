//! The controller router's travel rules: a focused terminal scrolls until it
//! clamps and only then hands ↑/↓ to focus travel, the right stick never moves
//! focus, B peels one layer per press, the shoulders cycle tabs and panes with
//! wraparound, and the legend hides after its idle window.
//! Pins behaviour of `apps/web/src/lib/padActions.ts` that
//! `smoke/terminal/gamepad-nav.spec.ts` drives end to end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod pad_router_support;

use pad_router_support::{FakeDom, FakeSurfaces};
use roost_web::input_nav::pad_bindings::PadAction;
use roost_web::input_nav::pad_hints::PadHintContext;
use roost_web::input_nav::pad_router::{PAD_HINT_IDLE_MS, PadActionRouter, cycle_index};
use roost_web::input_nav::pad_surfaces::{PadPaneTarget, PadShellAction, SyntheticKey};
use roost_web_terminal::reader_scroll::PAD_SCROLL_STEP_PX;

fn run(
    router: &mut PadActionRouter,
    dom: &mut FakeDom,
    surfaces: &mut FakeSurfaces,
    actions: &[PadAction],
) {
    router.run_pad_actions(actions, true, 0.0, dom, surfaces);
}

fn pane(pane_id: &str, tabs: &[&str], selected: &str, panes: &[&str]) -> PadPaneTarget {
    PadPaneTarget {
        pane_id: pane_id.to_string(),
        tabs: tabs.iter().map(|tab| tab.to_string()).collect(),
        selected_tab: selected.to_string(),
        layout_pane_ids: panes.iter().map(|pane| pane.to_string()).collect(),
    }
}

#[test]
fn a_focused_terminal_scrolls_until_it_clamps_then_travels() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::on_terminal_box(),
        FakeSurfaces::default(),
    );
    dom.focused_box_can_scroll = true;

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MoveUp]);
    assert_eq!(dom.focused_box_scrolls, vec![-PAD_SCROLL_STEP_PX]);
    assert_eq!(dom.keys, vec![]);
    assert_eq!(router.hints().context, PadHintContext::Terminal);

    dom.focused_box_can_scroll = false;
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MoveDown]);
    assert_eq!(dom.keys, vec![SyntheticKey::ArrowDown]);
}

#[test]
fn the_right_stick_scrolls_the_target_pane_and_never_moves_focus() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );

    run(
        &mut router,
        &mut dom,
        &mut surfaces,
        &[PadAction::ScrollDown],
    );
    assert_eq!(
        dom.pane_scrolls,
        vec![],
        "no routed session: nothing to scroll"
    );

    surfaces.target = Some(pane("pane-1", &["a"], "a", &["pane-1"]));
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::ScrollUp]);
    assert_eq!(
        dom.pane_scrolls,
        vec![("pane-1".to_string(), -PAD_SCROLL_STEP_PX)]
    );
    assert_eq!(dom.keys, vec![]);
}

#[test]
fn sideways_travel_is_one_synthetic_arrow() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );

    run(
        &mut router,
        &mut dom,
        &mut surfaces,
        &[PadAction::MoveLeft, PadAction::MoveRight],
    );

    assert_eq!(
        dom.keys,
        vec![SyntheticKey::ArrowLeft, SyntheticKey::ArrowRight]
    );
}

#[test]
fn b_leaves_the_pty_textarea_before_it_closes_the_drawer() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );
    surfaces.state.sidebar_open = true;
    dom.key_target_is_terminal_input = true;

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Back]);
    assert_eq!(dom.keys, vec![SyntheticKey::Escape]);
    assert_eq!(dom.left_terminal_input, 1);
    assert!(surfaces.state.sidebar_open, "one press, one effect");

    dom.key_target_is_terminal_input = false;
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Back]);
    assert!(!surfaces.state.sidebar_open);
}

#[test]
fn a_claimed_escape_ends_the_back_press() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );
    surfaces.state.sidebar_open = true;
    dom.keys_unclaimed = false;

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Back]);

    assert_eq!(dom.keys, vec![SyntheticKey::Escape]);
    assert!(surfaces.state.sidebar_open);
}

#[test]
fn x_toggles_the_palette_and_the_legend_follows_it() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Palette]);
    assert!(surfaces.state.palette_open);
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MoveDown]);
    assert_eq!(router.hints().context, PadHintContext::Overlay);
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Palette]);
    assert!(!surfaces.state.palette_open);
}

#[test]
fn y_opens_the_focused_items_menu() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );

    run(
        &mut router,
        &mut dom,
        &mut surfaces,
        &[PadAction::ContextMenu],
    );
    assert_eq!(dom.context_menus, 1);

    dom.focus.present = false;
    run(
        &mut router,
        &mut dom,
        &mut surfaces,
        &[PadAction::ContextMenu],
    );
    assert_eq!(dom.context_menus, 1);
}

#[test]
fn the_shoulders_cycle_tabs_with_wraparound() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );
    surfaces.target = Some(pane("pane-1", &["a", "b", "c"], "c", &["pane-1"]));

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::TabNext]);
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::TabPrev]);

    assert_eq!(
        surfaces.executed,
        vec![
            PadShellAction::SelectTab {
                tab: "a".to_string()
            },
            PadShellAction::SelectTab {
                tab: "b".to_string()
            },
        ]
    );

    // A single tab has nowhere to go.
    surfaces.executed.clear();
    surfaces.target = Some(pane("pane-1", &["a"], "a", &["pane-1"]));
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::TabNext]);
    assert_eq!(surfaces.executed, vec![]);
}

#[test]
fn the_triggers_cycle_panes_with_wraparound() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );
    surfaces.target = Some(pane("left", &["a"], "a", &["left", "right"]));

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::PanePrev]);

    assert_eq!(
        surfaces.executed,
        vec![PadShellAction::FocusPane {
            pane_id: "right".to_string()
        }]
    );
}

#[test]
fn cycling_counts_an_unknown_current_entry_from_before_the_first() {
    assert_eq!(cycle_index(Some(2), 1, 3), Some(0));
    assert_eq!(cycle_index(Some(0), -1, 3), Some(2));
    assert_eq!(cycle_index(None, 1, 3), Some(0));
    assert_eq!(cycle_index(None, -1, 3), Some(1));
    assert_eq!(cycle_index(None, 1, 0), None);
}

#[test]
fn the_legend_hides_after_its_idle_window_and_each_press_re_arms_it() {
    let (mut router, mut dom, mut surfaces) = (
        PadActionRouter::new(),
        FakeDom::default(),
        FakeSurfaces::default(),
    );

    router.run_pad_actions(
        &[PadAction::MoveDown],
        true,
        1000.0,
        &mut dom,
        &mut surfaces,
    );
    router.run_pad_actions(
        &[PadAction::MoveDown],
        true,
        3000.0,
        &mut dom,
        &mut surfaces,
    );

    // The first press's timer fires early and must change nothing.
    assert!(!router.expire_hints(1000.0 + PAD_HINT_IDLE_MS));
    assert!(router.hints().visible);
    assert!(router.expire_hints(3000.0 + PAD_HINT_IDLE_MS));
    assert!(!router.hints().visible);
}
