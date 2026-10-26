//! Port of `packages/ai/src/utils/uuid.ts` @ pi `9841914` (v0.85.0+,
//! `ef3786544`: external timestamps for follower ids + 41-bit sequence).
//!
//! Intentional differences: `crypto.getRandomValues` has no equivalent in the
//! dependency baseline (no `rand`/`uuid` crate, coding-standards appendix A),
//! so random bytes come from a process-wide non-security xorshift64* PRNG —
//! the same class as the upstream `Math.random()` fallback path — seeded from
//! system time (nanos), process id, and a global counter. UUIDs are
//! identifiers, not secrets; this matches the precedent in
//! `utils/provider_retry.rs`.
//!
//! Sequence semantics (upstream uuid.ts:2-28): the 41-bit sequence is a
//! global monotonic counter that never resets on timestamp change — ordering
//! comes from `timestamp(48) | sequence(41) | random`. Ordinary calls clamp
//! the timestamp to `max(now, last_ordinary)` so clock jitter cannot reorder
//! ids; supplied timestamps (follower ids) are used verbatim and do not touch
//! the ordinary-watermark or force sequence monotonicity. Sequence
//! exhaustion raises upstream's `RangeError` — kept as `Err`, never a silent
//! wrap.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// `MAX_UUID_V7_TIMESTAMP` (48-bit field).
pub const MAX_UUID_V7_TIMESTAMP: u64 = 0xffffffffffff;
/// `MAX_SEQUENCE = (1n << 41n) - 1n`.
pub const MAX_SEQUENCE: u64 = (1u64 << 41) - 1;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Non-security PRNG byte source (`crypto.getRandomValues` upstream).
fn fill_random_bytes(bytes: &mut [u8]) {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut state = STATE.load(Ordering::Relaxed);
    if state == 0 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e3779b97f4a7c15);
        let seed = (nanos ^ ((std::process::id() as u64) << 32).rotate_left(7)) | 1;
        state = match STATE.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed) {
            // Won the race: this thread's seed is now the state.
            Ok(_) => seed,
            // Lost the race: use the seed another thread installed.
            Err(current) => current,
        };
    }
    for chunk in bytes.chunks_mut(8) {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let next = state.wrapping_mul(0x2545F4914F6CDD1D);
        STATE.store(state, Ordering::Relaxed);
        let raw = next.to_le_bytes();
        chunk.copy_from_slice(&raw[..chunk.len()]);
    }
}

struct V7State {
    /// Watermark for ordinary (no-timestamp) calls only.
    last_ordinary_timestamp: u64,
    /// Global monotonic sequence; `None` until the first call seeds it.
    sequence: Option<u64>,
}

static V7_STATE: Mutex<V7State> = Mutex::new(V7State {
    last_ordinary_timestamp: 0,
    sequence: None,
});

fn to_hex(bytes: &[u8; 16]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    )
}

/// Generate a time-ordered UUIDv7 (`uuidv7` upstream). `timestamp_ms: None`
/// uses the wall clock (clamped to the ordinary watermark); `Some(ts)` keeps
/// the supplied timestamp verbatim for deterministic follower ids.
///
/// Mirrors the upstream `RangeError`s: an out-of-range timestamp and
/// sequence exhaustion both return `Err` (never a silent wrap).
pub fn uuidv7(timestamp_ms: Option<u64>) -> Result<String, String> {
    let requested_timestamp = timestamp_ms.unwrap_or_else(now_ms);
    if requested_timestamp > MAX_UUID_V7_TIMESTAMP {
        return Err(format!(
            "UUIDv7 timestamp must be an integer between 0 and {MAX_UUID_V7_TIMESTAMP}"
        ));
    }

    let mut random = [0u8; 16];
    fill_random_bytes(&mut random);

    let mut state = match V7_STATE.lock() {
        Ok(guard) => guard,
        // Poisoned mutex: a panicking thread must not take down id generation.
        Err(poisoned) => poisoned.into_inner(),
    };
    // `Math.max(requestedTimestamp, lastOrdinaryTimestamp)` for ordinary
    // calls; supplied timestamps bypass the watermark entirely.
    let effective_timestamp = if timestamp_ms.is_none() {
        let effective = requested_timestamp.max(state.last_ordinary_timestamp);
        state.last_ordinary_timestamp = effective;
        effective
    } else {
        requested_timestamp
    };
    let sequence = match state.sequence {
        None => {
            // 40 bits from random bytes[1..6] (upstream init; the counter can
            // still grow into the full 41-bit range).
            let seeded = (u64::from(random[1]) << 32)
                | (u64::from(random[2]) << 24)
                | (u64::from(random[3]) << 16)
                | (u64::from(random[4]) << 8)
                | u64::from(random[5]);
            state.sequence = Some(seeded);
            seeded
        }
        Some(MAX_SEQUENCE) => {
            return Err("UUIDv7 generator sequence exhausted".to_owned());
        }
        Some(sequence) => {
            let next = sequence + 1;
            state.sequence = Some(next);
            next
        }
    };
    drop(state);

    let timestamp = effective_timestamp;
    let mut bytes = [0u8; 16];
    for index in (0..6).rev() {
        bytes[index] = (timestamp >> ((5 - index) * 8)) as u8;
    }
    bytes[6] = 0x70 | ((sequence >> 37) as u8 & 0x0f);
    bytes[7] = (sequence >> 29) as u8;
    bytes[8] = 0x80 | ((sequence >> 23) as u8 & 0x3f);
    bytes[9] = (sequence >> 15) as u8;
    bytes[10] = (sequence >> 7) as u8;
    bytes[11] = (((sequence & 0x7f) as u8) << 1) | (random[11] & 0x01);
    bytes[12..16].copy_from_slice(&random[12..16]);

    Ok(to_hex(&bytes))
}

