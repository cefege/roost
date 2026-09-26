//! The Ed25519 signature check, and the SPKI framing v2 hand-rolls.
//!
//! Owned by the coordinator's auth layer; `jwt_claims` owns a token's shape and
//! `jwt_verify` owns the time and revocation bounds. Splitting them is what lets
//! a test prove the primitive against a token an **independent** implementation
//! signed -- see `tests/jwt_parity.rs` and
//! `tests/fixtures/v2_jwt_vectors.json`, which are minted by v2's own
//! `apps/worker/src/host/jwt.ts` under Bun.
//!
//! WHY THE SPKI PREFIX IS HAND-WRITTEN RATHER THAN DERIVED. A raw 32-byte
//! Ed25519 public key is not a DER document. v2 prepends twelve fixed bytes to
//! reach a minimal `SubjectPublicKeyInfo` (`apps/coord/src/auth/jwt.ts:15-17`),
//! and that is the shape the browser's WebCrypto and the worker's
//! `crypto.subtle` accept. `ed25519-dalek` takes the raw 32 bytes directly, so
//! the verifier does not need the prefix -- it is kept as a documented constant
//! and asserted byte-for-byte in a test, because a port that "cleaned it up"
//! and then fed a raw key to a DER parser elsewhere would fail pairing with no
//! obvious cause.
//!
//! WHY `ed25519-dalek` IS THE ONLY CRYPTO DEPENDENCY ADDED. v2's coordinator
//! verifies with Bun's `crypto.subtle` built-in and therefore imports no
//! third-party package at all; Rust has no such built-in, so parity REQUIRES an
//! implementation. Everything else the token needs is already owned exactly
//! once elsewhere: the SHA-256 digest is `sha2`, the hex rendering is
//! `roost_protocol::fingerprint::fingerprint_hex`, and the base64url codec is
//! `roost_host::jwt_base`.

use roost_host::b64url_decode;
use roost_protocol::{ProtocolError, ProtocolResult};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::jwt_claims::JwtParts;

/// The 12-byte `SubjectPublicKeyInfo` prefix v2 prepends to a raw Ed25519
/// public key: `SEQUENCE(0x2a) { SEQUENCE(5) { OID(3) 2b 65 70 } BIT STRING
/// (0x21) 0 unused-bits + 32 key bytes }`.
///
/// Byte-for-byte from `apps/coord/src/auth/jwt.ts:15-17`. The signing side uses
/// the sibling 16-byte PKCS#8 prefix for the private seed
/// (`apps/worker/src/host/jwt.ts:14-17`); both are literals in wire terms, and
/// neither is derivable without a DER encoder.
pub const SPKI_ED25519_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The width of a raw Ed25519 public key, and therefore of the DER document
/// [`spki_document`] produces.
pub const PUBLIC_KEY_BYTES: usize = 32;

/// Why a signature did not verify.
///
/// Split from [`ProtocolError`] because each reason gets its own `signal`: a
/// stored-key length fault is a database problem and must page someone, while
/// `Invalid` is the only value a peer can influence and is the only one worth a
/// rate-limited signal. One opaque "bad token" would make a credential sweep
/// indistinguishable from one busy worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignatureFailure {
    /// The stored public key was not 32 bytes. A database fault, not a peer
    /// fault -- `apps/coord/src/auth/jwt.ts:114` answers
    /// `stored pubkey wrong length`.
    #[error("stored pubkey wrong length")]
    StoredKeyLength,
    /// The signature segment did not decode to 64 raw bytes.
    #[error("bad signature encoding")]
    SignatureEncoding,
    /// The signature is well-formed and does not verify: the signature, the
    /// signed bytes, or the key are not the three v2 would have used.
    #[error("signature invalid")]
    Invalid,
}

/// The raw 32-byte Ed25519 public key a token's `kid` resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicKey([u8; PUBLIC_KEY_BYTES]);

impl PublicKey {
    /// Wrap exactly 32 bytes.
    ///
    /// A wrong length is refused here rather than zero-padded, because a padded
    /// key would verify against a **different** public key than the
    /// `authorized_keys` row holds -- a silent security failure rather than a
    /// crash, and the kind that passes every round-trip test.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SignatureFailure> {
        let array: [u8; PUBLIC_KEY_BYTES] = bytes
            .try_into()
            .map_err(|_| SignatureFailure::StoredKeyLength)?;
        Ok(Self(array))
    }

    /// The raw bytes, exactly as the `authorized_keys` row stores them.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_BYTES] {
        &self.0
    }
}

/// The minimal `SubjectPublicKeyInfo` document v2 feeds to WebCrypto.
///
/// Kept so a test can assert this crate frames a key exactly as v2 does; the
/// verifier itself does not use it.
#[must_use]
pub fn spki_document(key: &PublicKey) -> [u8; SPKI_ED25519_PREFIX.len() + PUBLIC_KEY_BYTES] {
    let mut document = [0u8; SPKI_ED25519_PREFIX.len() + PUBLIC_KEY_BYTES];
    document[..SPKI_ED25519_PREFIX.len()].copy_from_slice(&SPKI_ED25519_PREFIX);
    document[SPKI_ED25519_PREFIX.len()..].copy_from_slice(key.as_bytes());
    document
}

/// Verify a compact JWS signature over `signing_input` with a stored key.
///
/// `signing_input` MUST be the caller's verbatim `header.payload`, never
/// something re-encoded from parsed values. This function is the only place a
/// token's bytes meet its signature, and re-encoding is how a verifier ends up
/// checking a document the signer never produced.
pub fn verify_signature(
    signing_input: &str,
    signature_segment: &str,
    key: &PublicKey,
) -> Result<(), SignatureFailure> {
    let decoded =
        b64url_decode(signature_segment).map_err(|_| SignatureFailure::SignatureEncoding)?;
    let signature =
        Signature::from_slice(&decoded).map_err(|_| SignatureFailure::SignatureEncoding)?;
    let verifying_key =
        VerifyingKey::from_bytes(key.as_bytes()).map_err(|_| SignatureFailure::Invalid)?;
    verifying_key
        .verify(signing_input.as_bytes(), &signature)
        .map_err(|_| SignatureFailure::Invalid)
}

/// Verify a split token's signature, mapping a failure onto the error the
/// transport layer reports.
pub fn verify_token_signature(parts: &JwtParts, key: &PublicKey) -> ProtocolResult<()> {
    verify_signature(&parts.signing_input, &parts.signature_segment, key)
        .map_err(|failure| ProtocolError::new("jwt.signature", failure.to_string()))
}
