//! The one owner of terminal-capture file storage on a worker host: the
//! owner-only directory, exclusive `0600` writes named by capture id, and the
//! retention sweep over them. `capture::recorder` freezes a payload and calls
//! [`write_bundle`]; nothing else in the worker touches this directory.
//!
//! It is v2's `apps/worker/src/diag/capture-storage.ts`, and three of its rules
//! are the reason it is one file rather than a helper beside the recorder.
//!
//! THE DIRECTORY IS OWNER-ONLY AND THE CREATE IS EXCLUSIVE. The payload is
//! somebody's terminal, and a `0600` file in a `0700` directory is the only
//! thing standing between an incident bundle and the next account on the box.
//!
//! THE CAPTURE ID IS THE WHOLE FILENAME. It is a UUID minted by the
//! coordinator, so the name cannot collide with a neighbour's log — and the
//! create is `O_EXCL` so an RPC retry that reached the writer twice FAILS
//! rather than overwriting the evidence the first attempt already froze.
//!
//! NO ERROR TEXT LEAVES THIS FILE. An `errno` message can carry a path, and the
//! path can carry a session id, and the answer crosses a trust boundary into an
//! operator-visible download. The code is the report.

use std::path::{Path, PathBuf};

use roost_protocol::terminal_capture::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode};
use tokio::io::AsyncWriteExt as _;

use super::CAPTURE_DIR_NAME;

/// The directory's mode. Owner-only, because the bundles are terminal content.
const CAPTURE_DIR_MODE: u32 = 0o700;
/// The file's mode. Owner-only, for the same reason.
const CAPTURE_FILE_MODE: u32 = 0o600;

/// What a write produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    Written { path: PathBuf, byte_length: u64 },
    Refused(TerminalCaptureErrorCode),
}

/// Write one incident bundle, or say which of the protocol's codes it earned.
///
/// Every refusal below is a code rather than a message, and that is the whole
/// point of the signature: the caller serialises this into a reply that crosses
/// a trust boundary, and it must not be able to carry a path.
pub async fn write_bundle(
    log_dir: &Path,
    capture_id: &str,
    payload: &[u8],
) -> Stored {
    if payload.len() > TERMINAL_CAPTURE_LIMITS.bundle_bytes {
        return Stored::Refused(TerminalCaptureErrorCode::ResourceExhausted);
    }
    let directory = log_dir.join(CAPTURE_DIR_NAME);
    if ensure_owner_only_dir(&directory).await.is_err() {
        return Stored::Refused(TerminalCaptureErrorCode::StorageFailed);
    }
    sweep_retention(&directory, 1, payload.len() as u64).await;
    let path = directory.join(bundle_file_name(capture_id));
    // `create_new` is the exclusive create. A retry that arrives twice must not
    // be able to replace the bytes the first attempt froze.
    match tokio::fs::File::create_new(&path).await {
        Ok(mut file) => {
            if let Err(error) = file.write_all(payload).await {
                tracing::error!(
                    capture_id,
                    byte_len = payload.len(),
                    %error,
                    "a terminal capture could not be written in full"
                );
                return Stored::Refused(TerminalCaptureErrorCode::StorageFailed);
            }
            if let Err(error) = set_owner_only(&path).await {
                tracing::error!(capture_id, %error, "a capture file could not be sealed");
                return Stored::Refused(TerminalCaptureErrorCode::StorageFailed);
            }
            tracing::info!(
                capture_id,
                byte_len = payload.len(),
                "a terminal capture bundle was written"
            );
            Stored::Written {
                path,
                byte_length: payload.len() as u64,
            }
        }
        Err(error) => {
            tracing::error!(
                capture_id,
                byte_len = payload.len(),
                %error,
                "a terminal capture could not be created; an id is used once"
            );
            Stored::Refused(TerminalCaptureErrorCode::StorageFailed)
        }
    }
}

/// The capture's own name.
///
/// The id is a coordinator-minted UUID, so the whole filename is that UUID plus
/// one extension. Nothing else may write here, and nothing here may be named
/// anything else — a name a caller chose is a name that could escape the
/// directory.
pub fn bundle_file_name(capture_id: &str) -> String {
    format!("{capture_id}.json")
}

/// Whether a name in this directory is one this owner created.
///
/// Retention may only unlink a name it recognises: the directory also holds the
/// worker's own logs, and a sweep that removed them would be a different bug.
pub fn is_bundle_file_name(name: &str) -> bool {
    let Some(id) = name.strip_suffix(".json") else {
        return false;
    };
    !id.is_empty()
        && !id.contains('/')
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

use tokio::io::AsyncWriteExt as _;

/// Create the directory with the owner-only mode if it is not already there.
async fn ensure_owner_only_dir(directory: &Path) -> std::io::Result<()> {
    match tokio::fs::create_dir_all(directory).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(directory, std::fs::Permissions::from_mode(CAPTURE_DIR_MODE))
            .await?;
    }
    #[cfg(not(unix))]
    {
        let _ = CAPTURE_DIR_MODE;
    }
    Ok(())
}

#[cfg(unix)]
async fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(CAPTURE_FILE_MODE)).await
}

#[cfg(not(unix))]
async fn set_owner_only(path: &Path) -> std::io::Result<()> {
    let _ = path;
    Ok(())
}

/// Drop the oldest bundles until this one fits under both caps.
///
/// The new bundle's SLOT and BYTES ARE RESERVED, so the sweep cannot push the
/// combined footprint past either cap for the duration of this file's life. The
/// sweep is O(files) over a directory this owner alone populates and the cap is
/// fifty files, so there is no case for an index.
async fn sweep_retention(directory: &Path, reserve_slot: usize, reserve_bytes: u64) {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return;
    };
    let mut bundles: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_bundle_file_name(name) {
            continue;
        }
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        let modified = metadata
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        bundles.push((modified, entry.path(), metadata.len()));
    }
    if bundles.len() + reserve_slot <= TERMINAL_CAPTURE_LIMITS.storage_files
        && total_bytes(&bundles) + reserve_bytes <= TERMINAL_CAPTURE_LIMITS.storage_bytes as u64
    {
        return;
    }
    bundles.sort_by_key(|(modified, _, _)| *modified);
    let mut live_files = bundles.len() + reserve_slot;
    let mut live_bytes = total_bytes(&bundles) + reserve_bytes;
    for (_, path, bytes) in bundles {
        if live_files <= TERMINAL_CAPTURE_LIMITS.storage_files
            && live_bytes <= TERMINAL_CAPTURE_LIMITS.storage_bytes as u64
        {
            return;
        }
        if tokio::fs::remove_file(&path).await.is_ok() {
            live_files -= 1;
            live_bytes = live_bytes.saturating_sub(bytes);
        }
    }
}

fn total_bytes(bundles: &[(std::time::SystemTime, PathBuf, u64)]) -> u64 {
    bundles.iter().map(|(_, _, bytes)| *bytes).sum()
}
