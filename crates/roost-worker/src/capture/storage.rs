//! The ONE owner of terminal-capture files on a worker host: the owner-only
//! log directory, exclusive `0600` writes named by capture UUID, and the
//! COMBINED retention sweep over legacy `bytecap-*.bin` dumps and incident
//! bundles. Ports `apps/worker/src/diag/capture-storage.ts`; called by
//! `super::bundle_writer` (write) and `super::recorder` (retention start/stop).
//! Only a capture id and a fixed code leave here: an errno can carry a path.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use roost_protocol::terminal_capture::bundle::{
    is_terminal_capture_file_name, terminal_capture_file_name,
};
use roost_protocol::terminal_capture::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

const CAPTURE_DIR_MODE: u32 = 0o700;
const CAPTURE_FILE_MODE: u32 = 0o600;
/// Retention is a 24 h ceiling, so an hourly sweep is fine-grained enough.
const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const LEGACY_BYTECAP_PREFIX: &str = "bytecap-";
const LEGACY_BYTECAP_SUFFIX: &str = ".bin";

/// One written bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCapture {
    pub path: PathBuf,
    pub byte_length: u64,
}

/// What the write path reserves while it sweeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReserve {
    /// Make room for ONE more file. Only the write path reserves a slot: a
    /// periodic sweep that did would evict the oldest bundle every hour from a
    /// steady state at exactly the cap, with nothing to store.
    pub slot: bool,
    pub bytes: u64,
}

/// The capture directory, the tightened-mode memo and the periodic sweep.
#[derive(Debug)]
pub struct CaptureStorage {
    dir: PathBuf,
    tightened: Mutex<bool>,
    retention: Mutex<Option<JoinHandle<()>>>,
    runtime: Handle,
}

impl CaptureStorage {
    /// Storage under the worker's log directory, which is where v2 keeps it.
    pub fn new(dir: PathBuf, runtime: Handle) -> Self {
        Self {
            dir,
            tightened: Mutex::new(false),
            retention: Mutex::new(None),
            runtime,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write one incident bundle. The create is EXCLUSIVE: an RPC retry that
    /// reached the writer twice must fail rather than overwrite the evidence
    /// the first attempt already froze.
    pub fn write_terminal_incident_file(
        &self,
        capture_id: &str,
        payload: &[u8],
    ) -> Result<StoredCapture, TerminalCaptureErrorCode> {
        self.ensure_owner_only_dir();
        // The new file's slot and bytes are RESERVED, so it cannot push the
        // combined footprint past either cap for the duration of its life.
        sweep_capture_retention(
            &self.dir,
            SweepReserve {
                slot: true,
                bytes: payload.len() as u64,
            },
        );
        self.ensure_capture_retention();
        let path = self.dir.join(terminal_capture_file_name(capture_id));
        let written = create_owner_only(&path).and_then(|mut file| file.write_all(payload));
        if written.is_err() {
            tracing::warn!(
                capture_id,
                byte_len = payload.len(),
                "diag.terminal_capture_write_failed"
            );
            return Err(TerminalCaptureErrorCode::StorageFailed);
        }
        tracing::info!(
            capture_id,
            byte_len = payload.len(),
            "diag.terminal_capture_written"
        );
        Ok(StoredCapture {
            path,
            byte_length: payload.len() as u64,
        })
    }

    /// Run the startup sweep and arm the hourly one. Idempotent.
    pub fn ensure_capture_retention(&self) {
        let mut retention = self
            .retention
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if retention.is_some() {
            return;
        }
        sweep_capture_retention(&self.dir, SweepReserve::default());
        let dir = self.dir.clone();
        *retention = Some(self.runtime.spawn(async move {
            let mut ticker = tokio::time::interval(RETENTION_SWEEP_INTERVAL);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                sweep_capture_retention(&dir, SweepReserve::default());
            }
        }));
        tracing::info!(dir = %self.dir.display(), "the terminal capture retention sweep was armed");
    }

    /// Process shutdown: the sweep is the only capture state that outlives a
    /// session.
    pub fn stop_capture_retention(&self) {
        let taken = self
            .retention
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = taken {
            task.abort();
            tracing::info!("the terminal capture retention sweep was stopped");
        }
    }

    /// Create the directory owner-only, and tighten one a looser umask (or an
    /// older worker) already created: raw PTY bytes must not land in a `0755`
    /// directory.
    fn ensure_owner_only_dir(&self) {
        let _ = create_dir_owner_only(&self.dir);
        let mut tightened = self
            .tightened
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *tightened {
            return;
        }
        // Not ours or gone: the write below reports the real failure, and the
        // memo stays unset so the next write checks again.
        match tighten_dir(&self.dir) {
            Ok(DirMode::AlreadyOwnerOnly) => *tightened = true,
            Ok(DirMode::Tightened(from_mode)) => {
                tracing::warn!(
                    dir = %self.dir.display(),
                    from_mode = format!("{from_mode:o}"),
                    "diag.capture_dir_tightened"
                );
                *tightened = true;
            }
            Err(_) => {}
        }
    }
}

/// What the owner-only check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirMode {
    AlreadyOwnerOnly,
    // Only Unix has a mode to tighten; elsewhere the check always finds the
    // directory owner-only.
    #[cfg_attr(not(unix), allow(dead_code))]
    Tightened(u32),
}

