//! The store's capacity rule, as pure functions, so the two halves that must
//! both answer it cannot drift. Called by [`super::Store`] and by
//! [`super::database::claims`], and by nothing else.
//!
//! WHY A SEPARATE FILE AND NOT A METHOD. There are two halves: the in-memory
//! [`super::Store`] counts over its own counters, and the durable
//! [`super::database::Journal`] counts over a table. A rule that read either
//! one's state would be a second place the answer lives, and the two would
//! disagree the first time a bound moved — invisibly, until a spawn could not
//! record that it ended. So the state arrives as ARGUMENTS and the rule decides
//! nothing about where it came from.

use super::{DurableEventKind, ReserveError, MAX_PAYLOAD_BYTES, MAX_ROWS};

/// Whether one more claim of `payload_bytes` fits, given what is already used.
///
/// `used_rows` and `used_bytes` are the caller's own totals: stored rows plus
/// live claims, counted however that half counts them. A claim that would
/// exactly fill the store is REFUSED, because it leaves nothing for the next
/// event and the next event is usually a close.
pub fn admission_fits(
    used_rows: usize,
    used_bytes: usize,
    payload_bytes: usize,
) -> Result<(), ReserveError> {
    if used_rows >= MAX_ROWS || used_bytes.saturating_add(payload_bytes) > MAX_PAYLOAD_BYTES {
        return Err(ReserveError::Full {
            rows: used_rows,
            bytes: used_bytes,
            max_rows: MAX_ROWS,
            max_bytes: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(())
}

/// Whether a claim of `kind` for `payload_bytes` is well formed AT ALL, before
/// the caps are consulted.
///
/// Per kind rather than one global number, because the kinds are not
/// interchangeable: a generous bound for a small event wastes capacity a large
/// one needs. A zero-byte claim is refused because it would consume a row and
/// permanently reduce the store's real capacity for nothing.
pub fn claim_is_well_formed(
    kind: DurableEventKind,
    payload_bytes: usize,
) -> Result<(), ReserveError> {
    if payload_bytes == 0 {
        return Err(ReserveError::PayloadNotPositive {
            payload: payload_bytes,
        });
    }
    let limit = kind.payload_limit();
    if payload_bytes > limit {
        return Err(ReserveError::PayloadTooLarge {
            kind,
            payload: payload_bytes,
            limit,
        });
    }
    Ok(())
}
