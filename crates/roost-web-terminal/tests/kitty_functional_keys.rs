//! Kitty keyboard encoding of functional keys: the legacy CSI/SS3 forms flag 1
//! keeps, the private-use codes the spec declares, keypad identity, the legacy
//! no-guess rule for keys with no legacy sequence, and the controller routing a
//! modified Backspace through the encoder.

use roost_web_terminal::input::{
    AlternateKeys, InputControllerState, KeyChord, KeyDownAction, KeyEventType, KeyKind, Modifiers,
    NamedKey, TerminalKeyEvent, terminal_key_sequence,
};

fn named(key: NamedKey) -> KeyChord {
    KeyChord::named(key, Modifiers::NONE)
}

#[test]
fn functional_key_table_uses_legacy_forms_and_declared_pua_codes() {
    for (number, expected) in [
        (1, "\x1bOP"),
        (2, "\x1bOQ"),
        (3, "\x1b[13~"),
        (4, "\x1bOS"),
        (5, "\x1b[15~"),
        (6, "\x1b[17~"),
        (7, "\x1b[18~"),
        (8, "\x1b[19~"),
        (9, "\x1b[20~"),
        (10, "\x1b[21~"),
        (11, "\x1b[23~"),
        (12, "\x1b[24~"),
    ] {
        assert_eq!(
            terminal_key_sequence(&named(NamedKey::Function(number)), false, 1).as_deref(),
            Some(expected),
            "F{number}",
        );
    }
    for number in 13..=35 {
        let code = 57363 + u32::from(number);
        assert_eq!(
            terminal_key_sequence(&named(NamedKey::Function(number)), false, 1).as_deref(),
            Some(format!("\x1b[{code}u").as_str()),
            "F{number}",
        );
    }
    // Lock and modifier keys are absent here: they are reported only under
    // "report all keys" (`kitty_keyboard::a_bare_modifier_key_sends_nothing_…`).
    for code in (57361..=57363).chain(57376..=57440) {
        let encoded = terminal_key_sequence(&named(NamedKey::Functional(code)), false, 1);
        assert_eq!(encoded.as_deref(), Some(format!("\x1b[{code}u").as_str()));
    }
    for code in (57358..=57360).chain(57441..=57454) {
        let encoded = terminal_key_sequence(&named(NamedKey::Functional(code)), false, 8);
        assert_eq!(encoded.as_deref(), Some(format!("\x1b[{code};1u").as_str()));
    }
    for (dom_key, code) in [
        ("CapsLock", 57358),
        ("ScrollLock", 57359),
        ("NumLock", 57360),
        ("PrintScreen", 57361),
        ("Pause", 57362),
        ("ContextMenu", 57363),
        ("Numpad0", 57399),
        ("Numpad1", 57400),
        ("Numpad2", 57401),
        ("Numpad3", 57402),
        ("Numpad4", 57403),
        ("Numpad5", 57404),
        ("Numpad6", 57405),
        ("Numpad7", 57406),
        ("Numpad8", 57407),
        ("Numpad9", 57408),
        ("NumpadDecimal", 57409),
        ("NumpadDivide", 57410),
        ("NumpadMultiply", 57411),
        ("NumpadSubtract", 57412),
        ("NumpadAdd", 57413),
        ("NumpadEnter", 57414),
        ("NumpadEqual", 57415),
        ("NumpadComma", 57416),
        ("MediaPlay", 57428),
        ("MediaPause", 57429),
        ("MediaPlayPause", 57430),
        ("MediaReverse", 57431),
        ("MediaStop", 57432),
        ("MediaFastForward", 57433),
        ("MediaRewind", 57434),
        ("MediaTrackNext", 57435),
        ("MediaTrackPrevious", 57436),
        ("MediaRecord", 57437),
        ("AudioVolumeDown", 57438),
        ("AudioVolumeUp", 57439),
        ("AudioVolumeMute", 57440),
    ] {
        assert_eq!(
            KeyKind::from_dom_key(dom_key),
            KeyKind::Named(NamedKey::Functional(code)),
            "{dom_key}",
        );
    }
    assert_eq!(
        KeyKind::from_dom_key("F13"),
        KeyKind::Named(NamedKey::Function(13))
    );
    assert_eq!(
        KeyKind::from_dom_key("F35"),
        KeyKind::Named(NamedKey::Function(35))
    );
    assert_eq!(KeyKind::from_dom_key("F36"), KeyKind::BrowserOwned);
}
#[test]
fn disambiguation_uses_physical_keypad_codes_only_for_nontext_keys() {
    let controller = InputControllerState::new();
    let keypad_end = TerminalKeyEvent {
        key: "End",
        modifiers: Modifiers::NONE,
        alt_graph: false,
        is_composing: false,
        event_type: KeyEventType::Press,
        associated_text: None,
        alternate_keys: AlternateKeys::default(),
        key_override: Some(NamedKey::Functional(57424)),
    };
    assert_eq!(
        controller
            .dispatch_keydown_with_flags(&keypad_end, false, 1)
            .as_deref(),
        Some("\x1b[57424u")
    );
    let keypad_digit = TerminalKeyEvent {
        key: "1",
        key_override: Some(NamedKey::Functional(57400)),
        ..keypad_end
    };
    assert_eq!(
        controller
            .dispatch_keydown_with_flags(&keypad_digit, false, 1)
            .as_deref(),
        Some("1")
    );
}
#[test]
fn disambiguation_preserves_functional_csi_and_ss3_forms() {
    let arrow_up = named(NamedKey::ArrowUp);
    assert_eq!(
        terminal_key_sequence(&arrow_up, false, 1).as_deref(),
        Some("\x1b[A")
    );
    assert_eq!(
        terminal_key_sequence(&arrow_up, true, 1).as_deref(),
        Some("\x1bOA")
    );
    assert_eq!(
        terminal_key_sequence(&arrow_up.with_modifiers(Modifiers::SHIFT), false, 1,).as_deref(),
        Some("\x1b[1;2A"),
    );
    assert_eq!(
        terminal_key_sequence(&named(NamedKey::Home), false, 1).as_deref(),
        Some("\x1b[H"),
    );
    assert_eq!(
        terminal_key_sequence(&named(NamedKey::End), true, 1).as_deref(),
        Some("\x1bOF"),
    );
}
#[test]
fn kitty_flags_route_super_backspace_to_the_terminal_encoder() {
    let controller = InputControllerState::new();
    let event = TerminalKeyEvent {
        key: "Backspace",
        modifiers: Modifiers {
            super_key: true,
            ..Modifiers::NONE
        },
        alt_graph: false,
        is_composing: false,
        event_type: KeyEventType::Press,
        associated_text: None,
        alternate_keys: AlternateKeys::default(),
        key_override: None,
    };
    assert_eq!(
        controller.key_down_with_flags(&event, false, 1, || false),
        KeyDownAction::Write("\x1b[127;9u".to_owned()),
    );
}

#[test]
fn unmapped_functional_keys_do_not_guess_legacy_bytes() {
    assert_eq!(
        KeyKind::from_dom_key("F13"),
        KeyKind::Named(NamedKey::Function(13))
    );
    assert_eq!(named(NamedKey::Function(13)).to_bytes(false), None);
    assert_eq!(
        KeyKind::from_dom_key("ContextMenu"),
        KeyKind::Named(NamedKey::Functional(57363))
    );
    assert_eq!(named(NamedKey::Functional(57363)).to_bytes(false), None);
    assert!(named(NamedKey::Function(12)).to_bytes(false).is_some());
    assert!(named(NamedKey::Function(12)).to_bytes(true).is_some());
}
