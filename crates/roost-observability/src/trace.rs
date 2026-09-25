//! `trace_id` correlation: every HTTP request, WS frame and log line carries
//! one, so a `grep trace_id=abc12345` reconstructs a single request across
//! coordinator, worker and browser.
//!
//! The header name and the shape live here because this crate is the bottom of
//! the dependency graph and every layer logs through it. The *type* does not:
//! a trace id is a wire value, so `roost_protocol::wire::brand::TraceId` is
//! the one definition of it and this module hands out the rendered string a
//! caller mints. Two types for one wire value is how a request ends up
//! correlated in one log line and untraceable in the next.

use thiserror::Error;

/// The header a trace id travels in.
pub const TRACE_HEADER: &str = "x-roost-trace-id";

/// The bytes behind one trace id: 16 hex characters, plenty unique for a
/// single-operator fleet's logs.
pub const TRACE_ID_BYTES: usize = 8;

/// The shortest id the shape accepts, in hex characters.
pub const TRACE_ID_MIN_LEN: usize = 8;

/// The exact length [`trace_id_from_bytes`] renders.
pub const TRACE_ID_HEX_LEN: usize = TRACE_ID_BYTES * 2;

/// A value that is not a trace id. The rejected value is carried so a caller
/// validating a request header can log what actually arrived.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("value is not a trace id: expected at least 8 hexadecimal characters, got {value:?}")]
pub struct TraceIdError {
    pub value: String,
}

/// Whether a value has the trace id shape: at least eight hexadecimal
/// characters, and nothing else. Case-insensitive, like the wire regex.
pub fn is_trace_id(value: &str) -> bool {
    value.chars().count() >= TRACE_ID_MIN_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Validate a value that arrived from somewhere else, returning the rendered
/// id. The typed id is `roost_protocol::wire::brand::TraceId`; build it from
/// this string with `TraceId::try_from`.
pub fn as_trace_id(value: &str) -> Result<String, TraceIdError> {
    if is_trace_id(value) {
        Ok(value.to_owned())
    } else {
        Err(TraceIdError {
            value: value.to_owned(),
        })
    }
}

/// Render eight bytes of entropy as a trace id. The entropy source is the
/// caller's: this crate has none, and inventing one here would make trace ids
/// forgeable and unrepeatable across processes.
pub fn trace_id_from_bytes(bytes: [u8; TRACE_ID_BYTES]) -> String {
    const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";
    let mut rendered = String::with_capacity(TRACE_ID_HEX_LEN);
    for byte in bytes {
        rendered.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        rendered.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::{
        TRACE_HEADER, TRACE_ID_HEX_LEN, TRACE_ID_MIN_LEN, TraceIdError, as_trace_id, is_trace_id,
        trace_id_from_bytes,
    };

    #[test]
    fn the_header_name_is_the_one_the_whole_fleet_sends() {
        assert_eq!(TRACE_HEADER, "x-roost-trace-id");
    }

    #[test]
    fn eight_random_bytes_render_as_sixteen_lowercase_hex_characters() {
        let id = trace_id_from_bytes([0x00, 0x0f, 0x10, 0x9a, 0xab, 0xcd, 0xef, 0xff]);
        assert_eq!(id, "000f109aabcdefff");
        assert_eq!(id.len(), TRACE_ID_HEX_LEN);
    }

    #[test]
    fn a_rendered_id_validates_against_the_shape() {
        assert_eq!(
            as_trace_id(&trace_id_from_bytes([1, 2, 3, 4, 5, 6, 7, 8])),
            Ok("0102030405060708".to_owned())
        );
    }

    #[test]
    fn the_shape_is_at_least_eight_hex_digits_and_nothing_else() {
        assert!(is_trace_id("abcdef01"));
        assert!(is_trace_id("ABCDEF01"));
        assert!(is_trace_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_trace_id("abcdef0"), "seven is not eight");
        assert!(!is_trace_id("abcdef0g"), "g is not a hex digit");
        assert!(!is_trace_id("abcdef 1"), "a space is not a hex digit");
        assert!(!is_trace_id(""));
        assert_eq!(TRACE_ID_MIN_LEN, 8);
    }

    #[test]
    fn an_invalid_value_is_rejected_with_the_value_it_rejected() {
        let error = as_trace_id("nope").expect_err("not a trace id");
        assert_eq!(
            error,
            TraceIdError {
                value: "nope".to_owned()
            }
        );
        assert!(
            error.to_string().contains("\"nope\""),
            "the rejected value is named: {error}"
        );
    }
}
