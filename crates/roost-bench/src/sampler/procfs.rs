//! Reading one process out of `/proc`, and naming its role from its command
//! line. Called by the sampler's tick; nothing here holds state.

use std::path::Path;

use crate::error::BenchError;
use crate::sampler::Role;

pub struct ProcStat {
    pub comm: String,
    pub cpu_ticks: u64,
    pub start_ticks: u64,
}

/// `/proc/<pid>/stat`: comm, utime + stime (fields 14, 15), starttime (22).
pub fn read_stat(pid: u32) -> Option<ProcStat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?.to_string();
    // Fields after the comm start at field 3 (state).
    let fields: Vec<&str> = text.get(close + 2..)?.split_whitespace().collect();
    let field = |number: usize| -> Option<u64> { fields.get(number - 3)?.parse().ok() };
    Some(ProcStat {
        comm,
        cpu_ticks: field(14)? + field(15)?,
        start_ticks: field(22)?,
    })
}

pub fn read_cmdline(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

pub fn read_environ(pid: u32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/environ")).ok()
}

pub fn read_rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

pub fn classify(cmdline: &[String], comm: &str) -> Role {
    let joined = cmdline.join(" ");
    let program = cmdline
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let verb = cmdline.get(1).map(String::as_str);
    if joined.contains("apps/coord/src/main.ts") {
        Role::Coord
    } else if joined.contains("apps/worker/src/main.ts") {
        Role::Worker
    } else if joined.contains("multiplexed-main")
        || program == "roost-keeper"
        || comm == "roost-keeper"
        || (program == "bun" && joined.contains("keeper"))
    {
        Role::Keeper
    } else if program == "roost" && verb == Some("coord") {
        Role::Coord
    } else if program == "roost" && verb == Some("worker") {
        Role::Worker
    } else if program.contains("chrome") || comm.starts_with("chrome") {
        // Chromium rewrites a child's title, so the flags may arrive as one string.
        if joined.contains("--type=renderer") {
            Role::BrowserRenderer
        } else {
            Role::BrowserOther
        }
    } else {
        Role::Workload
    }
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

pub fn clock_ticks_per_sec() -> Result<f64, BenchError> {
    let output = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .map_err(|error| BenchError::io("running `getconf CLK_TCK`", error))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|ticks| *ticks > 0.0)
        .ok_or_else(|| BenchError::Prerequisite("`getconf CLK_TCK` gave no tick rate".into()))
}
