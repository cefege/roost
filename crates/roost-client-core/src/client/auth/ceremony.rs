//! The pairing ceremony's portable values: version, entropy, and the canonical
//! validation every end applies.
//!
//! Three values move between a requester, an approver and the coordinator — a
//! 16-byte request id, a 32-byte requester token, and a six-digit verification
//! code — and all three are generated in the browser. So the generators and the
//! validators live together, and the coordinator, which only ever hashes what it
//! is given, can never be the thing that decides what a well-formed value is.
//!
//! The verification code is drawn by REJECTION, not by modulo. `u32 % 1_000_000`
//! is biased toward the low codes by 294 of every 4.3 billion draws, and a
//! pairing code is a six-digit secret a human reads aloud, so the bias would put
//! a measurable edge on guessing it within the attempt limit.
//!
//! Ported from `packages/protocol/src/pairing.ts`; the limits are in
//! `protocol/spec/auth-and-pairing.md:38-44`.

use std::fmt;

/// The ceremony version this client speaks.
///
/// A mismatch is `FailedPrecondition: pairing client must reload`, which is the
/// coordinator refusing a value it cannot interpret — so it is checked on BOTH
/// sides of every request rather than negotiated.
pub const PAIRING_CEREMONY_VERSION: u32 = 1;

/// How many decimal digits a verification code has.
pub const PAIR_VERIFICATION_CODE_LENGTH: usize = 6;

/// The request id's byte width; rendered as 32 lowercase hex characters.
pub const PAIR_REQUEST_ID_BYTES: usize = 16;

/// The requester token's byte width; rendered as 64 lowercase hex characters.
pub const PAIR_REQUESTER_TOKEN_BYTES: usize = 32;

/// The decimal space a verification code is drawn from.
const PAIR_VERIFICATION_CODE_SPACE: u64 = 1_000_000;

/// `2^32` rounded DOWN to a whole multiple of the code space: every draw at or
/// above this is discarded rather than folded, which is what removes the modulo
/// bias.
const UNBIASED_VERIFICATION_CODE_LIMIT: u64 = 4_294 * PAIR_VERIFICATION_CODE_SPACE;

/// Why ceremony entropy could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CeremonyError {
    /// The host's random source refused or is absent.
    Entropy {
        /// What the host reported.
        detail: String,
    },
}

impl fmt::Display for CeremonyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entropy { detail } => {
                write!(formatter, "the host has no usable random source: {detail}")
            }
        }
    }
}

impl std::error::Error for CeremonyError {}

/// A source of cryptographically random bytes.
///
/// `&self`, because every host's is synchronous — `crypto.getRandomValues` in a
/// browser, `/dev/urandom` on native — and a trait that returned a future would
/// put a runtime type in this crate's public API for no gain.
pub trait RandomSource {
    /// Fill `out` completely. A short fill is a refusal, never a partial success:
    /// a request id padded with zeros is a guessable request id.
    fn fill_bytes(&self, out: &mut [u8]) -> Result<(), CeremonyError>;
}

/// A random source that produces one repeated byte.
///
/// For a test that must assert an exact request id or code, and for nothing
/// else. It is named `FixedRandomSource` rather than hidden behind a default so
/// that a caller who reaches for it by accident is visible in review: a
/// production ceremony on this source is a six-digit code anyone can predict.
#[derive(Debug, Clone, Copy)]
pub struct FixedRandomSource {
    byte: u8,
}

impl FixedRandomSource {
    /// A source that yields `byte` for every position.
    pub const fn new(byte: u8) -> Self {
        Self { byte }
    }
}

impl RandomSource for FixedRandomSource {
    fn fill_bytes(&self, out: &mut [u8]) -> Result<(), CeremonyError> {
        out.fill(self.byte);
        Ok(())
    }
}

/// A 16-byte request id as 32 lowercase hex characters.
pub fn generate_pair_request_id(
    source: &dyn RandomSource,
) -> Result<String, CeremonyError> {
    let mut bytes = [0u8; PAIR_REQUEST_ID_BYTES];
    source.fill_bytes(&mut bytes)?;
    Ok(lower_hex(&bytes))
}

