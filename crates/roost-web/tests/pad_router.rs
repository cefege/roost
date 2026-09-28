//! Which seam each controller press reaches — the difference between a pad
//! that can finish a task and one that dead-ends. A/B must mean COMMIT/DISCARD
//! while dictating and press-a-key/leave while the terminal key pad is open, a
//! mic press with no grant must explain itself rather than do nothing, and the
//! open controller map stands everything down but Start and B.
//! Ports `apps/web/tests/padActions.dispatch.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod pad_router_support;

use pad_router_support::{FakeDom, FakeSurfaces};
use roost_web::input_nav::pad_bindings::{PadAction, PadHintContext};
use roost_web::input_nav::pad_folders::FolderLead;
use roost_web::input_nav::pad_router::{MIC_NEEDS_GESTURE_WARNING, PadActionRouter};
use roost_web::input_nav::pad_surfaces::{PadDictation, PadFolderCycle, PadShellAction, SyntheticKey};

fn run(router: &mut PadActionRouter, dom: &mut FakeDom, surfaces: &mut FakeSurfaces, actions: &[PadAction]) {
    router.run_pad_actions(actions, true, 0.0, dom, surfaces);
}

fn mic(dictating: bool, can_start_without_gesture: bool) -> PadDictation {
    PadDictation { dictating, controls_mounted: true, can_start_without_gesture }
}

#[test]
fn a_on_the_terminal_box_opens_the_pad_and_lands_on_its_first_key() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::on_terminal_box(), FakeSurfaces::default());

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Activate]);

    assert!(surfaces.state.keypad_open);
    assert_eq!(dom.keypad_focus_starts, 1);
    // No synthetic key: the box would have swallowed it into the PTY.
    assert_eq!(dom.keys, vec![]);
}

#[test]
fn a_later_open_cancels_the_superseded_focus_retry() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::on_terminal_box(), FakeSurfaces::default());

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Activate]);
    let armed = dom.keypad_focus_cancels.get();
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Keypad]); // closes
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Keypad]); // opens again

    assert_eq!(dom.keypad_focus_starts, 2);
    assert_eq!(dom.keypad_focus_cancels.get(), armed + 1);
}

#[test]
fn a_on_a_key_presses_it_instead_of_re_toggling_the_pad() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::on_keypad_key(), FakeSurfaces::default());
    surfaces.state.keypad_open = true;

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Activate]);

    assert_eq!(surfaces.count(&PadShellAction::ToggleKeypad), 0);
    assert_eq!(dom.keys, vec![SyntheticKey::Enter]);
    assert_eq!(dom.clicks, 1);
}

#[test]
fn a_claimed_enter_is_not_clicked_again() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());
    dom.keys_unclaimed = false;

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Activate]);

    assert_eq!(dom.keys, vec![SyntheticKey::Enter]);
    assert_eq!(dom.clicks, 0);
}

#[test]
fn b_on_a_key_closes_the_pad_and_returns_focus_to_the_terminal() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::on_keypad_key(), FakeSurfaces::default());
    surfaces.state.keypad_open = true;

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Back]);

    assert_eq!(surfaces.count(&PadShellAction::CloseKeypad), 1);
    assert_eq!(dom.terminal_focuses.len(), 1);
    // No Escape: a body-portal key would escape past the pad to the document.
    assert_eq!(dom.keys, vec![]);
}

#[test]
fn the_legend_names_the_key_pad_while_focus_is_inside_it() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::on_keypad_key(), FakeSurfaces::default());
    surfaces.state.keypad_open = true;

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MoveDown]);

    assert!(router.hints().visible);
    assert_eq!(router.hints().context, PadHintContext::Keypad);
}

