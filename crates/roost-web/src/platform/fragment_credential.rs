//! The pairing credential a URL fragment carries, and the scrub that removes it.
//!
//! Owned by `platform`, called once by `main` before the router mounts and
//! before any other module that can issue a request, and depended on by nothing
//! else — the ceremony slice reads the captured value back out of session
//! storage.
//!
//! A pairing link is opened as `/pair#pair=<token>`. The fragment is not sent to
//! the server, which is why it is the right place for a bearer at all, and it IS
//! readable by every script on the page, by `window.location` in any error
//! report, and by the next navigation as a `Referer`. So the value is captured
//! into session storage and the address is rewritten before the application graph
//! is even requested.
//!
//! Ported from `apps/web/src/client/auth/fragment-credential.ts`; the contract
//! is `protocol/spec/auth-and-pairing.md`.

use roost_client_core::KeyValueStore as _;

use crate::platform::storage::SessionStorageKeyValueStore;

/// The session-storage key the captured credential is retained under.
pub const CAPTURED_CREDENTIAL_KEY: &str = "roost.fragmentCredential.v1";

/// What a fragment carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentCredential {
    /// No credential-shaped field. The ordinary case.
    None,
    /// A `pair` field that is not exactly one non-empty value: a repeated field
    /// or an empty one. It is scrubbed and NOT retained, so a bad link cannot
    /// fall through to a credential an earlier load kept.
    Invalid,
    /// A pairing token.
    Pair {
        /// The token, verbatim.
        token: String,
    },
}

/// Read the credential out of a fragment, with or without its leading `#`.
///
/// Query-shaped credentials are never accepted. `#pair=` is the only shape the
/// ceremony mints, and accepting `?pair=` too would mean a link pasted into a
/// chat client leaked the token to that client's servers.
pub fn parse_fragment_credential(hash: &str) -> FragmentCredential {
    let body = hash.strip_prefix('#').unwrap_or(hash);
    if !body.contains("pair") {
        return FragmentCredential::None;
    }
    let values: Vec<&str> = body
        .split('&')
        .filter_map(|segment| {
            let (key, value) = segment.split_once('=')?;
            (key == "pair").then_some(value)
        })
        .collect();
    match values.as_slice() {
        [token] if !token.is_empty() => FragmentCredential::Pair {
            token: (*token).to_string(),
        },
        [] => FragmentCredential::None,
        _ => FragmentCredential::Invalid,
    }
}

/// The address with every credential-shaped field removed, from both the query
/// and the fragment.
///
/// Unrelated bytes are NOT normalised: this rewrites the address the reader is
/// looking at, and a normalising rewrite would change a path the router is about
/// to match.
pub fn credential_free_url(pathname: &str, search: &str, hash: &str) -> String {
    format!(
        "{pathname}{}{}",
        strip_pair_field(search, '?'),
        strip_pair_field(hash, '#')
    )
}

/// Remove the `pair` field from one `?`/`#`-prefixed string.
///
/// A string that did not start with the prefix, or that held no `pair` field, is
/// returned as it came in — including the prefix.
fn strip_pair_field(value: &str, prefix: char) -> String {
    if !value.starts_with(prefix) {
        return String::new();
    }
    let body = &value[prefix.len_utf8()..];
    let segments: Vec<&str> = body.split('&').collect();
    let kept: Vec<&str> = segments
        .iter()
        .copied()
        .filter(|segment| segment_key(segment).as_deref() != Some("pair"))
        .collect();
    if kept.len() == segments.len() {
        return value.to_string();
    }
    if kept.is_empty() || kept == [""] {
        return String::new();
    }
    format!("{prefix}{}", kept.join("&"))
}

/// The decoded key of one query segment, or `None` when it has no `=` or its
/// escape is malformed.
fn segment_key(segment: &str) -> Option<String> {
    let (encoded, _value) = segment.split_once('=')?;
    percent_decode(&encoded.replace('+', " "))
}

