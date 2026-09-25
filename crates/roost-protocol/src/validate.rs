//! The explicit validators that replace Zod's `superRefine` chains.
//!
//! Zod does not exist in Rust, so every limit the TypeScript package enforced
//! in a schema becomes a function here that returns `ProtocolResult`. They
//! are deliberately small and single-purpose so a ported validator reads like
//! the `.refine()` it replaces, and so a caller can compose them the way the
//! schema composed them.

use crate::error::{ProtocolError, ProtocolResult};

/// A `z.string().uuid()` check: 8-4-4-4-12 lowercase-or-uppercase hex.
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

/// A `z.string().regex(/^[0-9a-f]{64}$/)` check: the worker fingerprint.
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

/// An integer within an inclusive range, the shape of every
/// `z.number().int().min(n).max(n)` in the contract.
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

/// A required non-empty string, the shape of every `z.string().min(1)`.
pub fn non_empty(field: &str, value: &str) -> ProtocolResult<()> {
    if value.is_empty() {
        Err(ProtocolError::new(field, "must not be empty"))
    } else {
        Ok(())
    }
}

/// A maximum length counted in **UTF-8 bytes**, which is how every
/// transport-level cap in the contract is defined. A Zod `.max(n)` counted
/// UTF-16 code units, so a port that switches units silently changes the cap
/// for any non-ASCII payload — the byte count is the one the wire enforces.
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

/// The "present together or all absent" rule the agent-status identity fields
/// share, the shape of Zod's `superRefine` on that object.
pub fn all_or_none(field: &str, present: [bool; 3]) -> ProtocolResult<()> {
    let any = present.iter().any(|is_present| *is_present);
    let all = present.iter().all(|is_present| *is_present);
    if any == all {
        Ok(())
    } else {
        Err(ProtocolError::new(
            field,
            "status_epoch, occupant_id, and source must be present together or all absent",
        ))
    }
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

    #[test]
    fn all_or_none_rejects_a_partial_identity() {
        assert!(all_or_none("status", [true, true, true]).is_ok());
        assert!(all_or_none("status", [false, false, false]).is_ok());
        assert!(all_or_none("status", [true, false, true]).is_err());
    }
}
