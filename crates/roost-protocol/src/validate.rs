//! The explicit validators every wire limit is written as.
//!
//! Every limit is a function that returns `ProtocolResult`, deliberately small
//! and single-purpose so a caller can compose them in order.

use crate::error::{ProtocolError, ProtocolResult};

/// A UUID check: 8-4-4-4-12 lowercase-or-uppercase hex.
pub fn uuid(field: &str, value: &str) -> ProtocolResult<()> {
    let bytes = value.as_bytes();
    let shape = matches!(bytes.len(), 36)
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        });
    if shape {
        Ok(())
    } else {
        Err(ProtocolError::new(field, "must be a UUID"))
    }
}

/// A fixed-length hex check, the shape of the worker fingerprint.
pub fn hex_of_len(field: &str, value: &str, len: usize) -> ProtocolResult<()> {
    if value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ProtocolError::new(
            field,
            format!("must be {len} lowercase hex characters"),
        ))
    }
}

/// An integer within an inclusive range, the shape of every bounded integer in
/// the contract.
pub fn integer_in_range(field: &str, value: i64, low: i64, high: i64) -> ProtocolResult<()> {
    if (low..=high).contains(&value) {
        Ok(())
    } else {
        Err(ProtocolError::new(
            field,
            format!("must be an integer in {low}..={high}, got {value}"),
        ))
    }
}

/// A non-negative integer, the shape of every channel and count.
pub fn nonnegative(field: &str, value: i64) -> ProtocolResult<()> {
    integer_in_range(field, value, 0, i64::MAX)
}

/// A required non-empty string.
pub fn non_empty(field: &str, value: &str) -> ProtocolResult<()> {
    if value.is_empty() {
        Err(ProtocolError::new(field, "must not be empty"))
    } else {
        Ok(())
    }
}

/// A maximum length counted in **UTF-8 bytes**, which is how every
/// transport-level cap in the contract is defined. A JavaScript length counts
/// UTF-16 code units, so a validator that switches units silently changes the
/// cap for any non-ASCII payload — the byte count is the one the wire enforces.
pub fn max_utf8_bytes(field: &str, value: &str, max: usize) -> ProtocolResult<()> {
    let actual = value.len();
    if actual <= max {
        Ok(())
    } else {
        Err(ProtocolError::new(
            field,
            format!("must not exceed {max} UTF-8 bytes, got {actual}"),
        ))
    }
}

/// A string matching one of a fixed set, the shape of every `z.enum([...])`.
pub fn one_of<'a>(field: &str, value: &str, allowed: &[&'a str]) -> ProtocolResult<&'a str> {
    allowed
        .iter()
        .copied()
        .find(|candidate| *candidate == value)
        .ok_or_else(|| {
            ProtocolError::new(
                field,
                format!("must be one of [{}], got {value:?}", allowed.join(", ")),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_accepts_either_case_and_rejects_a_short_string() {
        assert!(uuid("id", "00000000-0000-4000-8000-000000000101").is_ok());
        assert!(uuid("id", "00000000-0000-4000-8000-00000000010A").is_ok());
        assert!(uuid("id", "not-a-uuid").is_err());
        assert!(uuid("id", "00000000-0000-4000-8000-0000000001011").is_err());
    }

    #[test]
    fn fingerprint_hex_is_exactly_sixty_four_lowercase_hex() {
        let good = "a".repeat(64);
        assert!(hex_of_len("worker_fp", &good, 64).is_ok());
        assert!(hex_of_len("worker_fp", &"a".repeat(63), 64).is_err());
        assert!(hex_of_len("worker_fp", &format!("{}g", "a".repeat(63)), 64).is_err());
    }

    #[test]
    fn utf8_cap_counts_bytes_not_characters() {
        // Four 3-byte characters are twelve bytes: inside a 10-byte cap by
        // character count, outside it by the byte count the wire enforces.
        let value = "一二三四";
        assert_eq!(value.chars().count(), 4);
        assert!(max_utf8_bytes("value", value, 12).is_ok());
        assert!(max_utf8_bytes("value", value, 10).is_err());
    }
}
