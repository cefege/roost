//! Pure encoder behavior for kitty progressive keyboard enhancements.
//!
//! These tests pin legacy compatibility, key/event parameter syntax, and the
//! functional-key table without constructing browser events or a terminal.

use roost_web_terminal::input::{
    AlternateKeys, InputControllerState, KeyChord, KeyDownAction, KeyEventType, KeyKind,
    Modifiers, NamedKey, TerminalKeyEvent, terminal_key_sequence, terminal_key_sequence_for_event,
};

fn named(key: NamedKey) -> KeyChord {
    KeyChord::named(key, Modifiers::NONE)
}

fn printable(character: char) -> KeyChord {
    KeyChord::printable(character)
}

fn held(character: char, modifiers: Modifiers) -> KeyChord {
    KeyChord::printable(character).with_modifiers(modifiers)
}

#[test]
fn zero_flags_preserve_legacy_sequences() {
    for (chord, expected) in [
        (printable('a'), "a"),
        (held('c', Modifiers { ctrl: true, ..Modifiers::NONE }), "\x03"),
        (named(NamedKey::Escape), "\x1b"),
        (named(NamedKey::ArrowUp), "\x1b[A"),
        (named(NamedKey::Function(5)), "\x1b[15~"),
        (named(NamedKey::Enter), "\r"),
    ] {
        assert_eq!(terminal_key_sequence(&chord, false, 0).as_deref(), Some(expected));
    }
}

#[test]
fn disambiguation_uses_unicode_codes_only_for_ambiguous_legacy_keys() {
    assert_eq!(
        terminal_key_sequence(&named(NamedKey::Escape), false, 1).as_deref(),
        Some("\x1b[27u"),
    );
    assert_eq!(
        terminal_key_sequence(
            &held('a', Modifiers { alt: true, ..Modifiers::NONE }),
            false,
            1,
        )
        .as_deref(),
        Some("\x1b[97;3u"),
    );
    assert_eq!(
        terminal_key_sequence(
            &held('c', Modifiers { ctrl: true, ..Modifiers::NONE }),
            false,
            1,
        )
        .as_deref(),
        Some("\x1b[99;5u"),
    );
    assert_eq!(terminal_key_sequence(&printable('a'), false, 1).as_deref(), Some("a"));
}

#[test]
fn event_reporting_encodes_press_repeat_release_and_legacy_control_exceptions() {
    let arrow = named(NamedKey::ArrowUp);
    let encode = |event_type| {
        terminal_key_sequence_for_event(
            &arrow,
            false,
            2,
            event_type,
            None,
            AlternateKeys::default(),
        )
    };
    assert_eq!(encode(KeyEventType::Press).as_deref(), Some("\x1b[1;1:1A"));
    assert_eq!(encode(KeyEventType::Repeat).as_deref(), Some("\x1b[1;1:2A"));
    assert_eq!(encode(KeyEventType::Release).as_deref(), Some("\x1b[1;1:3A"));
    assert_eq!(
        terminal_key_sequence_for_event(
            &named(NamedKey::Enter), false, 2, KeyEventType::Release, None,
            AlternateKeys::default(),
        ),
        None,
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &named(NamedKey::Enter), false, 10, KeyEventType::Release, None,
            AlternateKeys::default(),
        )
        .as_deref(),
        Some("\x1b[13;1:3u"),
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &printable('x'), false, 2, KeyEventType::Release, None,
            AlternateKeys::default(),
        ),
        None,
    );
}

#[test]
fn all_key_and_associated_text_examples_encode_unicode_scalars() {
    assert_eq!(terminal_key_sequence(&printable('a'), false, 8).as_deref(), Some("\x1b[97;1u"));
    assert_eq!(
        terminal_key_sequence_for_event(
            &printable('A').with_modifiers(Modifiers::SHIFT),
            false,
            8 | 2 | 16,
            KeyEventType::Press,
            Some("A"),
            AlternateKeys { unshifted: Some('a'), ..AlternateKeys::default() },
        )
        .as_deref(),
        Some("\x1b[97;2:1;65u"),
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &held('a', Modifiers { alt: true, ..Modifiers::NONE }),
            false,
            8 | 16,
            KeyEventType::Press,
            Some("å🙂"),
            AlternateKeys::default(),
        )
        .as_deref(),
        Some("\x1b[97;3;229:128578u"),
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &printable('c'), false, 8 | 16, KeyEventType::Press, Some("\u{3}"),
            AlternateKeys::default(),
        )
        .as_deref(),
        Some("\x1b[99;1u"),
    );
}