#[test]
fn a_sends_and_b_discards_and_neither_reaches_the_focused_element() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());
    surfaces.state.dictation = mic(true, true);

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Activate]);
    assert_eq!(surfaces.count(&PadShellAction::ToggleDictation), 1);

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Back]);
    assert_eq!(surfaces.count(&PadShellAction::DiscardDictation), 1);

    assert_eq!(dom.keys, vec![]);
    assert_eq!(dom.clicks, 0);
    assert_eq!(router.hints().context, PadHintContext::Dictation);
}

#[test]
fn the_stick_click_starts_the_mic_when_the_browser_would_allow_it() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());
    surfaces.state.dictation = mic(false, true);

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MicToggle]);

    assert_eq!(surfaces.count(&PadShellAction::ToggleDictation), 1);
}

#[test]
fn a_start_the_browser_would_deny_explains_itself_exactly_once() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());
    surfaces.state.dictation = mic(false, false);

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MicToggle]);
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MicToggle]);

    assert_eq!(surfaces.count(&PadShellAction::ToggleDictation), 0);
    assert_eq!(surfaces.count(&PadShellAction::Warn { message: MIC_NEEDS_GESTURE_WARNING }), 1);
}

#[test]
fn no_mounted_composer_leaves_the_press_inert() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::MicToggle]);

    assert_eq!(surfaces.executed, vec![]);
}

#[test]
fn the_stick_click_opens_the_next_folders_newest_session() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());
    let lead = |key: &str, lead_id: &str| FolderLead { key: key.to_string(), lead_id: lead_id.to_string() };
    surfaces.folders = Some(PadFolderCycle {
        folders: vec![lead("web", "web-new"), lead("api", "api-new")],
        current_folder_key: None,
    });

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::FolderNext]);
    let opened = PadShellAction::OpenSession { session_id: "web-new".to_string() };
    assert_eq!(surfaces.executed, vec![opened.clone()]);

    // Nothing to switch to must not navigate to a stale target.
    surfaces.folders = Some(PadFolderCycle { folders: vec![], current_folder_key: None });
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::FolderNext]);
    // No router wired yet: inert too.
    surfaces.folders = None;
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::FolderNext]);
    assert_eq!(surfaces.executed, vec![opened]);
}

#[test]
fn start_toggles_the_controller_map() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::ControllerMap]);
    assert!(surfaces.state.controller_map_open);

    run(&mut router, &mut dom, &mut surfaces, &[PadAction::ControllerMap]);
    assert!(!surfaces.state.controller_map_open);
}

#[test]
fn the_open_map_stands_the_dispatcher_down_except_start_and_b() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::ControllerMap]);

    // Reading the diagram means holding buttons: firing what they document
    // would close the map, stack a palette on it, or switch tabs behind it.
    run(
        &mut router,
        &mut dom,
        &mut surfaces,
        &[PadAction::Activate, PadAction::Palette, PadAction::TabNext, PadAction::MoveDown, PadAction::Keypad],
    );
    assert!(surfaces.state.controller_map_open);
    assert!(!surfaces.state.palette_open);
    assert_eq!(surfaces.count(&PadShellAction::ToggleKeypad), 0);
    assert_eq!(dom.keys, vec![]);
    assert_eq!(dom.clicks, 0);

    // B closes it outright: a dispatched Escape would dismiss the dialog and
    // then fall through to close the drawer behind it as well.
    surfaces.state.sidebar_open = true;
    run(&mut router, &mut dom, &mut surfaces, &[PadAction::Back]);
    assert!(!surfaces.state.controller_map_open);
    assert!(surfaces.state.sidebar_open);
    assert_eq!(dom.keys, vec![]);
}

#[test]
fn nothing_routes_while_controller_mode_is_inactive() {
    let (mut router, mut dom, mut surfaces) = (PadActionRouter::new(), FakeDom::default(), FakeSurfaces::default());

    router.run_pad_actions(&[PadAction::Palette, PadAction::MoveDown], false, 0.0, &mut dom, &mut surfaces);

    assert_eq!(surfaces.executed, vec![]);
    assert_eq!(dom.keys, vec![]);
    assert!(!router.hints().visible);
}
