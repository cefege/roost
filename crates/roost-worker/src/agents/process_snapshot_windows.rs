//! The process table on Windows, read through `sysinfo` where Linux and macOS
//! run `ps`: pid, parent, name and command line for every process. There are
//! no process groups or controlling terminals, so `pgid` and `tpgid` are 0.
//! Read by `agents::status_stack` (the scan's reader) and `host::ports` (a
//! session's descendants).

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::process_snapshot::{ProcessSnapshotReader, SNAPSHOT_ABORTED, ScanAbort};
use super::process_tree::ProcessRecord;
use crate::uplink::OwnerFuture;

/// v2 `_readProcessSnapshot` on Windows: one full process refresh, abandoned
/// on abort.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsSnapshotReader;

impl ProcessSnapshotReader for WindowsSnapshotReader {
    fn read(&self, abort: ScanAbort) -> OwnerFuture<Result<Vec<ProcessRecord>, String>> {
        Box::pin(async move {
            if abort.is_aborted() {
                return Err(SNAPSHOT_ABORTED.to_owned());
            }
            let snapshot = tokio::task::spawn_blocking(windows_process_records);
            // The blocking refresh cannot be interrupted; an abort only stops
            // anyone from waiting for it.
            tokio::select! {
                biased;
                () = abort.aborted() => Err(SNAPSHOT_ABORTED.to_owned()),
                records = snapshot => records.map_err(|error| error.to_string()),
            }
        })
    }
}

/// Every process on this host, blocking for the length of one refresh.
pub(crate) fn windows_process_records() -> Vec<ProcessRecord> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    system
        .processes()
        .values()
        .map(|process| {
            let name = process.name().to_string_lossy();
            let comm = name
                .strip_suffix(".exe")
                .or_else(|| name.strip_suffix(".EXE"))
                .unwrap_or(&name)
                .to_owned();
            let args = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            ProcessRecord {
                pid: process.pid().as_u32(),
                ppid: process.parent().map_or(0, sysinfo::Pid::as_u32),
                pgid: 0,
                tpgid: 0,
                comm,
                args,
            }
        })
        .collect()
}
