//! The raw counters a heartbeat ships, read on Windows through `sysinfo`: CPU,
//! memory, the system drive, and interface bytes. The Windows half of
//! [`super::samples::HostSampler`] (Linux reads `/proc`, macOS
//! [`super::samples_darwin`]). Never fails: an unreadable counter is a zero.

use std::path::Path;

use sysinfo::{Disks, Networks, System};

use super::{HostSample, NetCounters};

/// The drive Windows booted from, when `SystemDrive` names none.
const DEFAULT_SYSTEM_DRIVE: &str = "C:";

/// One sampler's `sysinfo` handles. Stateful because CPU usage is a difference
/// between two refreshes: the first sample after a worker starts reads 0.
pub(super) struct WindowsSampler {
    system: Option<System>,
    disks: Option<Disks>,
    networks: Option<Networks>,
}

impl std::fmt::Debug for WindowsSampler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsSampler")
            .finish_non_exhaustive()
    }
}

impl WindowsSampler {
    pub(super) fn new() -> Self {
        Self {
            system: System::new().ok(),
            disks: Disks::new_with_refreshed_list().ok(),
            networks: Networks::new_with_refreshed_list().ok(),
        }
    }

    pub(super) fn sample(&mut self) -> HostSample {
        if let Some(system) = self.system.as_mut() {
            system.refresh_cpu_usage();
            system.refresh_memory();
        }
        if let Some(disks) = self.disks.as_mut() {
            disks.refresh(true);
        }
        if let Some(networks) = self.networks.as_mut() {
            networks.refresh(true);
        }
        let cpu_pct = self.system.as_ref().map_or(0.0, |system| {
            f64::from(system.global_cpu_usage().round()).clamp(0.0, 100.0)
        });
        let drive = std::env::var("SystemDrive")
            .ok()
            .filter(|drive| !drive.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SYSTEM_DRIVE.to_string());
        let root = format!("{drive}\\");
        let (disk_used_bytes, disk_total_bytes) = self
            .disks
            .as_ref()
            .and_then(|disks| {
                disks
                    .iter()
                    .find(|disk| disk.mount_point() == Path::new(&root))
            })
            .map_or((0, 0), |disk| {
                (
                    disk.total_space().saturating_sub(disk.available_space()),
                    disk.total_space(),
                )
            });
        let net = self.networks.as_ref().map(|networks| {
            networks
                .iter()
                .filter(|(name, _)| !name.to_string_lossy().contains("Loopback"))
                .fold(NetCounters::default(), |total, (_, data)| NetCounters {
                    rx_bytes: total.rx_bytes.saturating_add(data.total_received()),
                    tx_bytes: total.tx_bytes.saturating_add(data.total_transmitted()),
                })
        });
        HostSample {
            cpu_pct,
            mem_used_bytes: self.system.as_ref().map_or(0, System::used_memory),
            mem_total_bytes: self.system.as_ref().map_or(0, System::total_memory),
            disk_used_bytes,
            disk_total_bytes,
            net,
        }
    }
}
