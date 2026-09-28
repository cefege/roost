//! Streaming SHA-256 for attachment temp and destination files, and the one
//! lowercase-hex rendering every attachment digest uses. Ports v2
//! `apps/worker/src/attachments/attachment-file-hash.ts`. Called by the
//! operation owner (rebuilding a parked upload's digest, verifying a recovered
//! final name) and the file store (a destination that is already occupied).

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use roost_protocol::attachment_transfer::DIRECT_CHUNK_BYTES;
use sha2::{Digest, Sha256};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The running digest of a file's bytes, read one direct chunk at a time so a
/// multi-gigabyte temp never has to fit in memory.
pub fn hash_attachment_file(path: &Path) -> io::Result<Sha256> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; DIRECT_CHUNK_BYTES];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(hasher),
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// A file's SHA-256 as lowercase hex.
pub fn sha256_attachment_file(path: &Path) -> io::Result<String> {
    hash_attachment_file(path).map(digest_hex)
}

/// A byte slice's SHA-256 as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(Sha256::new_with_prefix(bytes))
}

/// Finish a running digest as lowercase hex, the form journals, manifests and
/// direct acknowledgements all compare.
pub fn digest_hex(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_of_nothing_is_the_published_empty_sha256() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
