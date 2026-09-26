//! Streaming gzip of one file into another, in fixed 1 MiB slices, so peak
//! memory tracks the chunk size instead of the input size.
//!
//! Owned by the coordinator. `backup` calls this for the nightly archive. A
//! coordinator database is measured in gigabytes, and anything that reads a
//! file whole to compress it holds the whole file on the heap while doing it.

use std::io::Error as IoError;
use std::path::Path;
use async_compression::tokio::write::GzipEncoder;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};

/// The slice size on each side of the compressor, and the reason peak memory
/// is bounded. v2's `GZIP_FILE_CHUNK_BYTES` (`gzip-file.ts:9`).
pub const GZIP_FILE_CHUNK_BYTES: usize = 1024 * 1024;

/// How many bytes moved, for the archive's own log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GzipFileResult {
    /// Bytes read from the source file.
    pub bytes_in: u64,
    /// Bytes written to the destination archive.
    pub bytes_out: u64,
}

/// Compress `source` into a fresh owner-only gzip archive at `destination`.
///
/// `destination` is created at mode `0600` rather than `0666`-and-umask: a
/// database archive must never be world-readable, not even for the length of
/// one write.
pub async fn gzip_file_to_path(
    source: &Path,
    destination: &Path,
) -> Result<GzipFileResult, IoError> {
    let outcome = compress_file(source, destination).await;
    if outcome.is_err() {
        // A partial archive must never survive for a later reader to trust, and
        // an unremovable destination must not mask the failure that brought us
        // here (`gzip-file.ts:80-81`).
        let _ = tokio::fs::remove_file(destination).await;
    }
    outcome
}

async fn compress_file(source: &Path, destination: &Path) -> Result<GzipFileResult, IoError> {
    let mut input = BufReader::with_capacity(GZIP_FILE_CHUNK_BYTES, tokio::fs::File::open(source).await?);
    let destination_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(destination)
        .await?;
    let mut output = BufWriter::with_capacity(GZIP_FILE_CHUNK_BYTES, destination_file);

    // A single task drives both ends, so backpressure IS the bound: the next
    // slice is not read until the previous one has been compressed and
    // written. v2 needed an explicit drain pump racing every await for the
    // same reason -- a JS `CompressionStream` whose readable side nobody pulls
    // stalls its writer once the queue fills -- and a pump that died on a full
    // disk left an `await` that never settled, a silent hang holding the
    // snapshot open. Here a failing destination stops the read instead.
    let mut encoder = GzipEncoder::new(&mut output);
    let bytes_in = tokio::io::copy(&mut input, &mut encoder).await?;
    // Writes the gzip trailer; a dropped encoder would leave a member that
    // decompresses to a truncated file.
    encoder.shutdown().await?;
    output.flush().await?;
    let bytes_out = output.get_ref().metadata().await?.len();

    Ok(GzipFileResult { bytes_in, bytes_out })
}
