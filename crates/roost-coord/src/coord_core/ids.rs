//! The one place a coordinator identifier is minted, and the one entropy
//! source behind it.
//!
//! Owned by `coord_core`, because every domain that needs an id needs it and a
//! per-domain copy is how the same value gets written twice. Two things live
//! here and nothing else does: where randomness comes from, and how sixteen
//! bytes become a v4-shaped id.
//!
//! WHY THESE ARE ONE CONCEPT AND NOT TWO UTILITIES. Two copies of the entropy
//! source are a fork the moment one of them needs a different one, and the
//! nibble-forcing is the part a reader has to verify by hand: get it wrong and
//! the id is a valid-looking uuid that nothing generated, which no test notices
//! because it still parses.

use std::io::Error as IoError;

/// Draw `N` bytes from the operating system's CSPRNG.
///
/// Through `getrandom`, so every platform reads its own OS generator with one
/// call and there is no platform branch here to get wrong; a function rather
/// than a `use` at each site so the source is named once.
pub fn draw<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(IoError::other)?;
    Ok(bytes)
}

/// Render sixteen bytes as a v4-shaped id, in the 8-4-4-4-12 grouping.
///
/// The version and variant nibbles are what every consumer actually checks, and
/// forcing them here is the reason this function exists rather than a `format!`
/// at each call site: an id that keeps its random version nibble still parses
/// as a uuid, so a consumer that checks the shape alone cannot tell.
#[must_use]
pub fn render_v4(bytes: [u8; 16]) -> String {
    let mut bytes = bytes;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    // The grouping is cut from the rendered hex rather than assembled byte by
    // byte, so there is one place the 8-4-4-4-12 decision is written down.
    let groups = [0, 8, 12, 16, 20, 32];
    groups
        .windows(2)
        .map(|ends| &hex[ends[0]..ends[1]])
        .collect::<Vec<_>>()
        .join("-")
}
