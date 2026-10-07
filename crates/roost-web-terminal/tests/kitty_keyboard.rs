//! Pure encoder behavior for kitty progressive keyboard enhancements.
//!
//! These tests pin legacy compatibility, key/event parameter syntax, and the
//! modifier bits without constructing browser events or a terminal; the
//! functional-key table is `kitty_functional_keys`.

use roost_web_terminal::input::{
    AlternateKeys, KeyChord, KeyEventType, Modifiers, NamedKey, terminal_key_sequence,
    terminal_key_sequence_for_event,
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
        (
            held(
                'c',
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                },
            ),
            "\x03",
        ),
        (named(NamedKey::Escape), "\x1b"),
        (named(NamedKey::ArrowUp), "\x1b[A"),
        (named(NamedKey::Function(5)), "\x1b[15~"),
        (named(NamedKey::Enter), "\r"),
    ] {
        assert_eq!(
            terminal_key_sequence(&chord, false, 0).as_deref(),
            Some(expected)
        );
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
            &held(
                'a',
                Modifiers {
                    alt: true,
                    ..Modifiers::NONE
                }
            ),
            false,
            1,
        )
        .as_deref(),
        Some("\x1b[97;3u"),
    );
    assert_eq!(
        terminal_key_sequence(
            &held(
                'c',
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                }
            ),
            false,
            1,
        )
        .as_deref(),
        Some("\x1b[99;5u"),
    );
    assert_eq!(
        terminal_key_sequence(&printable('a'), false, 1).as_deref(),
        Some("a")
    );
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
    assert_eq!(
        encode(KeyEventType::Release).as_deref(),
        Some("\x1b[1;1:3A")
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &named(NamedKey::Enter),
            false,
            2,
            KeyEventType::Release,
            None,
            AlternateKeys::default(),
        ),
        None,
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &named(NamedKey::Enter),
            false,
            10,
            KeyEventType::Release,
            None,
            AlternateKeys::default(),
        )
        .as_deref(),
        Some("\x1b[13;1:3u"),
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &printable('x'),
            false,
            2,
            KeyEventType::Release,
            None,
            AlternateKeys::default(),
        ),
        None,
    );
}

#[test]
fn all_key_and_associated_text_examples_encode_unicode_scalars() {
    assert_eq!(
        terminal_key_sequence(&printable('a'), false, 8).as_deref(),
        Some("\x1b[97;1u")
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &printable('A').with_modifiers(Modifiers::SHIFT),
            false,
            8 | 2 | 16,
            KeyEventType::Press,
            Some("A"),
            AlternateKeys {
                unshifted: Some('a'),
                ..AlternateKeys::default()
            },
        )
        .as_deref(),
        Some("\x1b[97;2:1;65u"),
    );
    assert_eq!(
        terminal_key_sequence_for_event(
            &held(
                'a',
                Modifiers {
                    alt: true,
                    ..Modifiers::NONE
                }
            ),
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
            &printable('c'),
            false,
            8 | 16,
            KeyEventType::Press,
            Some("\u{3}"),
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
            &held(
                'x',
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                }
            ),
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
