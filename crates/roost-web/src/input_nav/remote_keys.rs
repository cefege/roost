//! Which keydowns from a TV remote mean a controller press: Back means the
//! pad's B (leave the key tray, the terminal input, or the slide-over
//! sidebar), and OK on the terminal box means the pad's A (open the key tray).
//! Pure; the listener is `remote_keys_dom`, the meaning is `pad_router`.

/// webOS reports its remote's Back button with this `keyCode` and no `key`.
pub const WEBOS_BACK_KEY_CODE: u32 = 461;
/// Tizen reports its remote's Back button with this `keyCode`.
pub const TIZEN_BACK_KEY_CODE: u32 = 10009;

/// The `key` names TV browsers give a remote's Back button.
const BACK_KEY_NAMES: [&str; 4] = ["GoBack", "BrowserBack", "XF86Back", "Back"];

/// A remote key the TV shell claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKey {
    /// The remote's Back button.
    Back,
    /// OK / Enter: claimed only on the terminal box.
    Ok,
}

/// Classify one keydown. A modified key is never a remote press, so a
/// keyboard chord on a TV-mode desktop keeps its meaning.
pub fn classify_remote_key(key: &str, key_code: u32, modified: bool) -> Option<RemoteKey> {
    if modified {
        return None;
    }
    if BACK_KEY_NAMES.contains(&key)
        || key_code == WEBOS_BACK_KEY_CODE
        || key_code == TIZEN_BACK_KEY_CODE
    {
        return Some(RemoteKey::Back);
    }
    (key == "Enter").then_some(RemoteKey::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_back_button_is_back() {
        for key in BACK_KEY_NAMES {
            assert_eq!(classify_remote_key(key, 0, false), Some(RemoteKey::Back));
        }
        assert_eq!(
            classify_remote_key("Unidentified", 461, false),
            Some(RemoteKey::Back)
        );
        assert_eq!(classify_remote_key("", 10009, false), Some(RemoteKey::Back));
    }

    #[test]
    fn escape_and_backspace_stay_terminal_keys() {
        assert_eq!(classify_remote_key("Escape", 27, false), None);
        assert_eq!(classify_remote_key("Backspace", 8, false), None);
    }

    #[test]
    fn a_modified_key_is_never_a_remote_press() {
        assert_eq!(classify_remote_key("Enter", 13, true), None);
        assert_eq!(classify_remote_key("GoBack", 0, true), None);
        assert_eq!(classify_remote_key("Enter", 13, false), Some(RemoteKey::Ok));
    }
}