/// A 32-byte requester token as 64 lowercase hex characters.
pub fn generate_pair_requester_token(
    source: &dyn RandomSource,
) -> Result<String, CeremonyError> {
    let mut bytes = [0u8; PAIR_REQUESTER_TOKEN_BYTES];
    source.fill_bytes(&mut bytes)?;
    Ok(lower_hex(&bytes))
}

/// A random source that counts up from `start`, one byte at a time.
///
/// For the boundary the code generator actually has: a draw at or above the
/// rejection limit must be discarded and the next one used, and a source that
/// repeats one byte can only ever produce the same draw — which would make the
/// rejection branch a hang rather than a test.
#[derive(Debug)]
pub struct CountingRandomSource {
    next: std::cell::Cell<u8>,
}

impl CountingRandomSource {
    /// A source whose first byte is `start`.
    pub const fn new(start: u8) -> Self {
        Self {
            next: std::cell::Cell::new(start),
        }
    }
}

impl RandomSource for CountingRandomSource {
    fn fill_bytes(&self, out: &mut [u8]) -> Result<(), CeremonyError> {
        for byte in out.iter_mut() {
            *byte = self.next.get();
            self.next.set(self.next.get().wrapping_add(1));
        }
        Ok(())
    }
}


/// A six-digit verification code, drawn without bias.
pub fn generate_pair_verification_code(
    source: &dyn RandomSource,
) -> Result<String, CeremonyError> {
    loop {
        let mut bytes = [0u8; 4];
        source.fill_bytes(&mut bytes)?;
        // Big-endian, stated rather than left to the host: a native-endian read
        // would make the same bytes mean different draws on two machines.
        let draw = u32::from_be_bytes(bytes);
        if u64::from(draw) >= UNBIASED_VERIFICATION_CODE_LIMIT {
            continue;
        }
        return Ok(format!(
            "{:0width$}",
            u64::from(draw) % PAIR_VERIFICATION_CODE_SPACE,
            width = PAIR_VERIFICATION_CODE_LENGTH
        ));
    }
}

/// Accept a request id only in its canonical spelling.
///
/// Uppercase is refused for the same reason a fingerprint's is: one value must
/// have exactly one spelling, or a token-bound poll addressed in the wrong case
/// reads as a request that does not exist.
pub fn normalize_pair_request_id(value: &str) -> Option<String> {
    is_lowercase_hex(value, PAIR_REQUEST_ID_BYTES * 2).then(|| value.to_string())
}

/// Accept a requester token only in its canonical spelling.
pub fn normalize_pair_requester_token(value: &str) -> Option<String> {
    is_lowercase_hex(value, PAIR_REQUESTER_TOKEN_BYTES * 2).then(|| value.to_string())
}

/// Accept a verification code only as exactly six ASCII digits.
pub fn normalize_pair_verification_code(value: &str) -> Option<String> {
    (value.len() == PAIR_VERIFICATION_CODE_LENGTH
        && value.bytes().all(|byte| byte.is_ascii_digit()))
    .then(|| value.to_string())
}

/// Strip whitespace from what a human typed or pasted.
///
/// A six-digit code read off a screen and typed back arrives with a space or a
/// dash in it often enough that refusing it would fail a correct pairing. Only
/// whitespace goes: a dash is a different value, and quietly accepting one would
/// be accepting a code nobody generated.
pub fn compact_pair_verification_code(value: &str) -> String {
    value.chars().filter(|character| !character.is_whitespace()).collect()
}

/// Whether `value` is exactly `expected` lowercase hex characters.
fn is_lowercase_hex(value: &str, expected: usize) -> bool {
    value.len() == expected
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Ceremony entropy as lowercase hex.
///
/// Not `roost_protocol::fingerprint::fingerprint_hex`: that renders a SHA-256
/// digest and says so, and a request id is entropy, not a digest. Forcing
/// ceremony bytes through a 32-byte-only renderer would mean padding an id to
/// the wrong width to reuse it.
fn lower_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    hex
}
