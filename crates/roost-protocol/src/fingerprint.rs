//! The lowercase-hex rendering of a machine fingerprint.
//!
//! All three ends of the protocol derive a worker's, a coordinator's, or a
//! device's fingerprint independently, so a byte-for-byte divergence here
//! silently breaks pairing, JWT `kid` lookup, and authorized-keys matching at
//! once. The rendering lives here so there is one definition; the hash does
//! not, because hashing is a platform capability and this crate has none —
//! the coordinator, the worker, and the browser each hash with the primitive
//! their runtime provides and hand the 32 bytes in.

/// The byte width of a SHA-256 digest and of an ed25519 public key.
pub const FINGERPRINT_INPUT_BYTES: usize = 32;

/// The hex width of a rendered fingerprint: two characters per byte.
pub const FINGERPRINT_HEX_LEN: usize = FINGERPRINT_INPUT_BYTES * 2;

/// The lowercase hex of a SHA-256 digest of a raw ed25519 public key.
pub fn fingerprint_hex(digest: &[u8; FINGERPRINT_INPUT_BYTES]) -> String {
    let mut hex = String::with_capacity(FINGERPRINT_HEX_LEN);
    for byte in digest {
        hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    hex
}

/// Whether `value` has the shape of a rendered fingerprint: exactly 64
/// lowercase hex characters. Uppercase is rejected because a fingerprint
/// minted here is always lowercase and a match that ignores case would let
/// two spellings of one key coexist in the authorized-keys file.
pub fn is_fingerprint_hex(value: &str) -> bool {
    value.len() == FINGERPRINT_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::{FINGERPRINT_HEX_LEN, fingerprint_hex, is_fingerprint_hex};

    #[test]
    fn a_digest_renders_as_sixty_four_lowercase_hex_characters() {
        let mut digest = [0u8; 32];
        for (index, byte) in digest.iter_mut().enumerate() {
            *byte = index as u8;
        }
        let rendered = fingerprint_hex(&digest);
        assert_eq!(rendered.len(), FINGERPRINT_HEX_LEN);
        assert_eq!(
            rendered,
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
    }

    #[test]
    fn a_high_nibble_never_renders_as_nothing() {
        let rendered = fingerprint_hex(&[0xff; 32]);
        assert_eq!(rendered, "f".repeat(64));
    }

    #[test]
    fn uppercase_is_not_a_fingerprint() {
        assert!(is_fingerprint_hex(&"a".repeat(64)));
        assert!(!is_fingerprint_hex(&"A".repeat(64)));
        assert!(!is_fingerprint_hex(&"a".repeat(63)));
        assert!(!is_fingerprint_hex(&format!("{}g", "a".repeat(63))));
    }
}
