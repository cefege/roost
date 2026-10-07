//! The key-to-bytes tables and the pure rules over them, with no pane and no
//! DOM. `chord` supplies one key's modifiers and calls in here; `dom` is the
//! only module that knows a `KeyboardEvent` exists.
//!
//! The tables are xterm's, not a preference: the pane runs the same
//! application on every platform, and a key that encodes differently on two
//! of them is a bug in the shell behind it. Ports v2's
//! `apps/web/src/client/input/terminalInput.ts` (`terminalKeySequence`,
//! `applyCtrlModifier`, `isTerminalPrintableKey`).

mod kitty;

pub use self::kitty::terminal_key_sequence_for_event;
use crate::input::chord::{KeyChord, KeyKind, NamedKey};

/// The three bytes a focus report is written as under DECSET 1004. The
/// application asked WHICH SURFACE owns the keyboard, and only a real focus
/// transition answers that.
pub const FOCUS_REPORT_IN: &str = "\x1b[I";
/// The focus-lost report, the other half of `FOCUS_REPORT_IN`.
pub const FOCUS_REPORT_OUT: &str = "\x1b[O";

/// Whether a key is one printable code point, astral characters included.
///
/// A key that is not a character is either dead, mid-composition, or
/// unidentified, and all three belong to the browser's text services.
pub fn is_terminal_printable_key(key: &str) -> bool {
    !matches!(key, "Dead" | "Process" | "Unidentified") && key.chars().count() == 1
}

/// The ASCII Ctrl byte for one character, or `None` when it has none.
///
/// Digits and every non-ASCII character are `None` rather than a passthrough:
/// the caller must send nothing at all for a key with no Ctrl form, which is
/// what lets the browser's own shortcut keep working.
fn ctrl_byte(character: char) -> Option<char> {
    let code = match character {
        'A'..='Z' | 'a'..='z' => character as u32 & 0x1f,
        ' ' | '@' => 0x00,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(char::from_u32(code).unwrap_or('\0'))
}

/// One key turned into the bytes a PTY receives, with Ctrl folded in.
///
/// Input that is not a single ASCII character has no Ctrl form and comes back
/// UNCHANGED, so a caller that only wants a Ctrl binding can compare the
/// result with what it passed in.
pub fn apply_ctrl_modifier(data: &str) -> String {
    // The single-unit test is over UTF-16 units, so an astral character — one
    // code point, two units — is not a Ctrl binding and passes through.
    if data.encode_utf16().count() != 1 {
        return data.to_string();
    }
    match data.chars().next().and_then(ctrl_byte) {
        Some(byte) => byte.to_string(),
        None => data.to_string(),
    }
}

/// The xterm modifier parameter for a chord: 1, plus 1/2/4 for shift/alt/ctrl.
pub const fn modifier_parameter(shift: bool, alt: bool, ctrl: bool) -> u32 {
    1 + shift as u32 + 2 * (alt as u32) + 4 * (ctrl as u32)
}

/// A cursor or home/end key, in both cursor modes, with the final byte they
/// share. The two modes differ only in the introducer, so they are one entry.
fn navigation(kind: KeyKind) -> Option<(&'static str, &'static str, char)> {
    let entry = match kind {
        KeyKind::Named(NamedKey::ArrowUp) => ("\x1b[A", "\x1bOA", 'A'),
        KeyKind::Named(NamedKey::ArrowDown) => ("\x1b[B", "\x1bOB", 'B'),
        KeyKind::Named(NamedKey::ArrowRight) => ("\x1b[C", "\x1bOC", 'C'),
        KeyKind::Named(NamedKey::ArrowLeft) => ("\x1b[D", "\x1bOD", 'D'),
        KeyKind::Named(NamedKey::Home) => ("\x1b[H", "\x1bOH", 'H'),
        KeyKind::Named(NamedKey::End) => ("\x1b[F", "\x1bOF", 'F'),
        _ => return None,
    };
    Some(entry)
}

/// The keys xterm writes as `CSI n ~`, and their `n`.
fn tilde_code(kind: KeyKind) -> Option<u32> {
    let code = match kind {
        KeyKind::Named(NamedKey::Insert) => 2,
        KeyKind::Named(NamedKey::Delete) => 3,
        KeyKind::Named(NamedKey::PageUp) => 5,
        KeyKind::Named(NamedKey::PageDown) => 6,
        KeyKind::Named(NamedKey::Function(3)) => 13,
        KeyKind::Named(NamedKey::Function(5)) => 15,
        KeyKind::Named(NamedKey::Function(6)) => 17,
        KeyKind::Named(NamedKey::Function(7)) => 18,
        KeyKind::Named(NamedKey::Function(8)) => 19,
        KeyKind::Named(NamedKey::Function(9)) => 20,
        KeyKind::Named(NamedKey::Function(10)) => 21,
        KeyKind::Named(NamedKey::Function(11)) => 23,
        KeyKind::Named(NamedKey::Function(12)) => 24,
        _ => return None,
    };
    Some(code)
}

/// The four function keys xterm writes as `SS3 x` rather than `CSI n ~`.
fn ss3_final(kind: KeyKind) -> Option<char> {
    let final_byte = match kind {
        KeyKind::Named(NamedKey::Function(1)) => 'P',
        KeyKind::Named(NamedKey::Function(2)) => 'Q',
        KeyKind::Named(NamedKey::Function(4)) => 'S',
        _ => return None,
    };
    Some(final_byte)
}

/// The four keys whose bytes are themselves, with or without the Alt prefix.
fn simple_bytes(kind: KeyKind) -> Option<&'static str> {
    let bytes = match kind {
        KeyKind::Named(NamedKey::Enter) => "\r",
        KeyKind::Named(NamedKey::Backspace) => "\x7f",
        KeyKind::Named(NamedKey::Tab) => "\t",
        KeyKind::Named(NamedKey::Escape) => "\x1b",
        _ => return None,
    };
    Some(bytes)
}

