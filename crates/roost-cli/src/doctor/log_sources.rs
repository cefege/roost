//! Which files `roost doctor` reads, and how one JSON line comes out of each.
//! Called by doctor/mod.rs. Every path comes from `roost-host`, so the digest
//! reads the same log directory the services write and cannot drift onto a
//! second naming convention.
//!
//! Rotations are decompressed through `gzip -dc` as a subprocess rather than
//! through an in-process decoder. Both target platforms ship gzip, the call is
//! one line, and it keeps a compression library out of a binary whose only
//! reader is a daily review — the same reason the journal readers shell out to
//! `systemctl` rather than linking a service-manager client.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use roost_host::{EnvSource, HostPlatform, coord_log_dir, worker_log_dir};
use serde_json::Value;

/// The always-on channel. `main.err.log` is where the service managers point
/// stderr, and it is the only file with a bounded volume: `main.out.log` holds
/// the `diag()` firehose, which is per-keystroke on a busy session and would
/// turn a daily digest into a data dump.
pub const ERR_LOG_BASE: &str = "main.err.log";
/// The keeper's own stderr, wired by the worker when it spawns the keeper. The
/// keeper is the process that owns PTYs, so its complaints are the ones an
/// operator most needs and they land in a different file from the worker's.
pub const KEEPER_ERR_LOG_BASE: &str = "keeper.err.log";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSource {
    pub app: String,
    pub dir: PathBuf,
    pub base: String,
}

/// The three files the digest reads. Remote workers keep their own signals on
/// their own host until central logging lands, so this is a LOCAL digest and
/// says so in its heading.
pub fn sources(env: &dyn EnvSource, platform: HostPlatform) -> Vec<LogSource> {
    let coord = coord_log_dir(env, platform);
    let worker = worker_log_dir(env, platform);
    let mut out = Vec::new();
    if let Ok(dir) = coord {
        out.push(LogSource {
            app: "coord".to_string(),
            dir,
            base: ERR_LOG_BASE.to_string(),
        });
    }
    if let Ok(dir) = worker {
        out.push(LogSource {
            app: "worker".to_string(),
            dir: dir.clone(),
            base: ERR_LOG_BASE.to_string(),
        });
        out.push(LogSource {
            app: "keeper".to_string(),
            dir,
            base: KEEPER_ERR_LOG_BASE.to_string(),
        });
    }
    out
}

/// A source's base file plus its rotations, sorted so two runs of one window
/// read the same bytes in the same order. A source with no file at all
/// contributes nothing, and the caller records it as "never ran here".
pub fn log_files_for(source: &LogSource) -> Vec<PathBuf> {
    let prefix = format!("{}.", source.base);
    let Ok(entries) = std::fs::read_dir(&source.dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == source.base || name.starts_with(&prefix))
        })
        .collect();
    files.sort();
    files
}

/// Stream one file's parsed JSON lines. A line that is not JSON is skipped
/// rather than failing the digest: a truncated final line after a hard kill is
/// the normal shape of these files, and refusing to review the window because
/// its last line is half-written would hide every anomaly before it.
pub fn for_each_log_line(path: &Path, visit: &mut impl FnMut(Value)) {
    if path.extension().and_then(|suffix| suffix.to_str()) == Some("gz") {
        visit_gunzipped(path, visit);
    } else {
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        visit_lines(file, visit);
    }
}

fn visit_gunzipped(path: &Path, visit: &mut impl FnMut(Value)) {
    let Ok(mut child) = Command::new("gzip")
        .arg("-dc")
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
    else {
        tracing::warn!(
            target: "doctor",
            msg = "rotation_unreadable",
            fields = path.display().to_string(),
        );
        return;
    };
    if let Some(stdout) = child.stdout.take() {
        visit_lines(stdout, visit);
    }
    let _ = child.wait();
}

fn visit_lines(reader: impl Read, visit: &mut impl FnMut(Value)) {
    for line in BufReader::new(reader).lines() {
        let Ok(text) = line else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            visit(value);
        }
    }
}
