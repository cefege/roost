//! The pairing ceremony's secret contract: the shape of a request id, a
//! requester token and a verification code, the one digest every pairing
//! secret is stored under, and the attempt bound that ends a series.
//!
//! Owned by the pairing slice. `protocol/spec/auth-and-pairing.md` is the
//! definition; `apps/coord/src/auth/pairing-secrets.ts` is v2's
//! implementation of it and `packages/protocol/src/pairing.ts` holds the half
//! both ends share.
//!
//! WHERE THE SHARED HALF LIVES, AND WHY IT IS HERE. v2 keeps the canonical
//! validators in `@roost/protocol/pairing` and re-exports them, because the
//! browser mints these values and the coordinator only validates them. In v3
//! `roost-protocol` has no `pairing` module, and a browser that ported its own
//! copy of `PAIRING_CEREMONY_VERSION` would be a second definition of the one
//! number that decides whether two coordinators can talk. So the validators
//! live here, in the module whose whole subject is those values, and the
//! integrator's extraction into `roost-protocol::pairing` is recorded in this
//! slice's report: it is a move, not a second implementation.
//!
//! A SECRET IS ONLY EVER STORED AS ITS DIGEST. `requester_token` and
//! `verification_code` arrive over the wire, are validated by shape, digested
//! by [`pairing_secret_digest`], and dropped. Nothing in this module returns
//! them, keeps them in a struct, or formats them; the only value any caller
//! holds afterwards is the hex digest, and the only way a digest leaves is
//! bound to a SQL statement.

use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::alphabet;
use base64::Engine as _;
use sha2::Digest as _;

use super::{PairingRefusal, PairingResult};

/// The ceremony version this coordinator speaks.
///
/// Bumped only when the wire shape of a `Pair*` message changes incompatibly.
/// It is stored on every row so a request created by an older browser is
/// refused by name ("reload") rather than being confirmed with a code the new
/// shape means something else by.
pub const PAIRING_CEREMONY_VERSION: u32 = 1;

/// Decimal digits in a verification code.
pub const PAIR_VERIFICATION_CODE_LENGTH: usize = 6;

/// Random bytes behind a pair request id, and the hex width that renders it.
pub const PAIR_REQUEST_ID_BYTES: usize = 16;
pub const PAIR_REQUEST_ID_HEX_LEN: usize = PAIR_REQUEST_ID_BYTES * 2;

/// Random bytes behind a requester token, and the hex width that renders it.
pub const PAIR_REQUESTER_TOKEN_BYTES: usize = 32;
pub const PAIR_REQUESTER_TOKEN_HEX_LEN: usize = PAIR_REQUESTER_TOKEN_BYTES * 2;

/// Wrong verification codes a confirmation will accept before the request
/// terminalizes as `verification_failed`.
///
/// Bounded because the code is six decimal digits: an unbounded series is ten
/// thousand guesses against a standing credential, and a request lives ten
/// minutes. Five is v2's bound (`pairing-secrets.ts:23`).
pub const PAIR_VERIFICATION_ATTEMPT_LIMIT: i64 = 5;

/// How long a created request stays redeemable.
pub const PAIR_REQUEST_TTL_MS: i64 = 10 * 60_000;

/// How many requests may be live at once.
///
/// The cap is a denial-of-service bound, not a quota: past it, `PairCreate`
/// refuses rather than evicting, because evicting somebody's live request to
/// admit an attacker's is a worse failure than a refused pairing.
pub const MAX_PENDING_PAIR_REQUESTS: i64 = 32;

/// The `ssh-ed25519` key-type string an SSH public key blob is prefixed with.
const SSH_ED25519_TYPE: &[u8] = b"ssh-ed25519";

/// Refuse a browser whose ceremony this coordinator does not speak.
pub fn assert_pairing_ceremony_version(ceremony_version: u32) -> PairingResult<()> {
    if ceremony_version == PAIRING_CEREMONY_VERSION {
        Ok(())
    } else {
        Err(super::refuse(PairingRefusal::CeremonyVersion))
    }
}

/// The request id, if it is 32 lowercase hex characters.
///
/// Lowercase only, matching the renderer the browser used to mint it: a
/// case-insensitive match here would let two spellings of one id coexist and
/// make `pair_requests.ephemeral_id`'s UNIQUE index mean two different things.
pub fn normalize_pair_request_id(value: &str) -> PairingResult<&str> {
    if value.len() == PAIR_REQUEST_ID_HEX_LEN && is_lowercase_hex(value) {
        Ok(value)
    } else {
        Err(super::refuse(PairingRefusal::InvalidRequestId))
    }
}

