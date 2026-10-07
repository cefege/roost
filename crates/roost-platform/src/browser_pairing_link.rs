//! The one link a new browser opens to pair: the origin it will live on, and a
//! one-shot browser grant carried in the `#pair=` fragment.
//!
//! Owned here because more than one surface mints it — `roost add-browser` and
//! `roost quickstart` against a coordinator database, and Settings → Devices
//! against a running coordinator — and the web client's boot captures exactly
//! this shape. No I/O, so the encoding can be proved without a coordinator.

/// The fragment key the web client's boot captures and spends.
pub const PAIR_FRAGMENT_KEY: &str = "pair";

/// The URL that pairs a browser by spending `grant` at `origin`.
///
/// The grant rides in the fragment, which a browser never sends to the server,
/// so it stays out of request logs. A trailing `/` on the origin is dropped so a
/// declared `https://host/` and `https://host` produce one link.
pub fn browser_pairing_link(origin: &str, grant: &str) -> String {
    let origin = origin.trim_end_matches('/');
    format!(
        "{origin}/#{PAIR_FRAGMENT_KEY}={}",
        percent_encode_fragment_value(grant)
    )
}

/// Percent-encode the one value that travels in the fragment. Unreserved bytes
/// pass through; a minted grant is `roost_bt_` plus hex, so in practice nothing
/// is escaped, and anything else cannot split the fragment into two keys.
fn percent_encode_fragment_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            other => {
                encoded.push('%');
                encoded.push(char::from(HEX_DIGITS[usize::from(other >> 4)]));
                encoded.push(char::from(HEX_DIGITS[usize::from(other & 0x0f)]));
            }
        }
    }
    encoded
}

/// Upper-case hex, the spelling RFC 3986 recommends for a percent-encoding.
const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

#[cfg(test)]
mod tests {
    use super::browser_pairing_link;

    #[test]
    fn a_minted_grant_rides_unescaped_in_the_fragment_at_the_origin_root() {
        assert_eq!(
            browser_pairing_link("https://roost.example.com", "roost_bt_0a1b"),
            "https://roost.example.com/#pair=roost_bt_0a1b"
        );
        assert_eq!(
            browser_pairing_link("https://roost.example.com/", "roost_bt_0a1b"),
            "https://roost.example.com/#pair=roost_bt_0a1b"
        );
    }

    #[test]
    fn a_byte_that_could_split_the_fragment_is_escaped() {
        assert_eq!(
            browser_pairing_link("http://127.0.0.1:4113", "a&pair=b c"),
            "http://127.0.0.1:4113/#pair=a%26pair%3Db%20c"
        );
    }
}
