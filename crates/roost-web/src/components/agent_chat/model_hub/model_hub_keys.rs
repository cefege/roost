//! Translates a browser keyboard event into the hub's `HubKey`, so the pure
//! ladder in `hub_keys` never sees Dioxus types. Alt and Ctrl are read here
//! because the ladder's kind-tab and pane keys depend on them.

use dioxus::prelude::*;

use super::hub_keys::HubKey;

/// The hub key for this event, or `None` when the key is not one the hub reads.
pub fn to_hub_key(event: &KeyboardEvent) -> Option<HubKey> {
    let modifiers = event.modifiers();
    let alt = modifiers.contains(Modifiers::ALT);
    let shift = modifiers.contains(Modifiers::SHIFT);
    let blocked = modifiers.contains(Modifiers::CONTROL) || modifiers.contains(Modifiers::META);
    match event.key() {
        Key::ArrowLeft if alt => Some(HubKey::AltLeft),
        Key::ArrowRight if alt => Some(HubKey::AltRight),
        Key::ArrowUp => Some(HubKey::Up),
        Key::ArrowDown => Some(HubKey::Down),
        Key::ArrowLeft => Some(HubKey::Left),
        Key::ArrowRight => Some(HubKey::Right),
        Key::PageUp => Some(HubKey::PageUp),
        Key::PageDown => Some(HubKey::PageDown),
        Key::Home => Some(HubKey::Home),
        Key::End => Some(HubKey::End),
        Key::Enter => Some(HubKey::Enter),
        Key::Escape => Some(HubKey::Escape),
        Key::Tab if shift => Some(HubKey::ShiftTab),
        Key::Tab => Some(HubKey::Tab),
        Key::Backspace => Some(HubKey::Backspace),
        Key::F5 => Some(HubKey::F5),
        Key::Character(text) if !alt && !blocked => {
            let mut characters = text.chars();
            match (characters.next(), characters.next()) {
                (Some(letter), None) => Some(HubKey::Char(letter)),
                _ => None,
            }
        }
        _ => None,
    }
}
