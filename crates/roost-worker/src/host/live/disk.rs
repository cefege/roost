//! The root volume's used and total bytes, read through `sysinfo`'s disk list
//! on every OS, so no sample shells out to `df`. The mount point is `/` on unix
//! and `C:\` on Windows. Called by [`super::LiveSampler::sample`]; the disk list is
//! kept across samples and refreshed each time.

use std::path::Path;

use sysinfo::Disks;

#[cfg(windows)]
const ROOT_MOUNT: &str = "C:\\";
#[cfg(not(windows))]
const ROOT_MOUNT: &str = "/";

/// The root volume, read through a persistent disk list.
#[derive(Debug)]
pub(super) struct RootDisk {
    disks: Disks,
}

impl RootDisk {
    pub(super) fn new() -> Self {
        Self {
            disks: Disks::new_with_refreshed_list(),
        }
    }

    /// (used, total) bytes of the root volume, zeros when it is not listed.
    pub(super) fn sample(&mut self) -> (u64, u64) {
        self.disks.refresh(true);
        self.disks
            .list()
            .iter()
            .find(|disk| disk.mount_point() == Path::new(ROOT_MOUNT))
            .map_or((0, 0), |disk| {
                (
                    disk.total_space().saturating_sub(disk.available_space()),
                    disk.total_space(),
                )
            })
    }
}
