//! Durable, atomic replacement of an installed file, and the read that tells
//! "not there" apart from "there but unreadable". Every write an install or a
//! deploy performs goes through here.
//!
//! Both halves exist for the same reason. A service manager reads the file it
//! was pointed at, so a definition that is half-written when the machine loses
//! power is a service that never loads and an operator with no way back. And a
//! deploy that cannot read the definition already installed is a deploy that
//! cannot record a rollback point, so it must refuse rather than overwrite the
//! one copy of the previous state.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::services::definition_text::staging_name;

/// How many staging names to try before giving up. Each attempt either wins
/// the name or finds someone else's file there, so a large bound is only ever
/// reached on a pathological filesystem.
const STAGING_ATTEMPTS: u32 = 64;

/// What is on disk at a path an install or a deploy is about to replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledFile {
    /// Nothing is there. An install writes the first definition; a rollback
    /// removes what it wrote.
    Absent,
    /// Something is there and was read in full, permissions included.
    Present {
        /// The bytes, exactly as they will be restored from.
        bytes: Vec<u8>,
        /// The file's permission bits, which a stale install may have wrong.
        mode: u32,
    },
}

/// Read what is installed, distinguishing an absent path — the normal state of
/// a first install — from one that is present but cannot be read. The second is
/// an error rather than a fallback, and refusing on it is the whole point:
/// "could not read it" and "there was nothing there" are different answers and
/// a deploy that confuses them destroys the rollback point.
pub fn read_installed_file(path: &Path) -> Result<InstalledFile, std::io::Error> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstalledFile::Absent);
        }
        Err(error) => return Err(error),
    };
    let mode = fs::metadata(path)?.permissions().mode() & 0o7777;
    Ok(InstalledFile::Present { bytes, mode })
}

/// Replace `path` with `bytes`, atomically and durably: the content is written
/// to a fresh file beside the target, flushed, and renamed over it, and the
/// directory is flushed afterwards so the rename itself survives a power cut.
pub fn write_durable(path: &Path, bytes: &[u8], mode: u32) -> Result<(), std::io::Error> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} has no parent directory to stage in", path.display()),
        )
    })?;
    // A macOS account that has never launched a daemon has no `LaunchAgents`
    // directory, and a definition staged into a directory that does not exist
    // fails on the very first install of a machine.
    fs::create_dir_all(parent)?;
    let (staged, mut file) = create_staging_file(path)?;
    let staged_result = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)))
        .and_then(|()| file.sync_all());
    // The descriptor is closed before the rename, so the rename cannot land
    // while the content is still buffered.
    drop(file);
    match staged_result {
        Ok(()) => {}
        Err(error) => {
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
    }
    if let Err(error) = fs::rename(&staged, path) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    sync_directory(parent)
}

/// The permission bits a staged file already has, so an install that finds the
/// target correct does not rewrite it and a test can see the tree is untouched.
pub fn current_mode(path: &Path) -> Option<u32> {
    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o7777)
}

/// Create a staging file beside `target` and return its path with its open
/// descriptor. The name is per-process and per-attempt rather than a fixed
/// suffix, so two installs racing on one machine cannot write through the same
/// temporary file.
fn create_staging_file(target: &Path) -> Result<(PathBuf, File), std::io::Error> {
    let file_name = target.file_name().map_or_else(
        || "roost-install".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let mut last_error = None;
    for attempt in 0..STAGING_ATTEMPTS {
        let candidate = parent.join(staging_name(&file_name, attempt));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("no staging name beside {} was free", target.display()),
        )
    }))
}

fn sync_directory(parent: &Path) -> Result<(), std::io::Error> {
    File::open(parent)?.sync_all()
}
