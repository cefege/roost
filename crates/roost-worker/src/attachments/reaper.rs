//! Bounds what attachments may cost a machine: once at boot and then hourly,
//! files older than 7 days are deleted and everything together — the private
//! session directories and every registered project media directory — is
//! held under 1 GiB by evicting the oldest survivors. Dedup manifests and
//! media `.gitignore`s are never swept; operation temps and shortcuts are.
//! Ports the sweep of v2 `apps/worker/src/attachments/attachment-reaper.ts`.
//! Started by `runtime::owners`; a missing base is a quiet no-op.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;

use super::media_dirs::{MEDIA_GITIGNORE_NAME, is_project_media_dir, is_real_media_dir};
use super::store_paths::{
    ATTACHMENT_OPERATION_DIR_NAME, AttachmentBase, MANIFEST_NAME, SHORTCUT_DIR_NAME,
};

/// A file older than this is deleted. Long enough that an agent conversation
/// resumed days later still finds the screenshots it was shown.
pub const ATTACHMENT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The most every session's files together may hold.
pub const ATTACHMENT_SIZE_CAP_BYTES: u64 = 1024 * 1024 * 1024;

pub const ATTACHMENT_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// What one sweep did, for its log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepSummary {
    pub expired: usize,
    pub evicted: usize,
    pub retained_bytes: u64,
}

struct Survivor {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

#[derive(Default)]
struct Sweep {
    survivors: Vec<Survivor>,
    total: u64,
    summary: SweepSummary,
}

/// Sweep now — so a worker that was down for a day does not carry stale files
/// — and then every hour. The handle is the composition's to abort.
pub fn start_attachment_reaper(base: AttachmentBase) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(ATTACHMENT_SWEEP_INTERVAL);
        loop {
            ticks.tick().await;
            let base = base.clone();
            let swept =
                tokio::task::spawn_blocking(move || sweep_attachments(&base, SystemTime::now()))
                    .await;
            match swept {
                Ok(Ok(summary)) => tracing::info!(
                    expired = summary.expired,
                    evicted = summary.evicted,
                    retained_bytes = summary.retained_bytes,
                    "the attachment reaper swept"
                ),
                Ok(Err(error)) => tracing::warn!(%error, "attachment reaper sweep_failed"),
                Err(error) => tracing::warn!(%error, "the attachment reaper sweep did not finish"),
            }
        }
    })
}

/// One sweep of every session directory beneath the base and every registered
/// project media directory, as of `now`.
pub fn sweep_attachments(base: &AttachmentBase, now: SystemTime) -> io::Result<SweepSummary> {
    let mut sweep = Sweep::default();
    let root = base.root();
    if root.exists() {
        for session_dir in entries(root)? {
            if !fs::metadata(&session_dir).is_ok_and(|metadata| metadata.is_dir()) {
                continue;
            }
            sweep_media_dir(&session_dir, now, &mut sweep);
            remove_if_empty(&session_dir);
        }
    }
    let registry = base.media_registry();
    for media_dir in registry.registered() {
        if !is_project_media_dir(&media_dir) {
            continue;
        }
        if !media_dir.exists() {
            if let Err(error) = registry.forget(&media_dir) {
                tracing::warn!(dir = %media_dir.display(), %error, "attachment reaper could not forget a vanished media directory");
            }
            continue;
        }
        if !is_real_media_dir(&media_dir) {
            tracing::warn!(dir = %media_dir.display(), "attachment reaper skipped a media directory that is now a symlink");
            continue;
        }
        sweep_media_dir(&media_dir, now, &mut sweep);
    }
    evict_oldest(&mut sweep);
    sweep.summary.retained_bytes = sweep.total;
    Ok(sweep.summary)
}

fn sweep_media_dir(dir: &Path, now: SystemTime, sweep: &mut Sweep) {
    let Ok(names) = entries(dir) else {
        return;
    };
    for path in names {
        match file_name(&path) {
            Some(MANIFEST_NAME | MEDIA_GITIGNORE_NAME) => {}
            Some(ATTACHMENT_OPERATION_DIR_NAME) => sweep_operations(&path, now, sweep),
            Some(SHORTCUT_DIR_NAME) => sweep_shortcuts(&path, now, sweep),
            _ => sweep_file(&path, now, sweep, true),
        }
    }
}

/// Shortcuts count against the cap like any file, so the TTL and the 1 GiB
/// bound stay truthful however an upload was answered.
fn sweep_shortcuts(dir: &Path, now: SystemTime, sweep: &mut Sweep) {
    let Ok(shortcuts) = entries(dir) else {
        return;
    };
    for shortcut in shortcuts {
        sweep_file(&shortcut, now, sweep, true);
    }
    remove_if_empty(dir);
}

/// An expired journal or temp is deleted; only a live temp counts against the cap.
fn sweep_operations(dir: &Path, now: SystemTime, sweep: &mut Sweep) {
    let Ok(operation_files) = entries(dir) else {
        return;
    };
    for path in operation_files {
        let counts = path
            .extension()
            .is_some_and(|extension| extension == "part");
        sweep_file(&path, now, sweep, counts);
    }
    remove_if_empty(dir);
}

/// Delete a regular file past its TTL, or keep it as a survivor. The stat
/// follows a symlink, as v2's does: a shortcut ages with its target.
fn sweep_file(path: &Path, now: SystemTime, sweep: &mut Sweep, counts: bool) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if !metadata.is_file() {
        return;
    }
    let modified = metadata.modified().unwrap_or(now);
    let expired = now
        .duration_since(modified)
        .is_ok_and(|age| age > ATTACHMENT_TTL);
    if expired {
        match fs::remove_file(path) {
            Ok(()) => sweep.summary.expired += 1,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "attachment reaper unlink_failed")
            }
        }
        return;
    }
    if counts {
        sweep.total += metadata.len();
        sweep.survivors.push(Survivor {
            path: path.to_path_buf(),
            size: metadata.len(),
            modified,
        });
    }
}

/// Least recently modified first, until the total fits under the cap.
fn evict_oldest(sweep: &mut Sweep) {
    if sweep.total <= ATTACHMENT_SIZE_CAP_BYTES {
        return;
    }
    sweep.survivors.sort_by_key(|survivor| survivor.modified);
    for victim in &sweep.survivors {
        if sweep.total <= ATTACHMENT_SIZE_CAP_BYTES {
            break;
        }
        match fs::remove_file(&victim.path) {
            Ok(()) => {
                sweep.total -= victim.size;
                sweep.summary.evicted += 1;
                tracing::info!(path = %victim.path.display(), size = victim.size, "attachment reaper lru_evicted");
            }
            Err(error) => tracing::warn!(%error, "attachment reaper lru_unlink_failed"),
        }
    }
}

fn entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect())
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

/// A concurrent upload may have refilled it; that is not an error.
fn remove_if_empty(dir: &Path) {
    if fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(dir);
    }
}
