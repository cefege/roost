//! The key mapper: which bytes one key event puts on the wire, and that the
//! core's admission router accepts exactly those bytes for the same lane.
//!
//! The router cases are the ones that matter for a doubled keystroke. The
//! mapper decides the bytes; the router decides whether they are written. A
//! key the two sides encoded differently would be written as one sequence and
//! accounted for as another, so every case drives both.

use roost_client_core::terminal::{InputPhase, InputRouter};
use roost_web_terminal::input::{
    KeyChord, KeyKind, Modifiers, NamedKey, apply_ctrl_modifier, is_terminal_printable_key,
    modifier_parameter,
};

/// A named key with no modifiers.
fn named(key: NamedKey) -> KeyChord {
    KeyChord::named(key, Modifiers::NONE)
}

/// A printable key with no modifiers.
fn printable(character: char) -> KeyChord {
    KeyChord::printable(character)
}

/// A printable key with one modifier held.
fn held(character: char, modifiers: Modifiers) -> KeyChord {
    KeyChord::printable(character).with_modifiers(modifiers)
}

/// A chord the browser's own text services own, which the pane must not encode.
fn browser_owned(dom_key: &str) -> KeyChord {
    KeyChord {
        kind: KeyKind::from_dom_key(dom_key),
        modifiers: Modifiers::NONE,
        alt_graph: false,
        is_composing: false,
    }
}

#[test]
fn maps_terminal_ctrl_characters_and_leaves_unsupported_input_intact() {
    for character in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz".chars() {
        let expected = char::from_u32(character as u32 & 0x1f).expect("a control code point");
        assert_eq!(
            apply_ctrl_modifier(&character.to_string()),
            expected.to_string()
        );
    }
    for (input, expected) in [
        (" ", "\0"),
        ("@", "\0"),
        ("[", "\x1b"),
        ("\\", "\x1c"),
        ("]", "\x1d"),
        ("^", "\x1e"),
        ("_", "\x1f"),
        ("?", "\x7f"),
    ] {
        assert_eq!(apply_ctrl_modifier(input), expected);
    }
    assert_eq!(apply_ctrl_modifier("1"), "1");
    assert_eq!(apply_ctrl_modifier("文字"), "文字");
}

#[test]
fn switches_cursor_and_home_end_keys_with_decckm_application_mode() {
    assert_eq!(
        named(NamedKey::ArrowUp).to_bytes(false).as_deref(),
        Some(&b"\x1b[A"[..])
    );
    assert_eq!(
        named(NamedKey::ArrowUp).to_bytes(true).as_deref(),
        Some(&b"\x1bOA"[..])
    );
    assert_eq!(
        named(NamedKey::Home).to_bytes(false).as_deref(),
        Some(&b"\x1b[H"[..])
    );
    assert_eq!(
        named(NamedKey::End).to_bytes(true).as_deref(),
        Some(&b"\x1bOF"[..])
    );
}

#[test]
fn encodes_navigation_function_and_special_key_modifiers() {
    assert_eq!(
        KeyChord::named(NamedKey::ArrowUp, Modifiers::SHIFT)
            .to_bytes(false)
            .as_deref(),
        Some(&b"\x1b[1;2A"[..])
    );
    assert_eq!(
        KeyChord::named(
            NamedKey::ArrowRight,
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        )
        .to_bytes(false)
        .as_deref(),
        Some(&b"\x1b[1;5C"[..])
    );
    assert_eq!(
        KeyChord::named(
            NamedKey::Function(1),
            Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            }
        )
        .to_bytes(false)
        .as_deref(),
        Some(&b"\x1b[1;6P"[..])
    );
    assert_eq!(
        KeyChord::named(
            NamedKey::Function(12),
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            }
        )
        .to_bytes(false)
        .as_deref(),
        Some(&b"\x1b[24;3~"[..])
    );
    assert_eq!(
        KeyChord::named(NamedKey::Tab, Modifiers::SHIFT)
            .to_bytes(false)
            .as_deref(),
        Some(&b"\x1b[Z"[..])
    );
    assert_eq!(
        KeyChord::named(NamedKey::Enter, Modifiers::SHIFT)
            .to_bytes(false)
            .as_deref(),
        Some(&b"\x1b[13;2u"[..])
    );
    assert_eq!(
        named(NamedKey::Enter).to_bytes(false).as_deref(),
        Some(&b"\r"[..])
    );
}