/// Percent-decoding for a query key, tolerant of a malformed escape.
///
/// A key that does not decode cannot be compared against `pair`, and a fragment
/// carrying a malformed escape is exactly the input a scrub is asked to clean up.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Capture the fragment credential and scrub the address, once per document.
///
/// Returns what was captured. A document with no window has no address to scrub
/// and no credential to capture, which is the case a native test binary and a
/// worker are in.
pub fn capture_and_scrub() -> FragmentCredential {
    let Some(window) = web_sys::window() else {
        return FragmentCredential::None;
    };
    let location = window.location();
    let pathname = location.pathname().unwrap_or_default();
    let search = location.search().unwrap_or_default();
    let hash = location.hash().unwrap_or_default();

    let credential = parse_fragment_credential(&hash);
    let clean = credential_free_url(&pathname, &search, &hash);
    let visible = format!("{pathname}{search}{hash}");
    if clean != visible {
        // Not allowed to fail quietly: a caller that continues into
        // request-issuing code with a bearer still in `location` has leaked it to
        // everything the page touches next.
        replace_address(window, &clean);
    }

    let storage = SessionStorageKeyValueStore::new();
    match &credential {
        FragmentCredential::Pair { token } => storage.set(CAPTURED_CREDENTIAL_KEY, token),
        // A malformed attempt must never fall through to a token an earlier load
        // in this tab retained.
        FragmentCredential::Invalid => storage.remove(CAPTURED_CREDENTIAL_KEY),
        FragmentCredential::None => {}
    }
    credential
}

/// The credential this tab captured, if any.
pub fn captured_credential() -> Option<String> {
    SessionStorageKeyValueStore::new().get(CAPTURED_CREDENTIAL_KEY)
}

/// Forget the captured credential.
pub fn clear_captured_credential() {
    SessionStorageKeyValueStore::new().remove(CAPTURED_CREDENTIAL_KEY);
}

/// Rewrite the address bar without adding a history entry.
fn replace_address(window: web_sys::Window, clean: &str) {
    let history = match window.history() {
        Ok(history) => history,
        Err(_) => {
            tracing::warn!(
                target: "auth",
                "the address still holds a credential: this window has no History"
            );
            return;
        }
    };
    // A null state and an empty title leave the entry the reader arrived on
    // intact, which is what "scrub" means — the back button must not return to a
    // bearer.
    let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(clean));
}

#[cfg(test)]
mod tests {
    use super::{FragmentCredential, credential_free_url, parse_fragment_credential};

    #[test]
    fn a_single_pair_value_is_the_credential() {
        assert_eq!(
            parse_fragment_credential("#pair=abc123"),
            FragmentCredential::Pair {
                token: "abc123".to_string()
            }
        );
        assert_eq!(
            parse_fragment_credential("pair=abc123"),
            FragmentCredential::Pair {
                token: "abc123".to_string()
            }
        );
    }

    #[test]
    fn a_repeated_or_empty_pair_field_is_refused_rather_than_guessed() {
        // Two tokens is ambiguous, and picking one of them is how a link minted
        // for another device gets redeemed against this one.
        assert_eq!(
            parse_fragment_credential("#pair=one&pair=two"),
            FragmentCredential::Invalid
        );
        assert_eq!(
            parse_fragment_credential("#pair="),
            FragmentCredential::Invalid
        );
    }

    #[test]
    fn a_query_shaped_credential_is_never_accepted_as_one() {
        // The fragment is the only shape that never reaches the server. Accepting
        // the query form would accept a bearer every hop has already logged.
        assert_eq!(
            parse_fragment_credential("?pair=abc123"),
            FragmentCredential::None
        );
    }

    #[test]
    fn a_field_that_only_mentions_pair_is_not_a_credential() {
        assert_eq!(
            parse_fragment_credential("#repairs=2&pairish=1"),
            FragmentCredential::None
        );
    }

    #[test]
    fn scrubbing_removes_the_field_from_both_ends_and_keeps_the_rest() {
        assert_eq!(
            credential_free_url("/pair", "", "#pair=secret&tab=2"),
            "/pair#tab=2"
        );
    }

    #[test]
    fn scrubbing_leaves_an_address_with_no_credential_byte_identical() {
        // The rewrite is not a normaliser: a path the router is about to match
        // must come out the other side unchanged.
        assert_eq!(
            credential_free_url("/s/abc%2Fdef", "?x=1", "#anchor"),
            "/s/abc%2Fdef?x=1#anchor"
        );
        assert_eq!(credential_free_url("/", "", ""), "/");
    }

    #[test]
    fn a_percent_encoded_key_is_still_recognised_as_the_credential() {
        assert_eq!(credential_free_url("/pair", "", "#%70air=secret"), "/pair");
    }

    #[test]
    fn a_query_shaped_credential_is_still_scrubbed_from_the_address() {
        // Even though it is not accepted as a credential, it is still removed, or
        // a bearer pasted as a query string stays in the address and rides out as
        // a `Referer`.
        assert_eq!(credential_free_url("/pair", "?pair=secret", ""), "/pair");
    }
}
