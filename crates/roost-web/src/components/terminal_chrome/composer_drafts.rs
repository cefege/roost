//! The per-session composer draft, kept on THIS device and never on a server:
//! a typed line that must survive a pane switch, a compact/desktop instance
//! swap and a reload is the user's unfinished work, and losing it to a
//! navigation is the one composer failure that loses keystrokes.
//! Ports `apps/web/src/lib/composerDrafts.ts`, including its storage key and its
//! `{ sessionId: text }` shape, so a draft written by either build is read by
//! the other.

use crate::platform::LocalStorageKeyValueStore;
use roost_client_core::KeyValueStore;

/// The one key every draft shares, and the exact name v2 wrote.
pub const DRAFTS_KEY: &str = "roost.composerDrafts.v1";

/// The stored map, or an empty one when storage is unreadable. A private-mode
/// tab throws on the read; a draft store that panicked there would take the
/// composer with it, and a missing draft is a smaller loss than a dead page.
fn read_map() -> Vec<(String, String)> {
    let store = LocalStorageKeyValueStore::new();
    let Some(raw) = store.get(DRAFTS_KEY) else {
        return Vec::new();
    };
    parse_map(&raw)
}

/// Every stored draft, as ordered pairs. A hand walk of the object rather than a
/// JSON value type: the map is `{ sessionId: text }` and nothing else, and a
/// value the operator typed is never worth failing a whole reload over.
fn parse_map(raw: &str) -> Vec<(String, String)> {
    let Some(body) = raw
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return Vec::new();
    };
    let mut drafts = Vec::new();
    for entry in split_top_level(body) {
        let Some(colon) = find_key_end(entry) else {
            continue;
        };
        let key = unquote(&entry[..colon]);
        let value = unquote(entry[colon + 1..].trim_start());
        if !key.is_empty() {
            drafts.push((key, value));
        }
    }
    drafts
}

/// The splits a JSON object's members need, honouring quoted commas and
/// escapes so a draft containing `,` or `"` survives the round trip.
fn split_top_level(body: &str) -> Vec<&str> {
    let mut members = Vec::new();
    let bytes = body.as_bytes();
    let mut start = 0;
    let mut idx = 0;
    let mut quoted = false;
    let mut escaped = false;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == b',' && !quoted {
            members.push(&body[start..idx]);
            start = idx + 1;
        }
        idx += 1;
    }
    if start < body.len() {
        members.push(&body[start..]);
    }
    members
}

/// The index of the `:` that ends a member's key, ignoring the ones inside it.
fn find_key_end(entry: &str) -> Option<usize> {
    let bytes = entry.as_bytes();
    let mut idx = 0;
    let mut escaped = false;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\\' if escaped => escaped = false,
            b'\\' => escaped = true,
            b'"' => {
                // The key is over; the separator is the first `:` from here.
                return entry[idx + 1..].find(':').map(|offset| idx + 1 + offset);
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

/// The text of a JSON string literal, with the escapes a typed draft can carry.
fn unquote(literal: &str) -> String {
    let trimmed = literal.trim();
    let Some(body) = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return trimmed.to_owned();
    };
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The text of a JSON string literal, escaped for writing back.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The draft for `session_id`, or the empty string.
pub fn get_composer_draft(session_id: &str) -> String {
    read_map()
        .into_iter()
        .find(|(key, _)| key == session_id)
        .map(|(_, value)| value)
        .unwrap_or_default()
}

/// Store `text` for `session_id`. The whole map is rewritten, because the store
/// is one key and a partial write would drop another session's draft.
///
/// An empty draft is DELETED rather than stored as an empty string: sending a
/// line is not unfinished work, and a member left behind is a draft that comes
/// back on the next session restore with nothing in it. Every other session's
/// member is left exactly where it was.
pub fn save_composer_draft(session_id: &str, text: &str) {
    let mut drafts = read_map();
    if text.is_empty() {
        drafts.retain(|(key, _)| key != session_id);
    } else if let Some(entry) = drafts.iter_mut().find(|(key, _)| key == session_id) {
        entry.1 = text.to_owned();
    } else {
        drafts.push((session_id.to_owned(), text.to_owned()));
    }
    let store = LocalStorageKeyValueStore::new();
    store.set(DRAFTS_KEY, &render_map(&drafts));
}

/// The serialized map, in insertion order so a stored draft never moves.
fn render_map(drafts: &[(String, String)]) -> String {
    let members = drafts
        .iter()
        .map(|(key, value)| format!("{}:{}", quote(key), quote(value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", members.join(","))
}
