//! The one place a coordinator identifier is minted, and the one entropy
//! source behind the ones that are random.
//!
//! Owned by `coord_core`, because every domain that needs an id needs it and a
//! per-domain copy is how the same value gets written twice. Three things live
//! here and nothing else does: where randomness comes from, how sixteen bytes
//! become a v4-shaped id, and the one deterministic id a row key needs.
//!
//! WHY THESE ARE ONE CONCEPT AND NOT THREE UTILITIES. `sessions::mcp` opened
//! `/dev/urandom` and forced the version nibbles itself, and
//! `push::vapid` opened the same device for its scalar. Two copies of
//! "/dev/urandom on Linux and macOS" is a fork the moment one of them needs a
//! different source, and the nibble-forcing is the part a reader has to verify
//! by hand: get it wrong and the id is a valid-looking uuid that nothing
//! generated, which no test notices because it still parses.
//!
//! THE DETERMINISTIC ONE IS NOT A VARIANT OF THE OTHER. A task id is a ROW KEY:
//! any device may claim the next pending task without naming one, so uniqueness
//! has to come from the process epoch, the boot time and a counter, and no RNG
//! belongs in it. An MCP relay id is a minted capability name and randomness is
//! the whole point. Two sources, one module, and the comment on each says which
//! rule it is obeying.

use std::io::Read as _;

use sha2::{Digest, Sha256};

/// Draw `N` bytes from the kernel CSPRNG.
///
/// `/dev/urandom` rather than the `getrandom` crate because the crate is not
/// reachable from this crate's declared dependencies today, and because v3
/// supports Linux and macOS only (`CLAUDE.md`, "Fixed decisions") — both ship
/// the device, so there is no platform branch here to get wrong. That reasoning
/// was written once, in `push::vapid`, and it is the reason this is a
/// function rather than a `use` at each site.
pub fn draw<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
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

/// A fresh task row key.
///
/// **No RNG, deliberately.** A task id is a ROW KEY and not a capability — any
/// device may claim the next pending task without naming one — so uniqueness
/// comes from the process epoch, the boot time and a counter, and every restart
/// starts a distinguishable range. Randomness here would be a second source
/// where a derived one is provably unique within a boot.
///
/// The counter is the only mutable state in the crate, it is an
/// `AtomicU64` behind a function, and it exists because a derived id must not
/// repeat inside the process that derived it.
pub fn new_task_id(process_epoch: &str, boot_ms: i64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let mut digest = Sha256::new();
    digest.update(process_epoch.as_bytes());
    digest.update(boot_ms.to_be_bytes());
    digest.update(SEQUENCE.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.finalize()[..16]);
    render_v4(bytes)
}
