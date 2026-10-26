//! The root volume's used and total bytes, read through `sysinfo`'s disk list
//! on every OS, so no sample shells out to `df`. The root is `/` on unix and the
//! `SystemDrive` volume (`C:` when unset) on Windows. Called by
//! [`super::LiveSampler::sample`]; the disk list is kept across samples and
//! refreshed each time.

use std::path::PathBuf;

use sysinfo::Disks;

/// The root volume, read through a persistent disk list.
#[derive(Debug)]
pub(super) struct RootDisk {
    disks: Disks,
    root: PathBuf,
}

impl RootDisk {
    pub(super) fn new() -> Self {
        Self {
            disks: Disks::new_with_refreshed_list(),
            root: root_mount(),
        }
    }

    /// (used, total) bytes of the root volume, zeros when it is not listed.
    pub(super) fn sample(&mut self) -> (u64, u64) {
        self.disks.refresh(true);
        self.disks
            .list()
            .iter()
            .find(|disk| disk.mount_point() == self.root.as_path())
            .map_or((0, 0), |disk| {
                (
                    disk.total_space().saturating_sub(disk.available_space()),
                    disk.total_space(),
                )
            })
    }
}

/// Windows installs onto the volume named by `SystemDrive`, so the root follows
/// the install rather than assuming `C:`.
#[cfg(windows)]
fn root_mount() -> PathBuf {
    let drive = std::env::var("SystemDrive")
        .ok()
        .map(|drive| drive.trim().to_owned())
        .filter(|drive| !drive.is_empty())
        .unwrap_or_else(|| "C:".to_owned());
    PathBuf::from(format!("{drive}\\"))
}

#[cfg(not(windows))]
fn root_mount() -> PathBuf {
    PathBuf::from("/")
}