#[test]
fn preserves_text_ctrl_alt_and_meta_ownership() {
    assert_eq!(
        printable('é').to_bytes(false).as_deref(),
        Some("é".as_bytes())
    );
    assert_eq!(
        KeyChord::named(
            NamedKey::Function(1),
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            }
        )
        .to_bytes(false)
        .as_deref(),
        Some(&b"\x1b[1;3P"[..]),
        "Alt+F1 is F1 WITH the alt modifier, so it takes the CSI modifier form \
         and the modifier must survive. v2's terminalInput.ts returns \
         `\x1b[1;<modifier><final>` for any modified function key and reserves \
         SS3 for the unmodified one, so encoding this as a bare `\x1bOP` would \
         hand the shell a plain F1 and drop the Alt the reader pressed."
    );
    assert_eq!(
        held(
            'x',
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            }
        )
        .to_bytes(false)
        .as_deref(),
        Some(&b"\x1bx"[..])
    );
    assert_eq!(
        held(
            'c',
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        )
        .to_bytes(false)
        .as_deref(),
        Some(&b"\x03"[..])
    );
    assert_eq!(
        held(
            'x',
            Modifiers {
                meta: true,
                ..Modifiers::NONE
            }
        )
        .to_bytes(false),
        None,
        "a real Meta shortcut belongs to the browser, and the pane sends nothing"
    );
    assert_eq!(
        KeyKind::from_dom_key("Dead"),
        KeyKind::BrowserOwned,
        "a dead key has no named form, so the browser's text services own it"
    );
    assert_eq!(
        browser_owned("Dead").to_bytes(false),
        None,
        "and a key the browser's text services own must reach the shell as nothing"
    );
    assert_eq!(
        KeyChord::printable('x').composing().to_bytes(false),
        None,
        "a key the IME composition owns must reach the shell once, as text"
    );
}

#[test]
fn treats_explicit_and_ctrl_alt_reported_altgraph_as_printable_text() {
    let explicit = held(
        '€',
        Modifiers {
            ctrl: true,
            alt: true,
            ..Modifiers::NONE
        },
    )
    .with_alt_graph(true);
    let represented = held(
        '@',
        Modifiers {
            ctrl: true,
            alt: true,
            ..Modifiers::NONE
        },
    );
    assert!(explicit.is_alt_graph());
    assert!(represented.is_alt_graph());
    assert_eq!(explicit.to_bytes(false).as_deref(), Some("€".as_bytes()));
    assert_eq!(
        represented.to_bytes(false).as_deref(),
        Some(&b"@"[..]),
        "Ctrl+Alt over a printable character is text, not a control byte plus an ESC prefix"
    );
}

#[test]
fn a_composing_or_browser_owned_key_is_never_a_printable_key() {
    assert!(is_terminal_printable_key("a"));
    assert!(is_terminal_printable_key("😀"));
    assert!(!is_terminal_printable_key("Dead"));
    assert!(!is_terminal_printable_key("Process"));
    assert!(!is_terminal_printable_key("Unidentified"));
    assert!(!is_terminal_printable_key("ArrowUp"));
    assert!(!is_terminal_printable_key("ab"));
}

