//! The key-to-bytes tables and the pure rules over them, with no pane and no
//! DOM. `chord` supplies one key's modifiers and calls in here; `dom` is the
//! only module that knows a `KeyboardEvent` exists.
//!
//! The tables are xterm's, not a preference: the pane runs the same
//! application on every platform, and a key that encodes differently on two
//! of them is a bug in the shell behind it.

use crate::input::chord::{KeyChord, KeyKind};

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
        KeyKind::ArrowUp => ("\x1b[A", "\x1bOA", 'A'),
        KeyKind::ArrowDown => ("\x1b[B", "\x1bOB", 'B'),
        KeyKind::ArrowRight => ("\x1b[C", "\x1bOC", 'C'),
        KeyKind::ArrowLeft => ("\x1b[D", "\x1bOD", 'D'),
        KeyKind::Home => ("\x1b[H", "\x1bOH", 'H'),
        KeyKind::End => ("\x1b[F", "\x1bOF", 'F'),
        _ => return None,
    };
    Some(entry)
}

/// The keys xterm writes as `CSI n ~`, and their `n`.
fn tilde_code(kind: KeyKind) -> Option<u32> {
    let code = match kind {
        KeyKind::Insert => 2,
        KeyKind::Delete => 3,
        KeyKind::PageUp => 5,
        KeyKind::PageDown => 6,
        KeyKind::Function(5) => 15,
        KeyKind::Function(6) => 17,
        KeyKind::Function(7) => 18,
        KeyKind::Function(8) => 19,
        KeyKind::Function(9) => 20,
        KeyKind::Function(10) => 21,
        KeyKind::Function(11) => 23,
        KeyKind::Function(12) => 24,
        _ => return None,
    };
    Some(code)
}

/// The four function keys xterm writes as `SS3 x` rather than `CSI n ~`.
fn ss3_final(kind: KeyKind) -> Option<char> {
    let final_byte = match kind {
        KeyKind::Function(1) => 'P',
        KeyKind::Function(2) => 'Q',
        KeyKind::Function(3) => 'R',
        KeyKind::Function(4) => 'S',
        _ => return None,
    };
    Some(final_byte)
}

/// The four keys whose bytes are themselves, with or without the Alt prefix.
fn simple_bytes(kind: KeyKind) -> Option<&'static str> {
    let bytes = match kind {
        KeyKind::Enter => "\r",
        KeyKind::Backspace => "\x7f",
        KeyKind::Tab => "\t",
        KeyKind::Escape => "\x1b",
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
pub fn terminal_key_sequence(chord: &KeyChord, cursor_keys_application: bool) -> Option<String> {
    if chord.is_composing || matches!(chord.kind, KeyKind::BrowserOwned) {
        return None;
    }
    // Windows reports AltGraph either explicitly or as Ctrl+Alt over a
    // printable character. Both are text input, and treating the second form
    // as a Ctrl binding would send a control byte and an ESC prefix for one
    // character of a word.
    if chord.is_alt_graph()
        && let KeyKind::Printable(character) = chord.kind
    {
        return Some(character.to_string());
    }
    // A real Meta shortcut belongs to the browser or the app. The pane's two
    // terminal-specific macOS exceptions are the controller's, not this one's.
    if chord.modifiers.meta && !chord.modifiers.ctrl {
        return None;
    }
    if chord.modifiers.ctrl
        && !chord.modifiers.alt
        && !chord.modifiers.meta
        && let KeyKind::Printable(character) = chord.kind
    {
        return ctrl_byte(character).map(|byte| byte.to_string());
    }
    if chord.kind == KeyKind::Enter && chord.modifiers.shift {
        return Some(format!(
            "\x1b[13;{}u",
            modifier_parameter(
                chord.modifiers.shift,
                chord.modifiers.alt,
                chord.modifiers.ctrl
            )
        ));
    }
    if chord.kind == KeyKind::Tab
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
    {
        return Some(if chord.modifiers.alt {
            format!("\x1b{character}")
        } else {
            character.to_string()
        });
    }
    None
}