/// True for a name this owner created. Deletion is restricted to recognized
/// capture files: the directory also holds the keeper's and worker's own logs.
pub fn is_recognized_capture_file_name(name: &str) -> bool {
    is_terminal_capture_file_name(name)
        || (name.starts_with(LEGACY_BYTECAP_PREFIX) && name.ends_with(LEGACY_BYTECAP_SUFFIX))
}

/// Remove recognized files past the retention window, then enforce the
/// COMBINED file and byte caps oldest-first: two independent LRUs would each
/// believe they had the whole disk.
pub fn sweep_capture_retention(dir: &Path, reserve: SweepReserve) {
    let Some(stored) = stored_capture_files(dir) else {
        return;
    };
    let window = Duration::from_millis(TERMINAL_CAPTURE_LIMITS.retention_ms);
    let expired_before = SystemTime::now()
        .checked_sub(window)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut retained: Vec<(SystemTime, PathBuf, u64)> = Vec::with_capacity(stored.len());
    for (modified, path, size) in stored {
        if modified < expired_before && fs::remove_file(&path).is_ok() {
            continue;
        }
        retained.push((modified, path, size));
    }
    retained.sort_by_key(|(modified, _, _)| *modified);
    let mut total_bytes: u64 = retained.iter().map(|(_, _, size)| *size).sum();
    let file_cap = TERMINAL_CAPTURE_LIMITS.storage_files - usize::from(reserve.slot);
    let byte_cap = (TERMINAL_CAPTURE_LIMITS.storage_bytes as u64).saturating_sub(reserve.bytes);
    let mut live = retained.len();
    for (_, path, size) in &retained {
        if live <= file_cap && total_bytes <= byte_cap {
            break;
        }
        live -= 1;
        if fs::remove_file(path).is_ok() {
            total_bytes = total_bytes.saturating_sub(*size);
        }
    }
}

/// Every recognized file with its mtime and size, or `None` when the
/// directory cannot be read or a stat fails (v2 abandons the sweep then).
fn stored_capture_files(dir: &Path) -> Option<Vec<(SystemTime, PathBuf, u64)>> {
    let mut stored = Vec::new();
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(is_recognized_capture_file_name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::metadata(&path).ok()?;
        stored.push((metadata.modified().ok()?, path, metadata.len()));
    }
    Some(stored)
}

#[cfg(unix)]
fn create_dir_owner_only(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(CAPTURE_DIR_MODE)
        .create(dir)
}

#[cfg(not(unix))]
fn create_dir_owner_only(dir: &Path) -> std::io::Result<()> {
    let _ = CAPTURE_DIR_MODE;
    fs::create_dir_all(dir)
}

/// `mkdir`'s mode is ignored on an existing path, so an existing directory is
/// checked and tightened here.
#[cfg(unix)]
fn tighten_dir(dir: &Path) -> std::io::Result<DirMode> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(dir)?.permissions().mode() & 0o777;
    if mode == CAPTURE_DIR_MODE {
        return Ok(DirMode::AlreadyOwnerOnly);
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(CAPTURE_DIR_MODE))?;
    Ok(DirMode::Tightened(mode))
}

#[cfg(not(unix))]
fn tighten_dir(_dir: &Path) -> std::io::Result<DirMode> {
    Ok(DirMode::AlreadyOwnerOnly)
}

#[cfg(unix)]
fn create_owner_only(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(CAPTURE_FILE_MODE)
        .open(path)
}

#[cfg(not(unix))]
fn create_owner_only(path: &Path) -> std::io::Result<fs::File> {
    let _ = CAPTURE_FILE_MODE;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}