#[test]
fn the_modifier_parameter_is_xterms_shift_alt_ctrl_sum() {
    assert_eq!(modifier_parameter(false, false, false), 1);
    assert_eq!(modifier_parameter(true, false, false), 2);
    assert_eq!(modifier_parameter(false, true, false), 3);
    assert_eq!(modifier_parameter(false, false, true), 5);
    assert_eq!(modifier_parameter(true, true, true), 8);
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

/// Every chord the pane can produce, in both cursor modes.
fn the_panes_key_space() -> Vec<(KeyChord, bool)> {
    let modifier_sets = [
        Modifiers::NONE,
        Modifiers::SHIFT,
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        },
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        Modifiers {
            meta: true,
            ..Modifiers::NONE
        },
    ];
    let named_keys = [
        NamedKey::ArrowUp,
        NamedKey::ArrowDown,
        NamedKey::ArrowRight,
        NamedKey::ArrowLeft,
        NamedKey::Home,
        NamedKey::End,
        NamedKey::Insert,
        NamedKey::Delete,
        NamedKey::PageUp,
        NamedKey::PageDown,
        NamedKey::Function(1),
        NamedKey::Function(5),
        NamedKey::Function(12),
        NamedKey::Enter,
        NamedKey::Backspace,
        NamedKey::Tab,
        NamedKey::Escape,
    ];
    let mut cases = Vec::new();
    for key in named_keys {
        for modifiers in modifier_sets {
            for application in [false, true] {
                cases.push((KeyChord::named(key, modifiers), application));
            }
        }
    }
    for character in ['a', 'A', '1', ' ', '@', '?', '[', 'é', '😀'] {
        for modifiers in modifier_sets {
            let chord = KeyChord::printable(character).with_modifiers(modifiers);
            // Both cursor modes, exactly as the named keys are covered: DECCKM
            // reaches the encoder on every keystroke, so a printable that
            // encoded differently under one flag would slip past a matrix that
            // only ever tried the normal mode.
            for application in [false, true] {
                cases.push((chord.clone(), application));
            }
        }
    }
    cases
}

#[test]
fn the_key_mapper_and_the_input_router_cannot_disagree_about_a_keystroke() {
    let cases = the_panes_key_space();
    assert!(cases.len() > 100, "the pane's key space is not this small");
    let session = "session-key-mapper";
    for (chord, cursor_keys_application) in cases {
        let mapped = chord.to_bytes(cursor_keys_application);
        let mut router = InputRouter::new();
        let Some(bytes) = mapped.clone() else {
            // A key the pane does not own never becomes a batch, and the
            // refusal that says so must not leave a sequence outstanding: the
            // pane correlates what it typed with what came back.
            assert!(router.admit(session, None, Vec::new(), 0).is_err());
            assert_eq!(router.allocated_count(), 1);
            assert!(router.outstanding(session).is_empty());
            continue;
        };
        assert!(!bytes.is_empty(), "a mapped key produced no bytes");
        let admitted = router
            .admit(session, Some("view-1".to_string()), bytes.clone(), 0)
            .expect("a keystroke the mapper produced is admitted for a live lane");
        assert_eq!(
            admitted.bytes, bytes,
            "the router must hold exactly the bytes the mapper produced for {:?}",
            chord.kind
        );
        assert_eq!(admitted.view_id.as_deref(), Some("view-1"));
        let outstanding = router.outstanding(session);
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].bytes, bytes);
        assert!(
            !outstanding[0].started,
            "admission is not dispatch: the router still decides when a batch starts"
        );
    }
}

#[test]
fn a_mapped_keystroke_is_refused_by_the_same_router_once_the_lane_closes() {
    let mut router = InputRouter::new();
    let session = "session-key-mapper-closed";
    let bytes = named(NamedKey::ArrowUp)
        .to_bytes(false)
        .expect("ArrowUp is the pane's key");
    router
        .admit(session, None, bytes.clone(), 0)
        .expect("admitted while the lane is open");
    router.set_phase(session, InputPhase::Closed);
    let refused = router.admit(session, None, bytes, 1);
    assert_eq!(
        refused.expect_err("a closed lane refuses").reason,
        "terminal session is closed"
    );
    assert!(router.outstanding(session).is_empty());
}
