//! Pins the controller contract a keyboard-free session depends on: the stick
//! clicks are bound at all (they are the only path to the mic and to another
//! folder), no discrete command auto-repeats, and the transient legend can
//! never name a control the controller map does not describe.
//! Ports `apps/web/tests/padBindings.test.ts`.

use roost_web::input_nav::pad_bindings::{PAD_CONTROL_GUIDE, PadAction, PadControl, button_action};
use roost_web::input_nav::pad_hints::{PadHint, PadHintContext, pad_hints};

fn hint(cap: &str, label: &'static str) -> PadHint {
    PadHint {
        cap: cap.to_string(),
        label,
    }
}

#[test]
fn binds_the_stick_clicks_and_the_controller_map() {
    assert_eq!(button_action(9), Some(PadAction::ControllerMap));
    assert_eq!(button_action(10), Some(PadAction::MicToggle));
    assert_eq!(button_action(11), Some(PadAction::FolderNext));
    // 16 is the optional guide button: unbound, so the map can show it lighting
    // up without it doing anything.
    assert_eq!(button_action(16), None);
}

#[test]
fn no_discrete_command_auto_repeats_while_held() {
    // A held stick click that fired every frame would restart dictation or walk
    // every folder in a second.
    for action in [
        PadAction::MicToggle,
        PadAction::FolderNext,
        PadAction::ControllerMap,
        PadAction::Keypad,
        PadAction::Activate,
        PadAction::Back,
    ] {
        assert!(
            !action.is_repeating(),
            "{} must not repeat",
            action.as_str()
        );
    }
    assert!(PadAction::MoveDown.is_repeating());
    assert!(PadAction::ScrollUp.is_repeating());
}

#[test]
fn the_guide_describes_every_bound_button() {
    // The D-pad and stick rows cover a direction family, so they carry no single
    // action; every other binding must name its own row.
    let clustered = [
        PadAction::MoveUp,
        PadAction::MoveDown,
        PadAction::MoveLeft,
        PadAction::MoveRight,
    ];
    let undocumented: Vec<PadAction> = (0..32)
        .filter_map(button_action)
        .filter(|action| !clustered.contains(action))
        .filter(|action| {
            !PAD_CONTROL_GUIDE
                .iter()
                .any(|row| row.action == Some(*action))
        })
        .collect();
    assert_eq!(undocumented, vec![]);
}

#[test]
fn every_control_resolves_to_its_own_guide_row() {
    for row in PAD_CONTROL_GUIDE {
        assert_eq!(
            row.control.guide(),
            &row,
            "{} resolves to another row",
            row.cap
        );
        assert_eq!(row.control.cap(), row.cap);
    }
}

#[test]
fn every_context_has_a_legend() {
    for context in PadHintContext::ALL {
        assert!(
            !pad_hints(context).is_empty(),
            "{} has no legend",
            context.as_str()
        );
    }
}

#[test]
fn a_legend_row_that_does_not_override_its_verb_reuses_the_guides() {
    let default = pad_hints(PadHintContext::Default);
    let mic = default.iter().find(|hint| hint.cap == "L3").unwrap();
    assert_eq!(mic.label, PadControl::L3.guide().label);
    let terminal = pad_hints(PadHintContext::Terminal);
    let tab = terminal.iter().find(|hint| hint.cap == "LB/RB").unwrap();
    assert_eq!(tab.label, PadControl::Lb.guide().label);
}

#[test]
fn the_key_pad_and_dictation_surfaces_name_their_own_buttons() {
    assert_eq!(
        pad_hints(PadHintContext::Keypad),
        vec![
            hint("D-pad", "Move"),
            hint("A", "Press"),
            hint("B", "Close")
        ]
    );
    assert_eq!(
        pad_hints(PadHintContext::Dictation),
        vec![hint("A", "Send"), hint("B", "Discard"), hint("L3", "Stop")]
    );
}

#[test]
fn mic_folders_and_the_guide_are_advertised_where_they_work() {
    for context in [PadHintContext::Default, PadHintContext::Terminal] {
        let hints = pad_hints(context);
        assert!(hints.contains(&hint("L3", "Mic")), "{}", context.as_str());
        assert!(
            hints.contains(&hint("R3", "Folder")),
            "{}",
            context.as_str()
        );
        assert!(
            hints.contains(&hint("Start", "Guide")),
            "{}",
            context.as_str()
        );
    }
}
