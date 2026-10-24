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
        Key::Named(NamedKey::ArrowLeft) if alt => Some(HubKey::AltLeft),
        Key::Named(NamedKey::ArrowRight) if alt => Some(HubKey::AltRight),
        Key::Named(NamedKey::ArrowUp) => Some(HubKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(HubKey::Down),
        Key::Named(NamedKey::ArrowLeft) => Some(HubKey::Left),
        Key::Named(NamedKey::ArrowRight) => Some(HubKey::Right),
        Key::Named(NamedKey::PageUp) => Some(HubKey::PageUp),
        Key::Named(NamedKey::PageDown) => Some(HubKey::PageDown),
        Key::Named(NamedKey::Home) => Some(HubKey::Home),
        Key::Named(NamedKey::End) => Some(HubKey::End),
        Key::Named(NamedKey::Enter) => Some(HubKey::Enter),
        Key::Named(NamedKey::Escape) => Some(HubKey::Escape),
        Key::Named(NamedKey::Tab) if shift => Some(HubKey::ShiftTab),
        Key::Named(NamedKey::Tab) => Some(HubKey::Tab),
        Key::Named(NamedKey::Backspace) => Some(HubKey::Backspace),
        Key::Named(NamedKey::F5) => Some(HubKey::F5),
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
