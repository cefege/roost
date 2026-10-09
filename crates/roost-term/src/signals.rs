//! Program-to-operator signals a core parses from live output: a progress
//! report (OSC 9;4), a desktop notification (OSC 9 / OSC 777;notify) and the
//! shell's published user variables (OSC 1337 SetUserVar). The adapter in
//! `rio/` fills them; the worker's terminal-metadata stage drains them. The
//! caps here bound what one program can push down the metadata lane.

/// Characters kept of a notification's title or body.
pub const NOTIFICATION_TEXT_MAX_CHARS: usize = 256;
/// Notifications kept from one parse; the rest of a burst is dropped.
pub const NOTIFICATIONS_PER_PARSE_MAX: usize = 8;
/// User variables a session publishes; the alphabetically first are kept.
pub const USER_VARS_MAX: usize = 8;
/// Characters kept of a user variable's name.
pub const USER_VAR_KEY_MAX_CHARS: usize = 32;
/// Characters kept of a user variable's value.
pub const USER_VAR_VALUE_MAX_CHARS: usize = 64;

pub use roost_protocol::terminal_signals::{
    TerminalNotification, TerminalProgress, TerminalUserVar,
};

/// `text` cut to at most `max` characters.
pub fn truncated(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The bounded, sorted user variables of a raw map.
pub fn bounded_user_vars<'a>(
    vars: impl Iterator<Item = (&'a String, &'a String)>,
) -> Vec<TerminalUserVar> {
    let mut bounded: Vec<TerminalUserVar> = vars
        .map(|(key, value)| TerminalUserVar {
            key: truncated(key, USER_VAR_KEY_MAX_CHARS),
            value: truncated(value, USER_VAR_VALUE_MAX_CHARS),
        })
        .collect();
    bounded.sort();
    bounded.truncate(USER_VARS_MAX);
    bounded
}
