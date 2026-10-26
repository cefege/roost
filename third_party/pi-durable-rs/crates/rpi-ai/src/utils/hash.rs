//! Port of `packages/ai/src/utils/hash.ts` @ pi 0.82.1 (2efa728).

/// `shortHash` — fast deterministic hash to shorten long strings.
///
/// Operates on UTF-16 code units like JS `charCodeAt`; multiplication is
/// 32-bit with wraparound (`Math.imul`), output is two base-36 halves.
pub fn short_hash(input: &str) -> String {
    let mut h1: u32 = 0xdeadbeef;
    let mut h2: u32 = 0x41c6ce57;
    for ch in input.encode_utf16() {
        h1 = (h1 ^ u32::from(ch)).wrapping_mul(2654435761);
        h2 = (h2 ^ u32::from(ch)).wrapping_mul(1597334677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2246822507) ^ (h2 ^ (h2 >> 13)).wrapping_mul(3266489909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2246822507) ^ (h1 ^ (h1 >> 13)).wrapping_mul(3266489909);
    format!("{}{}", to_base36(h2), to_base36(h1))
}

fn to_base36(mut value: u32) -> String {
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        digits.push(match digit {
            0..=9 => (b'0' + digit) as char,
            _ => (b'a' + digit - 10) as char,
        });
        value /= 36;
    }
    digits.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_hash_deterministic() {
        assert_eq!(short_hash("hello"), short_hash("hello"));
        assert_ne!(short_hash("hello"), short_hash("world"));
    }

    #[test]
    fn test_short_hash_matches_upstream() {
        // Golden values captured from the pinned upstream hash.ts (node).
        assert_eq!(short_hash("hello"), "1h6qa0qrowduu");
        assert_eq!(short_hash("world"), "yoqfis1dkxj7l");
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("call_abc|item_xyz"), "5r62khfrcycv");
        // UTF-16 code-unit semantics: astral chars hash as surrogate pairs.
        assert_eq!(short_hash("🙈"), "kphsz0153ms3q");
    }

    #[test]
    fn test_to_base36() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
    }
}
