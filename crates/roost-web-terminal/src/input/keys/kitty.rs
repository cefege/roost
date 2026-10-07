//! Kitty progressive keyboard encoding, kept separate from the legacy key table.
//!
//! `keys` owns the flag-zero byte contract; this module emits enhanced events
//! using the active flags and the same key vocabulary.

use super::super::chord::{AlternateKeys, KeyChord, KeyEventType, KeyKind, Modifiers, NamedKey};
use super::super::keys::{legacy_key_sequence, navigation, ss3_final, tilde_code};

/// Encode a browser key event with kitty progressive enhancements.
pub fn terminal_key_sequence_for_event(
    chord: &KeyChord,
    cursor_keys_application: bool,
    kitty_flags: u8,
    event_type: KeyEventType,
    associated_text: Option<&str>,
    alternate_keys: AlternateKeys,
) -> Option<String> {
    let all_keys = kitty_flags & 8 != 0;
    let event_reporting = kitty_flags & 2 != 0;
    if event_type == KeyEventType::Release && !event_reporting {
        return None;
    }
    if kitty_flags & 3 == 0 && !all_keys {
        return legacy_key_sequence(chord, cursor_keys_application);
    }
    if chord.is_composing || matches!(chord.kind, KeyKind::BrowserOwned) {
        return None;
    }
    if event_type == KeyEventType::Release
        && event_reporting
        && !all_keys
        && matches!(chord.kind, KeyKind::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace))
    {
        return None;
    }

    let legacy_control_key = matches!(
        chord.kind,
        KeyKind::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace)
    ) && !chord.modifiers.super_key
        && !chord.modifiers.meta
        && !chord.modifiers.hyper
        && !chord.modifiers.caps_lock
        && !chord.modifiers.num_lock;
    if !all_keys && legacy_control_key {
        return legacy_key_sequence(chord, cursor_keys_application);
    }

    if let Some(bytes) = functional_sequence(
        chord,
        cursor_keys_application,
        kitty_flags,
        event_type,
    ) {
        return Some(bytes);
    }

    let is_text_key = matches!(chord.kind, KeyKind::Printable(_));
    let report_key = all_keys
        || (!is_text_key && kitty_flags & 3 != 0)
        || (kitty_flags & 1 != 0 && disambiguate_key(chord));
    if !report_key {
        if event_type == KeyEventType::Release {
            return None;
        }
        return legacy_key_sequence(chord, cursor_keys_application);
    }

    let code = kitty_key_code(chord.kind, alternate_keys.unshifted)?;
    let shifted = (kitty_flags & 4 != 0)
        .then_some(alternate_keys.shifted)
        .flatten()
        .filter(|_| chord.modifiers.shift);
    let base_layout = (kitty_flags & 4 != 0)
        .then_some(alternate_keys.base_layout)
        .flatten();
    let modifiers = kitty_modifier_parameter(chord.modifiers);
    let mut output = format!("\x1b[{code}");
    if shifted.is_some() || base_layout.is_some() {
        output.push(':');
        if let Some(character) = shifted {
            output.push_str(&(character as u32).to_string());
        }
        if let Some(character) = base_layout {
            output.push(':');
            output.push_str(&(character as u32).to_string());
        }
    }
    if all_keys || modifiers != 1 || event_reporting {
        output.push(';');
        output.push_str(&modifiers.to_string());
        if event_reporting {
            output.push(':');
            output.push_str(event_type.parameter());
        }
    }
    if kitty_flags & 16 != 0 && all_keys {
        append_associated_text(&mut output, associated_text.unwrap_or_default());
    }
    output.push('u');
    Some(output)
}

fn functional_sequence(
    chord: &KeyChord,
    cursor_keys_application: bool,
    kitty_flags: u8,
    event_type: KeyEventType,
) -> Option<String> {
    let all_keys = kitty_flags & 8 != 0;
    let event_reporting = kitty_flags & 2 != 0;
    let modifiers = kitty_modifier_parameter(chord.modifiers);
    let include_parameter = all_keys || event_reporting || modifiers != 1;
    let parameter = if event_reporting {
        format!("{modifiers}:{}", event_type.parameter())
    } else {
        modifiers.to_string()
    };

    if let Some((normal, application, final_byte)) = navigation(chord.kind) {
        if !include_parameter {
            return Some(if cursor_keys_application { application.to_owned() } else { normal.to_owned() });
        }
        return Some(format!("\x1b[1;{parameter}{final_byte}"));
    }
    if let Some(code) = tilde_code(chord.kind) {
        if !include_parameter {
            return Some(format!("\x1b[{code}~"));
        }
        return Some(format!("\x1b[{code};{parameter}~"));
    }
    if let Some(final_byte) = ss3_final(chord.kind) {
        if !include_parameter {
            return Some(format!("\x1bO{final_byte}"));
        }
        return Some(format!("\x1b[1;{parameter}{final_byte}"));
    }
    None
}

fn disambiguate_key(chord: &KeyChord) -> bool {
    match chord.kind {
        KeyKind::Named(NamedKey::Escape | NamedKey::Functional(_)) => true,
        KeyKind::Printable(character) => {
            character.is_ascii()
                && (chord.modifiers.alt
                    || chord.modifiers.ctrl
                    || chord.modifiers.meta
                    || chord.modifiers.super_key)
        }
        _ => false,
    }
}

fn kitty_modifier_parameter(modifiers: Modifiers) -> u32 {
    1 + modifiers.shift as u32
        + 2 * modifiers.alt as u32
        + 4 * modifiers.ctrl as u32
        + 8 * modifiers.super_key as u32
        + 16 * modifiers.hyper as u32
        + 32 * modifiers.meta as u32
        + 64 * modifiers.caps_lock as u32
        + 128 * modifiers.num_lock as u32
}

pub(super) fn kitty_key_code(kind: KeyKind, unshifted: Option<char>) -> Option<u32> {
    let code = match kind {
        KeyKind::Printable(character) => unshifted.unwrap_or(character.to_lowercase().next()?) as u32,
        KeyKind::Named(NamedKey::Escape) => 27,
        KeyKind::Named(NamedKey::Enter) => 13,
        KeyKind::Named(NamedKey::Tab) => 9,
        KeyKind::Named(NamedKey::Backspace) => 127,
        KeyKind::Named(NamedKey::Function(number @ 13..=35)) => 57363 + number as u32,
        KeyKind::Named(NamedKey::Functional(code)) => code as u32,
        _ => return None,
    };
    Some(code)
}

fn append_associated_text(output: &mut String, text: &str) {
    if text.is_empty() || text.chars().any(is_control_code) {
        return;
    }
    output.push(';');
    for (index, character) in text.chars().enumerate() {
        if index != 0 {
            output.push(':');
        }
        output.push_str(&(character as u32).to_string());
    }
}

fn is_control_code(character: char) -> bool {
    let code = character as u32;
    code < 0x20 || (0x7f..=0x9f).contains(&code)
}

impl KeyEventType {
    const fn parameter(self) -> &'static str {
        match self {
            Self::Press => "1",
            Self::Repeat => "2",
            Self::Release => "3",
        }
    }
}
