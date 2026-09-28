//! One process-table snapshot: the `ps` reader, its parser, and the abort that
//! kills a snapshot nobody is waiting for any more. Ports the snapshot half of
//! v2 `apps/worker/src/agents/process-scan.ts` (`parsePsSnapshot`,
//! `_readProcessSnapshot`, the `AbortSignal` it honours). Read by
//! `agents::process_scan::AgentProcessScanner`; `ScanAbort` is also what
//! `agents::prompt_control` aborts at budget expiry.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

use super::process_tree::ProcessRecord;
use crate::uplink::OwnerFuture;

/// What an aborted snapshot fails with (v2's error text).
pub const SNAPSHOT_ABORTED: &str = "process snapshot aborted";

/// v2 `AbortSignal` for a process scan: once aborted it stays aborted, and
/// every waiter learns it.
#[derive(Clone, Debug, Default)]
pub struct ScanAbort {
    inner: Arc<AbortState>,
}

#[derive(Debug, Default)]
struct AbortState {
    aborted: AtomicBool,
    notify: Notify,
}

impl ScanAbort {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        if !self.inner.aborted.swap(true, Ordering::AcqRel) {
            tracing::debug!("a process scan was aborted");
        }
        self.inner.notify.notify_waiters();
    }

    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::Acquire)
    }

    /// Resolves once [`ScanAbort::abort`] has been called.
    pub async fn aborted(&self) {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_aborted() {
                return;
            }
            notified.await;
        }
    }
}

/// v2 `ProcessSnapshotReader`: the whole process table, or why not.
pub trait ProcessSnapshotReader: Send + Sync {
    fn read(&self, abort: ScanAbort) -> OwnerFuture<Result<Vec<ProcessRecord>, String>>;
}

/// v2 `_readProcessSnapshot` on Linux and macOS: one
/// `ps -A -o pid=,ppid=,pgid=,tpgid=,comm=,args=`, killed on abort.
#[derive(Debug, Clone)]
pub struct PsSnapshotReader {
    program: PathBuf,
}

impl Default for PsSnapshotReader {
    fn default() -> Self {
        Self {
            program: PathBuf::from("ps"),
        }
    }
}

impl PsSnapshotReader {
    /// A reader that runs `program` instead of `ps` from `PATH`, so a test can
    /// stand in a `ps` that never answers without editing the environment.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub async fn read_snapshot(&self, abort: ScanAbort) -> Result<Vec<ProcessRecord>, String> {
        if abort.is_aborted() {
            return Err(SNAPSHOT_ABORTED.to_owned());
        }
        let child = tokio::process::Command::new(&self.program)
            .args(["-A", "-o", "pid=,ppid=,pgid=,tpgid=,comm=,args="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        // Dropping the output future drops the child, and `kill_on_drop`
        // SIGKILLs it: an aborted snapshot leaves no `ps` behind.
        let output = tokio::select! {
            biased;
            () = abort.aborted() => return Err(SNAPSHOT_ABORTED.to_owned()),
            output = child.wait_with_output() => output.map_err(|error| error.to_string())?,
        };
        if abort.is_aborted() {
            return Err(SNAPSHOT_ABORTED.to_owned());
        }
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(if stderr.is_empty() {
                format!(
                    "ps exited {}",
                    output
                        .status
                        .code()
                        .map_or_else(|| "by signal".to_owned(), |code| code.to_string())
                )
            } else {
                stderr
            });
        }
        Ok(parse_ps_snapshot(&String::from_utf8_lossy(&output.stdout)))
    }
}

impl ProcessSnapshotReader for PsSnapshotReader {
    fn read(&self, abort: ScanAbort) -> OwnerFuture<Result<Vec<ProcessRecord>, String>> {
        let reader = self.clone();
        Box::pin(async move { reader.read_snapshot(abort).await })
    }
}

/// v2 `parsePsSnapshot`: `^\s*(\d+)\s+(\d+)\s+(-?\d+)\s+(-?\d+)\s+(\S+)\s*(.*)$`
/// per line; a line that does not match is skipped.
pub fn parse_ps_snapshot(output: &str) -> Vec<ProcessRecord> {
    output.split('\n').filter_map(parse_ps_line).collect()
}

fn parse_ps_line(line: &str) -> Option<ProcessRecord> {
    let mut rest = line.trim_start();
    let pid = take_number(&mut rest, false)?;
    let ppid = take_number(&mut rest, false)?;
    let pgid = take_number(&mut rest, true)?;
    let tpgid = take_number(&mut rest, true)?;
    let comm_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    if comm_end == 0 {
        return None;
    }
    let comm = rest[..comm_end].to_owned();
    let args = rest[comm_end..].trim_start().to_owned();
    Some(ProcessRecord {
        pid: u32::try_from(pid).ok()?,
        ppid: u32::try_from(ppid).ok()?,
        pgid: i32::try_from(pgid).ok()?,
        tpgid: i32::try_from(tpgid).ok()?,
        comm,
        args,
    })
}

/// One `\d+` (or `-?\d+`) field followed by at least one whitespace.
fn take_number(rest: &mut &str, signed: bool) -> Option<i64> {
    let negative = signed && rest.starts_with('-');
    let digits_start = usize::from(negative);
    let digits_end = rest[digits_start..]
        .find(|character: char| !character.is_ascii_digit())
        .map_or(rest.len(), |end| end + digits_start);
    if digits_end == digits_start {
        return None;
    }
    let value: i64 = rest[..digits_end].parse().ok()?;
    let after = &rest[digits_end..];
    let trimmed = after.trim_start();
    if trimmed.len() == after.len() {
        return None;
    }
    *rest = trimmed;
    Some(value)
}
