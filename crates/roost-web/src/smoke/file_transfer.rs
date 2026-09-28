//! The chunked worker-file download the smoke probe reassembles: read 4 MiB
//! chunks until end of file (or an empty chunk), then report the length and a
//! lowercase hex digest. Native; `smoke::dispatch` supplies the RPC and the
//! SHA-256. Ports `apps/web/src/smoke/smokeFileTransferProbes.ts:35-64`.

/// The chunk size each `FilesReadChunk` asks for.
pub const DOWNLOAD_CHUNK_BYTES: u32 = 4 * 1024 * 1024;

/// One read of the file.
pub trait ChunkReader {
    /// `(data, eof)` for up to `len` bytes at `offset`.
    fn read_chunk(
        &self,
        offset: u64,
        len: u32,
    ) -> impl Future<Output = Result<(Vec<u8>, bool), String>>;
}

/// The whole file, in order.
pub async fn download_whole_file<R: ChunkReader>(reader: &R) -> Result<Vec<u8>, String> {
    let mut file = Vec::new();
    loop {
        let (data, eof) = reader
            .read_chunk(file.len() as u64, DOWNLOAD_CHUNK_BYTES)
            .await?;
        let empty = data.is_empty();
        file.extend_from_slice(&data);
        if eof || empty {
            return Ok(file);
        }
    }
}

/// Lowercase hex, two digits a byte.
pub fn lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}