/// The requester token, if it is 64 lowercase hex characters.
pub fn normalize_pair_requester_token(value: &str) -> PairingResult<&str> {
    if value.len() == PAIR_REQUESTER_TOKEN_HEX_LEN && is_lowercase_hex(value) {
        Ok(value)
    } else {
        Err(super::refuse(PairingRefusal::InvalidRequesterToken))
    }
}

/// The verification code, if it is exactly six ASCII digits.
///
/// Digits only and no separators, because this value is read aloud across a
/// room and typed by a human: every character a human might drop or add is a
/// character that would have to be normalized, and normalization is how a
/// six-digit code becomes a code space an attacker can enumerate.
pub fn normalize_pair_verification_code(value: &str) -> PairingResult<&str> {
    if value.len() == PAIR_VERIFICATION_CODE_LENGTH && value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(value)
    } else {
        Err(super::refuse(PairingRefusal::InvalidVerificationCode))
    }
}

/// The stored form of a pairing secret: lowercase hex of its SHA-256 digest.
///
/// The plaintext is borrowed, digested, and dropped here. This is the ONLY
/// function in the crate that turns a pairing secret into anything storable,
/// and it is also `bootstrapTokenDigest`'s definition
/// (`apps/coord/src/auth/bootstrap-tokens.ts:54`): same primitive, same
/// rendering, so a secret digested as a bootstrap token and the same secret
/// digested as a pairing secret are the same string and cannot be confused at
/// the boundary between the two ceremonies.
#[must_use]
pub fn pairing_secret_digest(plaintext: &str) -> String {
    let digest: [u8; 32] = sha2::Sha256::digest(plaintext.as_bytes()).into();
    hex::encode(digest)
}

/// The raw 32-byte ed25519 public key a `PairCreate` request carries.
///
/// Accepts both encodings a browser may send: the bare key, and the
/// `ssh-ed25519` wire blob `authorized_keys` lines use
/// (`apps/coord/src/auth/authorized-keys.ts:37-57`). The blob form is parsed
/// field by field rather than by offset arithmetic on a trusted length, so a
/// blob that declares the wrong key length is refused instead of being read
/// from a body that happens to be long enough.
pub fn decode_ed25519_pubkey(encoded: &str) -> PairingResult<[u8; 32]> {
    let normalized: String = encoded
        .chars()
        .filter(|character| *character != '-' && *character != '_')
        .collect();
    let decoded = decode_base64_lenient(&normalized)
        .ok_or_else(|| super::refuse(PairingRefusal::InvalidPublicKey))?;
    match decoded.len() {
        32 => to_key(&decoded),
        _ => ssh_blob_key(&decoded),
    }
}

/// The key half of an `ssh-ed25519` blob, after the type and length framing.
fn ssh_blob_key(decoded: &[u8]) -> PairingResult<[u8; 32]> {
    let type_len = read_u32(decoded, 0)?;
    if type_len as usize != SSH_ED25519_TYPE.len()
        || decoded.get(4..4 + SSH_ED25519_TYPE.len()) != Some(SSH_ED25519_TYPE)
    {
        return Err(super::refuse(PairingRefusal::InvalidPublicKey));
    }
    let key_len = read_u32(decoded, 4 + SSH_ED25519_TYPE.len())?;
    let key_at = 8 + SSH_ED25519_TYPE.len();
    if key_len as usize != 32 || decoded.len() != key_at + 32 {
        return Err(super::refuse(PairingRefusal::InvalidPublicKey));
    }
    to_key(&decoded[key_at..])
}

/// A big-endian `u32` length prefix, or a malformed blob.
fn read_u32(decoded: &[u8], at: usize) -> PairingResult<u32> {
    let bytes = decoded
        .get(at..at + 4)
        .ok_or_else(|| super::refuse(PairingRefusal::InvalidPublicKey))?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Exactly 32 bytes into a key, or a refusal. Never zero-pads: a padded key
/// would fingerprint to a different key than the one the browser holds.
fn to_key(bytes: &[u8]) -> PairingResult<[u8; 32]> {
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| super::refuse(PairingRefusal::InvalidPublicKey))?;
    Ok(key)
}

/// Base64 that tolerates absent padding, as `Buffer.from(value, "base64")`
/// does. Stricter about the alphabet than Node, which is a difference a
/// browser cannot reach: it mints the encoding, it does not transcribe it.
/// The engine is built per call rather than held in a static -- it is two
/// small fields, and a `PairCreate` is not a hot path.
fn decode_base64_lenient(value: &str) -> Option<Vec<u8>> {
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent);
    GeneralPurpose::new(&alphabet::STANDARD, config).decode(value).ok()
}

/// Lowercase hex, and nothing else.
fn is_lowercase_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
