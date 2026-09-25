//! The base64url codec the native side of JWT minting and verification uses.
//!
//! Unpadded and URL-safe, because a JWS segment travels in a header and a
//! query string. Decoding is strict where the Node original was lenient: a
//! segment that is not canonical base64url is refused rather than silently
//! resolved to bytes, so one token cannot be spelled two ways and still verify.

use base64::prelude::{BASE64_URL_SAFE_NO_PAD, Engine as _};
use roost_protocol::{ProtocolError, ProtocolResult};

/// Where a rejected segment failed.
const SEGMENT_FIELD: &str = "jwt.base64url_segment";

/// Encode bytes as an unpadded base64url segment.
pub fn b64url_encode(input: &[u8]) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(input)
}

/// Decode an unpadded base64url segment.
pub fn b64url_decode(input: &str) -> ProtocolResult<Vec<u8>> {
    BASE64_URL_SAFE_NO_PAD.decode(input).map_err(|error| {
        ProtocolError::new(SEGMENT_FIELD, format!("invalid base64url segment: {error}"))
    })
}

/// Decode an unpadded base64url segment as UTF-8 text.
pub fn b64url_decode_to_utf8(input: &str) -> ProtocolResult<String> {
    let bytes = b64url_decode(input)?;
    String::from_utf8(bytes).map_err(|error| {
        ProtocolError::new(SEGMENT_FIELD, format!("segment is not UTF-8: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::{b64url_decode, b64url_decode_to_utf8, b64url_encode};

    #[test]
    fn a_jws_segment_is_unpadded_and_url_safe() {
        // 0xFB 0xFF 0xFE is the classic case: standard base64 would emit `+` and
        // `/`, and both would need escaping inside a URL.
        assert_eq!(b64url_encode(&[0xFB, 0xFF, 0xFE]), "-__-");
        assert_eq!(b64url_encode(b"a"), "YQ");
        assert_eq!(b64url_encode(b"ab"), "YWI");
        assert_eq!(b64url_encode(b"abc"), "YWJj");
    }

    #[test]
    fn a_segment_round_trips_through_the_url_safe_alphabet() {
        let payload: &[u8] = br#"{"sub":"d-1","iat":1700000000}"#;
        let encoded = b64url_encode(payload);
        assert!(!encoded.contains('='), "padding is not a JWS character");
        assert_eq!(
            b64url_decode(&encoded).unwrap_or_else(|error| panic!("{error}")),
            payload
        );
    }

    #[test]
    fn a_non_canonical_segment_is_refused_rather_than_resolved() {
        for bad in ["YQ==", "Y WJj", "!!!!", "YWI="] {
            assert!(
                b64url_decode(bad).is_err(),
                "{bad} was accepted as a segment"
            );
        }
    }

    #[test]
    fn text_segments_decode_to_utf8_and_non_utf8_bytes_do_not() {
        assert_eq!(
            b64url_decode_to_utf8("aGVsbG8").unwrap_or_else(|error| panic!("{error}")),
            "hello"
        );
        assert!(b64url_decode_to_utf8(&b64url_encode(&[0xFF, 0xFE])).is_err());
    }
}
