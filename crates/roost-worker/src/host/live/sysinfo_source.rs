//! Host CPU, memory, uptime and process readings through `sysinfo`, for macOS
//! and Windows. One persistent [`sysinfo::System`] is refreshed on every sample,
//! because CPU usage is a difference between refreshes. Only the mapping from
//! sysinfo's process rows to the model is pure; tests drive it with synthetic rows.

use std::ffi::OsStr;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use super::{HostReading, LiveProcessSample};

/// The sysinfo-backed host reader. Holds the `System` so its CPU history survives between samples.
#[derive(Debug)]
pub(super) struct SysinfoSource {
    system: System,
}

impl SysinfoSource {
    pub(super) fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    pub(super) fn sample(&mut self) -> HostReading {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let processes = self
            .system
            .processes()
            .values()
            .map(|process| {
                model_process(
                    process.pid().as_u32(),
                    process.parent().map(Pid::as_u32),
                    process.name(),
                    process.cpu_usage(),
                    process.memory(),
                )
            })
            .collect();
        HostReading {
            cpu_pct: finite_pct(self.system.global_cpu_usage()),
            mem_total_bytes: self.system.total_memory(),
            mem_available_bytes: self.system.available_memory(),
            uptime_secs: System::uptime(),
            processes,
        }
    }
}

/// A global CPU share, clamped to 0–100. A non-finite reading is zero.
fn finite_pct(cpu_usage: f32) -> f64 {
    if cpu_usage.is_finite() {
        f64::from(cpu_usage).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// One model row from sysinfo's fields. `name` is the command name only: sysinfo
/// is asked for no command line, so no argv, environment or working directory is read.
/// A process with no parent reports ppid 0, the same as `/proc` does for init.
fn model_process(
    pid: u32,
    parent: Option<u32>,
    name: &OsStr,
    cpu_pct: f32,
    rss_bytes: u64,
) -> LiveProcessSample {
    LiveProcessSample {
        pid,
        ppid: parent.unwrap_or(0),
        name: name.to_string_lossy().into_owned(),
        cpu_pct: f64::from(cpu_pct),
        rss_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_process_maps_fields_and_widens_cpu() {
        let row = model_process(77, Some(5), OsStr::new("Finder"), 12.5, 4096);
        assert_eq!(
            row,
            LiveProcessSample {
                pid: 77,
                ppid: 5,
                name: "Finder".to_string(),
                cpu_pct: 12.5,
                rss_bytes: 4096,
            }
        );
    }

    #[test]
    fn model_process_without_parent_reports_ppid_zero() {
        let row = model_process(1, None, OsStr::new("launchd"), 0.0, 0);
        assert_eq!(row.ppid, 0);
    }

    #[test]
    fn cpu_above_one_core_is_kept_for_processes() {
        let row = model_process(2, Some(1), OsStr::new("node"), 240.0, 1);
        assert_eq!(row.cpu_pct, 240.0);
    }

    #[test]
    fn global_cpu_is_clamped_and_non_finite_reads_zero() {
        assert_eq!(finite_pct(-3.0), 0.0);
        assert_eq!(finite_pct(140.0), 100.0);
        assert_eq!(finite_pct(37.5), 37.5);
        assert_eq!(finite_pct(f32::NAN), 0.0);
    }
}
