//! The one decoder for an Ed25519 public key a client presents: pairing,
//! bootstrap redemption, device rotation and the `authorized_keys` import.
//!
//! Two encodings reach the coordinator: the bare 32-byte key a browser or
//! worker sends, and the OpenSSH wire blob a file import carries. Both arrive
//! in standard or URL-safe base64, padded or not.

use base64::Engine as _;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};

/// The width of a raw Ed25519 public key.
pub const ED25519_PUBLIC_KEY_BYTES: usize = 32;

/// The OpenSSH key-type name a wire-format public key is wrapped in.
const SSH_ED25519_TYPE: &[u8] = b"ssh-ed25519";

/// `<u32 len><"ssh-ed25519"><u32 len><32 bytes>`.
const SSH_WIRE_KEY_BYTES: usize = 4 + SSH_ED25519_TYPE.len() + 4 + ED25519_PUBLIC_KEY_BYTES;

/// Decode the public key a redeeming device, worker or pairing browser sent.
///
/// Accepts exactly 32 raw bytes, or exactly one OpenSSH wire blob. The wire
/// form is checked field by field and by total length rather than sliced at a
/// fixed offset: a slice accepts any document long enough, and a key of the
/// wrong width is never zero-padded, because a padded key fingerprints to a
/// different key than the one the sender holds.
#[must_use]
pub fn decode_ed25519_pubkey(encoded: &str) -> Option<[u8; ED25519_PUBLIC_KEY_BYTES]> {
    let raw = decode_lenient_base64(encoded)?;
    if raw.len() == ED25519_PUBLIC_KEY_BYTES {
        return <[u8; ED25519_PUBLIC_KEY_BYTES]>::try_from(raw).ok();
    }
    ssh_wire_key(&raw)
}

/// Decode only the OpenSSH wire form, the blob an `authorized_keys` line
/// carries in its second field. A bare 32-byte key is refused here: a line
/// holding anything but the whole document was never a key.
#[must_use]
pub fn decode_ssh_wire_ed25519_pubkey(encoded: &str) -> Option<[u8; ED25519_PUBLIC_KEY_BYTES]> {
    ssh_wire_key(&decode_lenient_base64(encoded)?)
}

/// Standard or URL-safe base64, padded or not, trailing bits allowed.
///
/// A browser sends URL-safe and a file import sends standard; refusing either
/// fails an enrollment that has a perfectly good key. The URL-safe characters
/// are translated, never dropped: dropping `-` and `_` would change the byte
/// count and corrupt the key.
fn decode_lenient_base64(encoded: &str) -> Option<Vec<u8>> {
    // The shipped `STANDARD_*` engines refuse trailing bits, so the engine is
    // built here to stay lenient about both padding and trailing bits.
    const DECODER: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    DECODER
        .decode(encoded.replace('-', "+").replace('_', "/"))
        .ok()
}

/// The key bytes out of an OpenSSH wire document, or `None` if it is not
/// exactly one.
fn ssh_wire_key(raw: &[u8]) -> Option<[u8; ED25519_PUBLIC_KEY_BYTES]> {
    if raw.len() != SSH_WIRE_KEY_BYTES {
        return None;
    }
    let type_end = 4 + SSH_ED25519_TYPE.len();
    let type_length = u32::from_be_bytes(raw[..4].try_into().ok()?) as usize;
    let key_length = u32::from_be_bytes(raw[type_end..type_end + 4].try_into().ok()?) as usize;
    if type_length != SSH_ED25519_TYPE.len()
        || &raw[4..type_end] != SSH_ED25519_TYPE
        || key_length != ED25519_PUBLIC_KEY_BYTES
    {
        return None;
    }
    <[u8; ED25519_PUBLIC_KEY_BYTES]>::try_from(&raw[type_end + 4..]).ok()
}
