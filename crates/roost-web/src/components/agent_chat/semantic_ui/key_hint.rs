//! Keycap text for OMP key names, matching the TUI's unicode key hints.
//! Called by every semantic-UI node that draws a `keys` list (button, kbd, tool actions);
//! depends only on the key id strings OMP sends, not on any component.

/// `key-hint-format.ts` `KEY_SYMBOLS` in the unicode symbol preset.
fn key_glyph(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "esc" | "escape" => "⎋",
        "enter" | "return" => "⏎",
        "space" => "␣",
        "tab" => "⇥",
        "backspace" => "⌫",
        "delete" => "⌦",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        _ => return None,
    })
}

/// `key-hint-format.ts` `KEY_WORDS`: keys with no keycap glyph.
fn key_word(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "home" => "Home",
        "end" => "End",
        "pageup" => "PgUp",
        "pagedown" => "PgDn",
        "insert" => "Ins",
        _ => return None,
    })
}

/// Modifier label in display order: (id, generic label, macOS label).
const MODIFIERS: [(&str, &str, &str); 4] = [
    ("ctrl", "Ctrl", "⌃"),
    ("alt", "Alt", "⌥"),
    ("shift", "⇧", "⇧"),
    ("super", "Super", "⌘"),
];

/// Format one key id the way `formatKeyHint` does: `ctrl+o` → `⌃O` (macOS) or `Ctrl+O`,
/// `escape` → `⎋`, a bare letter stays lowercase.
pub(super) fn format_key_hint(key: &str, mac: bool) -> String {
    let mut parts: Vec<&str> = if let Some(stripped) = key.strip_suffix('+') {
        let mut parts: Vec<&str> = stripped.split('+').collect();
        parts.pop();
        parts.push("+");
        parts
    } else {
        key.split('+').collect()
    };
    let base = parts.pop().unwrap_or_default();
    let mut out = String::new();
    for (id, generic, apple) in MODIFIERS {
        if parts.contains(&id) {
            let label = if mac { apple } else { generic };
            out.push_str(label);
            if label.chars().any(|ch| ch.is_ascii_alphanumeric()) {
                out.push('+');
            }
        }
    }
    let lower = base.to_lowercase();
    if let Some((_, generic, apple)) = MODIFIERS.iter().find(|(id, ..)| *id == lower) {
        out.push_str(if mac { apple } else { generic });
    } else if let Some(glyph) = key_glyph(&lower) {
        out.push_str(glyph);
    } else if let Some(word) = key_word(&lower) {
        out.push_str(word);
    } else if base.chars().count() == 1 {
        if parts.is_empty() {
            out.push_str(base);
        } else {
            out.push_str(&base.to_uppercase());
        }
    } else {
        let mut chars = base.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Whether this browser's keyboard uses macOS keycaps.
#[cfg(target_arch = "wasm32")]
fn apple_keyboard() -> bool {
    web_sys::window()
        .and_then(|window| window.navigator().platform().ok())
        .is_some_and(|platform| platform.starts_with("Mac") || platform.starts_with("iP"))
}

#[cfg(not(target_arch = "wasm32"))]
fn apple_keyboard() -> bool {
    false
}

/// Keycap text for a key id on this browser's platform.
pub(super) fn keycap(key: &str) -> String {
    format_key_hint(key, apple_keyboard())
}

#[cfg(test)]
mod tests {
    use super::format_key_hint;

    #[test]
    fn matches_the_tui_unicode_hints() {
        assert_eq!(format_key_hint("escape", true), "⎋");
        assert_eq!(format_key_hint("enter", false), "⏎");
        assert_eq!(format_key_hint("q", true), "q");
        assert_eq!(format_key_hint("ctrl+o", true), "⌃O");
        assert_eq!(format_key_hint("ctrl+o", false), "Ctrl+O");
        assert_eq!(format_key_hint("alt+up", true), "⌥↑");
        assert_eq!(format_key_hint("alt+up", false), "Alt+↑");
        assert_eq!(format_key_hint("shift+ctrl+p", true), "⌃⇧P");
        assert_eq!(format_key_hint("shift", false), "⇧");
        assert_eq!(format_key_hint("f5", false), "F5");
        assert_eq!(format_key_hint("ctrl++", false), "Ctrl++");
        assert_eq!(format_key_hint("pageup", true), "PgUp");
    }
}
