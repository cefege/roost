//! Durable snapshots for worker file mutations, keyed by conversation and user turn.
//! The manifest stores absolute paths so a later rewind is independent of cwd.
//! Checkpoint directories expire after seven days and close with their conversation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    entries: Vec<ManifestEntry>,
}

/// One path's state before its first change in a checkpoint: the content in
/// `backup` when it `existed`, else the file is deleted on restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntry {
    path: PathBuf,
    existed: bool,
    backup: Option<String>,
}

fn conversation_path(root: &Path, conversation_id: &str) -> PathBuf {
    root.join("agent-tools/checkpoints").join(conversation_id)
}
fn checkpoint_path(root: &Path, conversation_id: &str, checkpoint_id: u64) -> PathBuf {
    conversation_path(root, conversation_id).join(checkpoint_id.to_string())
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    pub deleted: Vec<String>,
}

/// Records a path's pre-mutation state once for a conversation checkpoint.
pub fn record_before_change(
    root: &Path,
    conversation_id: &str,
    checkpoint_id: u64,
    path: &Path,
) -> Result<(), String> {
    let absolute = absolute_path(path)?;
    let directory = checkpoint_path(root, conversation_id, checkpoint_id);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let manifest_path = directory.join("manifest.json");
    let mut manifest = read_manifest(&manifest_path)?;
    if manifest.entries.iter().any(|entry| entry.path == absolute) {
        return Ok(());
    }
    let prior = match std::fs::read(&absolute) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let backup = if let Some(bytes) = prior {
        let name = format!("{}.bak", manifest.entries.len());
        std::fs::write(directory.join(&name), bytes).map_err(|error| error.to_string())?;
        Some(name)
    } else {
        None
    };
    manifest.entries.push(ManifestEntry {
        path: absolute,
        existed: backup.is_some(),
        backup,
    });
    write_manifest(&manifest_path, &manifest)
}

/// Restores paths changed by this or any later checkpoint, newest first.
pub fn restore_from(
    root: &Path,
    conversation_id: &str,
    checkpoint_id: u64,
) -> Result<RestoreReport, String> {
    let directory = conversation_path(root, conversation_id);
    let mut checkpoints = Vec::new();
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RestoreReport::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(name) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
        else {
            continue;
        };
        if name >= checkpoint_id && entry.path().is_dir() {
            checkpoints.push((name, entry.path()));
        }
    }
    checkpoints.sort_by_key(|checkpoint| std::cmp::Reverse(checkpoint.0));
    let mut final_states = BTreeMap::new();
    for (_, path) in checkpoints {
        let manifest_path = path.join("manifest.json");
        let manifest = read_manifest(&manifest_path)?;
        for item in manifest.entries {
            if item.existed {
                let backup = item
                    .backup
                    .ok_or_else(|| "restore manifest has no backup".to_owned())?;
                let bytes = std::fs::read(path.join(backup)).map_err(|error| error.to_string())?;
                if let Some(parent) = item.path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                std::fs::write(&item.path, bytes).map_err(|error| error.to_string())?;
            } else {
                match std::fs::remove_file(&item.path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            final_states.insert(item.path, item.existed);
        }
    }
    let mut report = RestoreReport::default();
    for (path, existed) in final_states {
        if existed {
            report.restored.push(path.display().to_string());
        } else {
            report.deleted.push(path.display().to_string());
        }
    }
    Ok(report)
}

/// Removes all durable restore points when the worker conversation closes.
pub fn close_conversation(root: &Path, conversation_id: &str) -> Result<(), String> {
    remove_directory(&conversation_path(root, conversation_id))
}

/// Deletes expired conversation checkpoint trees; called once by the tool host.
pub fn prune_expired(root: &Path) -> Result<(), String> {
    let checkpoints = root.join("agent-tools/checkpoints");
    let conversations = match std::fs::read_dir(&checkpoints) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let now = SystemTime::now();
    for conversation in conversations {
        let conversation = conversation.map_err(|error| error.to_string())?;
        if !conversation.path().is_dir() {
            continue;
        }
        for checkpoint in
            std::fs::read_dir(conversation.path()).map_err(|error| error.to_string())?
        {
            let checkpoint = checkpoint.map_err(|error| error.to_string())?;
            let metadata = checkpoint.metadata().map_err(|error| error.to_string())?;
            let expired = now
                .duration_since(metadata.modified().unwrap_or(now))
                .unwrap_or_default()
                > RETENTION;
            if expired {
                remove_directory(&checkpoint.path())?;
            }
        }
        if std::fs::read_dir(conversation.path())
            .map_err(|error| error.to_string())?
            .next()
            .is_none()
        {
            remove_directory(&conversation.path())?;
        }
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| error.to_string())
    }
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Manifest {
            entries: Vec::new(),
        }),
        Err(error) => Err(error.to_string()),
    }
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let bytes = serde_json::to_vec(manifest).map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}

fn remove_directory(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
