//! Bounded, allowlisted Tailscale CLI invocation for the Serve controller.
//!
//! No shell, caller-provided executable, or subprocess output enters errors.
//! Captured status is bounded and discarded after parsing.

use super::TailscaleError;
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: u64 = 1_048_576;

pub(super) enum Operation {
    Version,
    Status,
    ServeStatus,
    Mount(u16),
    Unmount,
}

pub(super) fn execute(operation: Operation) -> Result<Vec<u8>, TailscaleError> {
    let (arguments, seconds): (Vec<String>, u64) = match operation {
        Operation::Version => (vec!["version".into()], 10),
        Operation::Status => (vec!["status".into(), "--json".into()], 10),
        Operation::ServeStatus => (vec!["serve".into(), "status".into(), "--json".into()], 10),
        Operation::Mount(port) => (
            vec![
                "serve".into(),
                "--bg".into(),
                "--https=443".into(),
                "--set-path=/".into(),
                format!("http://127.0.0.1:{port}"),
            ],
            30,
        ),
        Operation::Unmount => (
            vec![
                "serve".into(),
                "--https=443".into(),
                "--set-path=/".into(),
                "off".into(),
            ],
            10,
        ),
    };
    let mut child = Command::new("tailscale")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TailscaleError::NotInstalled
            } else {
                TailscaleError::CommandFailed
            }
        })?;
    let stdout = child.stdout.take().ok_or(TailscaleError::CommandFailed)?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TailscaleError::CommandTimedOut);
            }
        }
    };
    if !status.success() {
        return Err(TailscaleError::CommandFailed);
    }
    // A descendant retaining the pipe must not turn the bounded CLI call into an unbounded join.
    while !reader.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !reader.is_finished() {
        return Err(TailscaleError::CommandTimedOut);
    }
    let output = reader
        .join()
        .map_err(|_| TailscaleError::InvalidResponse)?
        .map_err(|_| TailscaleError::InvalidResponse)?;
    if output.len() as u64 > OUTPUT_LIMIT {
        return Err(TailscaleError::InvalidResponse);
    }
    Ok(output)
}