#[test]
fn alternate_keys_report_shifted_and_base_layout_fields() {
    assert_eq!(
        terminal_key_sequence_for_event(
            &printable('A').with_modifiers(Modifiers::SHIFT),
            false,
            4 | 8,
            KeyEventType::Press,
            None,
            AlternateKeys {
                shifted: Some('A'),
                base_layout: Some('c'),
                unshifted: Some('a'),
            },
        )
        .as_deref(),
        Some("\x1b[97:65:99;2u"),
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &held('x', Modifiers { ctrl: true, ..Modifiers::NONE }),
            false,
            4 | 8,
            KeyEventType::Press,
            None,
            AlternateKeys {
                shifted: None,
                base_layout: Some('c'),
                unshifted: Some('x'),
            },
        )
        .as_deref(),
        Some("\x1b[120::99;5u"),
    );
}

#[test]
fn kitty_modifiers_cover_all_eight_specified_bits() {
    let modifiers = Modifiers {
        shift: true,
        alt: true,
        ctrl: true,
        super_key: true,
        hyper: true,
        meta: true,
        caps_lock: true,
        num_lock: true,
    };
    assert_eq!(
        terminal_key_sequence(&held('a', modifiers), false, 8).as_deref(),
        Some("\x1b[97;256u"),
    );
}

#[test]
fn functional_key_table_uses_legacy_forms_and_declared_pua_codes() {
    for (number, expected) in [
        (1, "\x1bOP"), (2, "\x1bOQ"), (3, "\x1b[13~"), (4, "\x1bOS"),
        (5, "\x1b[15~"), (6, "\x1b[17~"), (7, "\x1b[18~"), (8, "\x1b[19~"),
        (9, "\x1b[20~"), (10, "\x1b[21~"), (11, "\x1b[23~"), (12, "\x1b[24~"),
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
    for code in (57358..=57363).chain(57376..=57454) {
        let encoded = terminal_key_sequence(&named(NamedKey::Functional(code)), false, 1);
        assert_eq!(encoded.as_deref(), Some(format!("\x1b[{code}u").as_str()));
    }
    for (dom_key, code) in [
        ("CapsLock", 57358), ("ScrollLock", 57359), ("NumLock", 57360),
        ("PrintScreen", 57361), ("Pause", 57362), ("ContextMenu", 57363),
        ("Numpad0", 57399), ("Numpad1", 57400), ("Numpad2", 57401),
        ("Numpad3", 57402), ("Numpad4", 57403), ("Numpad5", 57404),
        ("Numpad6", 57405), ("Numpad7", 57406), ("Numpad8", 57407),
        ("Numpad9", 57408), ("NumpadDecimal", 57409), ("NumpadDivide", 57410),
        ("NumpadMultiply", 57411), ("NumpadSubtract", 57412), ("NumpadAdd", 57413),
        ("NumpadEnter", 57414), ("NumpadEqual", 57415), ("NumpadComma", 57416),
        ("MediaPlay", 57428), ("MediaPause", 57429), ("MediaPlayPause", 57430),
        ("MediaReverse", 57431), ("MediaStop", 57432), ("MediaFastForward", 57433),
        ("MediaRewind", 57434), ("MediaTrackNext", 57435), ("MediaTrackPrevious", 57436),
        ("MediaRecord", 57437), ("AudioVolumeDown", 57438), ("AudioVolumeUp", 57439),
        ("AudioVolumeMute", 57440),
    ] {
        assert_eq!(
            KeyKind::from_dom_key(dom_key),
            KeyKind::Named(NamedKey::Functional(code)),
            "{dom_key}",
        );
    }
    assert_eq!(KeyKind::from_dom_key("F13"), KeyKind::Named(NamedKey::Function(13)));
    assert_eq!(KeyKind::from_dom_key("F35"), KeyKind::Named(NamedKey::Function(35)));
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
    assert_eq!(terminal_key_sequence(&arrow_up, false, 1).as_deref(), Some("\x1b[A"));
    assert_eq!(terminal_key_sequence(&arrow_up, true, 1).as_deref(), Some("\x1bOA"));
    assert_eq!(
        terminal_key_sequence(
            &arrow_up.with_modifiers(Modifiers::SHIFT),
            false,
            1,
        )
        .as_deref(),
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
