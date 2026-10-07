//! KeyboardEvent adapters for the terminal input controller.
//!
//! This child module owns keydown/keyup translation; its parent owns the textarea
//! and listener lifecycle. Key values, physical codes, and lock state are read per event.

use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{Event, KeyboardEvent};

use crate::input::chord::{AlternateKeys, KeyEventType, Modifiers, NamedKey};
use crate::input::controller::{KeyDownAction, TerminalKeyEvent};

use super::{ControllerShared, focus_without_scroll, select_contents, selection_has_text};

pub(super) fn on_key_down(shared: &Rc<ControllerShared>, event: &Event) {
    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    let key = event.key();
    let event_type = if event.repeat() { KeyEventType::Repeat } else { KeyEventType::Press };
    let key_event = keyboard_event(event, &key, event_type);
    let application = (shared.options.cursor_keys_application)();
    let kitty_flags = (shared.options.kitty_keyboard_flags)();
    let Ok(state) = shared.state.try_borrow() else {
        return;
    };
    let action = state.key_down_with_flags(&key_event, application, kitty_flags, || selection_has_text(&shared.doc));
    drop(state);
    match action {
        KeyDownAction::Browser => {}
        KeyDownAction::FocusForPaste => focus_without_scroll(&shared.textarea),
        KeyDownAction::SelectPane => {
            event.prevent_default();
            select_contents(&shared.doc, &shared.root);
        }
        KeyDownAction::Write(bytes) => {
            event.prevent_default();
            (shared.options.on_data)(&bytes);
        }
        KeyDownAction::Consume => event.prevent_default(),
    }
}

pub(super) fn on_key_up(shared: &Rc<ControllerShared>, event: &Event) {
    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    let key = event.key();
    let key_event = keyboard_event(event, &key, KeyEventType::Release);
    let application = (shared.options.cursor_keys_application)();
    let kitty_flags = (shared.options.kitty_keyboard_flags)();
    if kitty_flags & 2 == 0 || keyup_is_browser_owned(shared, &key_event, kitty_flags) {
        return;
    }
    let Some(bytes) = shared
        .state
        .try_borrow()
        .ok()
        .and_then(|state| state.dispatch_keydown_with_flags(&key_event, application, kitty_flags))
    else {
        return;
    };
    event.prevent_default();
    (shared.options.on_data)(&bytes);
}


fn keyup_is_browser_owned(
    shared: &ControllerShared,
    event: &TerminalKeyEvent<'_>,
    kitty_flags: u8,
) -> bool {
    if kitty_flags & 8 != 0 {
        return false;
    }
    let command = event.modifiers.ctrl || event.modifiers.meta || event.modifiers.super_key;
    (command
        && event.key.eq_ignore_ascii_case("c")
        && selection_has_text(&shared.doc))
        || (command && event.key.eq_ignore_ascii_case("v"))
}
pub(super) fn keyboard_event<'a>(event: &KeyboardEvent, key: &'a str, event_type: KeyEventType) -> TerminalKeyEvent<'a> {
    let modifiers = Modifiers {
        shift: event.shift_key(),
        alt: event.alt_key(),
        ctrl: event.ctrl_key(),
        meta: false,
        super_key: event.meta_key() || event.get_modifier_state("OS"),
        hyper: event.get_modifier_state("Hyper"),
        caps_lock: event.get_modifier_state("CapsLock"),
        num_lock: event.get_modifier_state("NumLock"),
    };
    let code = event.code();
    let alternate_keys = AlternateKeys {
        shifted: modifiers.shift.then(|| key.chars().next()).flatten(),
        base_layout: base_layout_key(&code),
        unshifted: unshifted_key(&event.key(), &code),
    };
    let associated_text = (event_type != KeyEventType::Release
        && key.chars().count() == 1
        && (!modifiers.ctrl && !modifiers.meta && !modifiers.super_key
            || event.get_modifier_state("AltGraph")))
        .then_some(key);
    TerminalKeyEvent {
        key,
        modifiers,
        alt_graph: event.get_modifier_state("AltGraph"),
        is_composing: event.is_composing(),
        event_type,
        associated_text,
        alternate_keys,
        key_override: functional_key(event, &code),
    }
}