/// Ordinary-call convenience: wall-clock timestamp. Sequence exhaustion is
/// unreachable in practice within one millisecond (2^41 ids); a panic here is
/// the honest failure mode, matching an unhandled upstream `RangeError`.
pub fn uuidv7_now() -> String {
    uuidv7(None).expect("UUIDv7 generator sequence exhausted")
}

/// Generate a random UUIDv4 (`randomUUID` from node:crypto upstream, used by
/// the coding-agent session entry id generator).
pub fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    fill_random_bytes(&mut bytes);
    bytes[6] = 0x40 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    to_hex(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test that calls [`uuidv7`] (or pokes `V7_STATE`):
    /// the generator state is process-global and the sequence-exhaustion
    /// test deliberately fills it to `MAX_SEQUENCE`, so parallel test threads
    /// otherwise observe a poisoned window (intermittent ordering/exhaustion
    /// failures unrelated to the code under test).
    static TEST_SERIALIZE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const UUID_V7_RE_CHAR_CHECK: fn(&str) -> bool = |id: &str| {
        // ^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$
        let parts: Vec<&str> = id.split('-').collect();
        if parts.len() != 5 {
            return false;
        }
        let hex = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        };
        parts[0].len() == 8
            && hex(parts[0])
            && parts[1].len() == 4
            && hex(parts[1])
            && parts[2].len() == 4
            && parts[2].starts_with('7')
            && hex(parts[2])
            && parts[3].len() == 4
            && matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b'))
            && hex(parts[3])
            && parts[4].len() == 12
            && hex(parts[4])
    };

    #[test]
    fn uuidv7_matches_upstream_shape() {
        let _guard = TEST_SERIALIZE.lock().expect("test lock");
        let id = uuidv7(None).expect("uuid");
        assert!(UUID_V7_RE_CHAR_CHECK(&id), "not a uuidv7: {id}");
    }

    #[test]
    fn uuidv7_is_time_ordered() {
        let _guard = TEST_SERIALIZE.lock().expect("test lock");
        let a = uuidv7(None).expect("uuid");
        let b = uuidv7(None).expect("uuid");
        assert!(a <= b, "uuidv7 must be monotonic: {a} then {b}");
    }

    #[test]
    fn uuidv7_external_timestamp_is_deterministic_and_monotonic() {
        let _guard = TEST_SERIALIZE.lock().expect("test lock");
        // Same supplied timestamp: sequence increments → lexicographic order.
        let a = uuidv7(Some(1_700_000_000_000)).expect("uuid");
        let b = uuidv7(Some(1_700_000_000_000)).expect("uuid");
        assert!(a < b, "same-timestamp ids must order by sequence: {a} {b}");
        // Timestamp bits land in the prefix (millis → first 6 bytes).
        assert!(a.starts_with("018bcfe5"), "timestamp prefix: {a}");
    }

    #[test]
    fn uuidv7_rejects_out_of_range_timestamp() {
        let _guard = TEST_SERIALIZE.lock().expect("test lock");
        let error = uuidv7(Some(MAX_UUID_V7_TIMESTAMP + 1)).expect_err("out of range");
        assert_eq!(
            error,
            format!("UUIDv7 timestamp must be an integer between 0 and {MAX_UUID_V7_TIMESTAMP}")
        );
        // Boundary itself is fine.
        uuidv7(Some(MAX_UUID_V7_TIMESTAMP)).expect("boundary ok");
    }

    #[test]
    fn uuidv7_sequence_exhaustion_errors_instead_of_wrapping() {
        let _guard = TEST_SERIALIZE.lock().expect("test lock");
        let mut state = V7_STATE.lock().expect("state");
        state.sequence = Some(MAX_SEQUENCE);
        drop(state);
        let error = uuidv7(Some(1_700_000_000_000)).expect_err("exhausted");
        assert_eq!(error, "UUIDv7 generator sequence exhausted");
        // Restore a fresh state so later tests are unaffected.
        let mut state = V7_STATE.lock().expect("state");
        state.sequence = None;
        state.last_ordinary_timestamp = 0;
    }

    #[test]
    fn uuidv7_sequence_survives_timestamp_change_without_reset() {
        let _guard = TEST_SERIALIZE.lock().expect("test lock");
        // The 41-bit sequence never resets on a new millisecond (upstream
        // ef3786544): two calls straddling a timestamp bump still increment.
        let a = uuidv7(Some(1_700_000_000_000)).expect("uuid");
        let b = uuidv7(Some(1_700_000_000_001)).expect("uuid");
        assert!(a < b, "ts+seq must order: {a} {b}");
    }

    #[test]
    fn random_uuid_shape_and_uniqueness() {
        let a = random_uuid();
        let b = random_uuid();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4');
    }
}
