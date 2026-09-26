//! The session layer's minted identities: the session id a spawn claims, the
//! trace id that correlates its log lines, and the snapshot id a parked cursor
//! is addressed by. `session::spawn` and `session::snapshot_cursor` both mint
//! through here, so there is ONE entropy read and ONE uuid rendering in the
//! worker session layer — two would be two places to fix a collision.
//!
//! The entropy source is the kernel CSPRNG at `/dev/urandom`. v3 supports Linux
//! and macOS only, and both ship that device, so there is no platform branch to
//! get wrong. `getrandom` would be the tidier spelling; it is not a declared
//! dependency, and the same hand-rolled read is already the precedent in
//! `roost-coord/src/push/vapid.rs`.

use std::fs::File;
use std::io::Read;

use roost_observability::trace::{TRACE_ID_BYTES, trace_id_from_bytes};

/// The bytes behind one uuid. The 6th and 8th nibbles are the version and
/// variant, so a minted id is a well-formed v4 shape rather than 32 hex
/// characters that happen to be unique.
const UUID_BYTES: usize = 16;

/// Why an identity could not be minted.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    #[error("this host's entropy source could not be read: {0}")]
    Entropy(String),
}

/// A fresh uuid: the session id a spawn claims, the grid epoch a core's frames
/// are numbered under, or the snapshot id a parked cursor is addressed by.
pub fn mint_uuid() -> Result<String, MintError> {
    let mut bytes = [0_u8; UUID_BYTES];
    draw(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(render(&bytes))
}

/// A fresh trace id, in the shape `roost_observability` validates.
pub fn mint_trace_id() -> Result<String, MintError> {
    let mut bytes = [0_u8; TRACE_ID_BYTES];
    draw(&mut bytes)?;
    Ok(trace_id_from_bytes(bytes))
}

fn draw(bytes: &mut [u8]) -> Result<(), MintError> {
    let mut source =
        File::open("/dev/urandom").map_err(|error| MintError::Entropy(error.to_string()))?;
    source
        .read_exact(bytes)
        .map_err(|error| MintError::Entropy(error.to_string()))
}

fn render(bytes: &[u8; UUID_BYTES]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut rendered = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            rendered.push('-');
        }
        rendered.push(char::from(HEX[usize::from(byte >> 4)]));
        rendered.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    rendered
}