fn unshifted_key(key: &str, code: &str) -> Option<char> {
    let character = key.chars().next()?;
    if character.is_ascii_uppercase() {
        return Some(character.to_ascii_lowercase());
    }
    let unshifted = match character {
        '!' => '1', '@' => '2', '#' => '3', '$' => '4', '%' => '5',
        '^' => '6', '&' => '7', '*' => '8', '(' => '9', ')' => '0',
        '_' => '-', '+' => '=', '{' => '[', '}' => ']', '|' => '\\',
        ':' => ';', '"' => char::from_u32(39)?, '<' => ',', '>' => '.', '?' => '/',
        '~' => '`',
        _ => return character.to_lowercase().next(),
    };
    base_layout_key(code).filter(|base| base.is_ascii_punctuation()).or(Some(unshifted))
}

fn functional_key(event: &KeyboardEvent, code: &str) -> Option<NamedKey> {
    let key_code = match code {
        "Numpad0" if event.get_modifier_state("NumLock") => 57399,
        "Numpad1" if event.get_modifier_state("NumLock") => 57400,
        "Numpad2" if event.get_modifier_state("NumLock") => 57401,
        "Numpad3" if event.get_modifier_state("NumLock") => 57402,
        "Numpad4" if event.get_modifier_state("NumLock") => 57403,
        "Numpad5" if event.get_modifier_state("NumLock") => 57404,
        "Numpad6" if event.get_modifier_state("NumLock") => 57405,
        "Numpad7" if event.get_modifier_state("NumLock") => 57406,
        "Numpad8" if event.get_modifier_state("NumLock") => 57407,
        "Numpad9" if event.get_modifier_state("NumLock") => 57408,
        "Numpad0" => 57425,
        "Numpad1" => 57424,
        "Numpad2" => 57420,
        "Numpad3" => 57422,
        "Numpad4" => 57417,
        "Numpad5" => 57427,
        "Numpad6" => 57418,
        "Numpad7" => 57423,
        "Numpad8" => 57419,
        "Numpad9" => 57421,
        "NumpadDecimal" => 57426,
        "NumpadDivide" => 57410,
        "NumpadMultiply" => 57411,
        "NumpadSubtract" => 57412,
        "NumpadAdd" => 57413,
        "NumpadEnter" => 57414,
        "NumpadEqual" => 57415,
        "NumpadComma" => 57416,
        "ShiftLeft" => 57441,
        "ControlLeft" => 57442,
        "AltLeft" => 57443,
        "MetaLeft" => 57444,
        "ShiftRight" => 57447,
        "ControlRight" => 57448,
        "AltRight" => 57449,
        "MetaRight" => 57450,
        "_HyperLeft" => 57445,
        "_MetaLeft" => 57446,
        "_HyperRight" => 57451,
        "_MetaRight" => 57452,
        "AltGraph" => 57453,
        "Level5Shift" => 57454,
        _ => return None,
    };
    Some(NamedKey::Functional(key_code))
}

fn base_layout_key(code: &str) -> Option<char> {
    match code {
        "KeyA" => Some('a'), "KeyB" => Some('b'), "KeyC" => Some('c'), "KeyD" => Some('d'),
        "KeyE" => Some('e'), "KeyF" => Some('f'), "KeyG" => Some('g'), "KeyH" => Some('h'),
        "KeyI" => Some('i'), "KeyJ" => Some('j'), "KeyK" => Some('k'), "KeyL" => Some('l'),
        "KeyM" => Some('m'), "KeyN" => Some('n'), "KeyO" => Some('o'), "KeyP" => Some('p'),
        "KeyQ" => Some('q'), "KeyR" => Some('r'), "KeyS" => Some('s'), "KeyT" => Some('t'),
        "KeyU" => Some('u'), "KeyV" => Some('v'), "KeyW" => Some('w'), "KeyX" => Some('x'),
        "KeyY" => Some('y'), "KeyZ" => Some('z'),
        "Digit0" => Some('0'), "Digit1" => Some('1'), "Digit2" => Some('2'),
        "Digit3" => Some('3'), "Digit4" => Some('4'), "Digit5" => Some('5'),
        "Digit6" => Some('6'), "Digit7" => Some('7'), "Digit8" => Some('8'),
        "Digit9" => Some('9'), "Space" => Some(' '),
        "Minus" => Some('-'), "Equal" => Some('='), "BracketLeft" => Some('['),
        "BracketRight" => Some(']'), "Backslash" => Some('\\'), "Semicolon" => Some(';'),
        "Quote" => Some('\''), "Comma" => Some(','), "Period" => Some('.'),
        "Slash" => Some('/'), "Backquote" => Some('`'),
        _ => None,
    }
}
