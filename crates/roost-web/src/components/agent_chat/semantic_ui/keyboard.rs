//! Keyboard adaptation for native semantic controls and non-editable command surfaces.
//! Uses the terminal crate's encoder so modified Enter, function keys and IME
//! ownership have the same bytes as OMP's terminal input decoder.
use dioxus::prelude::*;

pub(super) fn input_data(event: &KeyboardEvent) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        use dioxus::web::WebEventExt;
        let native: web_sys::KeyboardEvent = event.data().try_as_web_event()?;
        let chord = roost_web_terminal::input::dom::key_chord_from_event(&native);
        roost_web_terminal::input::terminal_key_sequence(&chord, false, 8)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use roost_web_terminal::input::chord::{KeyChord, KeyKind, Modifiers};
        let modifiers = event.modifiers();
        let chord = KeyChord {
            kind: KeyKind::from_dom_key(&event.key().to_string()),
            modifiers: Modifiers {
                shift: modifiers.shift(),
                alt: modifiers.alt(),
                ctrl: modifiers.ctrl(),
                meta: modifiers.meta(),
                ..Modifiers::NONE
            },
            alt_graph: false,
            is_composing: event.is_composing(),
        };
        roost_web_terminal::input::terminal_key_sequence(&chord, false, 8)
    }
}

pub(super) fn surface_input(event: &KeyboardEvent, modal_open: bool) -> Option<String> {
    let key = event.key();
    if key == Key::Named(NamedKey::Tab) || (modal_open && key == Key::Named(NamedKey::Escape)) {
        return None;
    }
    #[cfg(target_arch = "wasm32")]
    {
        use dioxus::web::WebEventExt;
        use wasm_bindgen::JsCast;
        let native: web_sys::KeyboardEvent = event.data().try_as_web_event()?;
        if native.default_prevented() {
            return None;
        }
        let target = native.target()?.dyn_into::<web_sys::Element>().ok()?;
        if target
            .closest("input, textarea, [contenteditable]")
            .ok()
            .flatten()
            .is_some()
        {
            return None;
        }
        if (key == Key::Named(NamedKey::Enter) || key == Key::Character(" ".into()))
            && target.closest("button, a").ok().flatten().is_some()
        {
            return None;
        }
    }
    input_data(event)
}

/// Whether Backspace or Delete at the caret is still a browser text edit.
/// An empty side of the caret leaves the key to the program: the terminal's
/// picker consumes a Backspace over an empty query as its delete action.
fn caret_edit_delete(backspace: bool, text: &str, cursor: usize) -> bool {
    let length = text.encode_utf16().count();
    if backspace {
        cursor > 0
    } else {
        cursor < length
    }
}

pub(super) fn browser_editing_key(event: &KeyboardEvent, text: &str, cursor: usize) -> bool {
    if event.is_composing() {
        return true;
    }
    let modifiers = event.modifiers();
    match event.key() {
        Key::Named(NamedKey::Backspace) => caret_edit_delete(true, text, cursor),
        Key::Named(NamedKey::Delete) => caret_edit_delete(false, text, cursor),
        Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Home | NamedKey::End) => {
            true
        }
        Key::Character(value) => {
            (!(modifiers.ctrl() || modifiers.meta() || modifiers.alt()))
                || ((modifiers.ctrl() || modifiers.meta())
                    && matches!(value.as_str(), "a" | "c" | "v" | "x"))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::caret_edit_delete;

    #[test]
    fn delete_keys_over_an_empty_caret_side_belong_to_the_program() {
        assert!(!caret_edit_delete(true, "", 0));
        assert!(!caret_edit_delete(false, "", 0));
        assert!(!caret_edit_delete(true, "query", 0));
        assert!(!caret_edit_delete(false, "query", 5));
        assert!(caret_edit_delete(true, "query", 5));
        assert!(caret_edit_delete(false, "query", 0));
        assert!(caret_edit_delete(true, "🦉", 2));
        assert!(!caret_edit_delete(false, "🦉", 2));
    }
}