/// The exact bytes one key event sends to the PTY, or `None` when the pane
/// does not own the key.
///
/// `cursor_keys_application` is the worker's DECCKM mode, read per event: a
/// shell that enables application cursor keys mid-session must see its own
/// encoding on the very next keystroke.
pub fn terminal_key_sequence(
    chord: &KeyChord,
    cursor_keys_application: bool,
    kitty_flags: u8,
) -> Option<String> {
    terminal_key_sequence_for_event(
        chord,
        cursor_keys_application,
        kitty_flags,
        super::chord::KeyEventType::Press,
        None,
        super::chord::AlternateKeys::default(),
    )
}

fn legacy_key_sequence(chord: &KeyChord, cursor_keys_application: bool) -> Option<String> {
    if chord.is_composing || matches!(chord.kind, KeyKind::BrowserOwned) {
        return None;
    }
    if chord.is_alt_graph()
        && let KeyKind::Printable(character) = chord.kind
    {
        return Some(character.to_string());
    }
    if (chord.modifiers.meta || chord.modifiers.super_key) && !chord.modifiers.ctrl {
        return None;
    }
    if chord.modifiers.ctrl
        && !chord.modifiers.alt
        && !chord.modifiers.meta
        && !chord.modifiers.super_key
        && let KeyKind::Printable(character) = chord.kind
    {
        return ctrl_byte(character).map(|byte| byte.to_string());
    }
    if chord.kind == KeyKind::Named(NamedKey::Enter) && chord.modifiers.shift {
        return Some(format!(
            "\x1b[13;{}u",
            modifier_parameter(
                chord.modifiers.shift,
                chord.modifiers.alt,
                chord.modifiers.ctrl
            )
        ));
    }
    if chord.kind == KeyKind::Named(NamedKey::Tab)
        && chord.modifiers.shift
        && !chord.modifiers.alt
        && !chord.modifiers.ctrl
    {
        return Some("\x1b[Z".to_string());
    }
    let modifier = modifier_parameter(
        chord.modifiers.shift,
        chord.modifiers.alt,
        chord.modifiers.ctrl,
    );
    if let Some((normal, application, final_byte)) = navigation(chord.kind) {
        return Some(if modifier == 1 {
            if cursor_keys_application {
                application.to_string()
            } else {
                normal.to_string()
            }
        } else {
            format!("\x1b[1;{modifier}{final_byte}")
        });
    }
    if let Some(code) = tilde_code(chord.kind) {
        return Some(if modifier == 1 {
            format!("\x1b[{code}~")
        } else {
            format!("\x1b[{code};{modifier}~")
        });
    }
    if let Some(final_byte) = ss3_final(chord.kind) {
        return Some(if modifier == 1 {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[1;{modifier}{final_byte}")
        });
    }
    if let Some(bytes) = simple_bytes(chord.kind) {
        return Some(if chord.modifiers.alt {
            format!("\x1b{bytes}")
        } else {
            bytes.to_string()
        });
    }
    if let KeyKind::Printable(character) = chord.kind
        && !chord.modifiers.ctrl
        && !chord.modifiers.meta
        && !chord.modifiers.super_key
    {
        return Some(if chord.modifiers.alt {
            format!("\x1b{character}")
        } else {
            character.to_string()
        });
    }
    None
}
