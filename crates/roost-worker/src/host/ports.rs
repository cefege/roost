//! The TCP ports a session's process tree is LISTENing on, so the sidebar can
//! chip a dev server and open it. Only this worker can see the host's sockets.
//! Depends on `host::tool_path` for the bounded runner, on `roost_host` for the
//! platform, and on nothing here.
//!
//! ONLY REACHABLE BINDS. A port is reported only when it is bound on something
//! other than loopback, because the chip opens `http://<worker's address>:<port>`
//! from another device: a `127.0.0.1` vite, a language server or a debugger
//! answers nothing there, and the chip was always a dead link. A localhost-only
//! dev server now shows no chip, which is true rather than disappointing.
//!
//! THE PARSERS ARE SEPARATED FROM THE READER because the reader needs `ps`,
//! `ss` or `lsof` and the interesting failures are all in the parsing: a
//! loopback row that leaks through, a `pid=` that belongs to somebody else.

use std::collections::{BTreeMap, BTreeSet};

use roost_host::HostPlatform;

use super::tool_path::run;

/// Every descendant pid of `root`, itself included, from one `ps` snapshot.
#[must_use]
pub fn descendant_pids(root: u32) -> Vec<u32> {
    let Some(snapshot) = run("ps", &["-Ao", "pid,ppid"], None) else {
        return vec![root];
    };
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for line in snapshot.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(parent)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let (Ok(pid), Ok(parent)) = (pid.parse::<u32>(), parent.parse::<u32>()) {
            children.entry(parent).or_default().push(pid);
        }
    }
    let mut seen = BTreeSet::from([root]);
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        for child in children.get(&pid).into_iter().flatten() {
            if seen.insert(*child) {
                pending.push(*child);
            }
        }
    }
    seen.into_iter().collect()
}

/// The reachable LISTEN ports in `lsof -nP -iTCP -sTCP:LISTEN` output.
///
/// Ascending and distinct. The host is the token before the last `:` in the
/// NAME column; `lsof` runs numeric, so it is never a DNS name.
#[must_use]
pub fn parse_reachable_listen_ports(lsof_output: &str) -> Vec<u16> {
    let mut ports = BTreeSet::new();
    for line in lsof_output.lines() {
        if let Some(port) = reachable_bind(line) {
            ports.insert(port);
        }
    }
    ports.into_iter().collect()
}

/// The reachable LISTEN ports in `ss -ltnpH` rows held by `pids`.
///
/// `ss` has no pid selector, so the tree filter happens here rather than in the
/// command line — a session's chips must not become the host's port list.
#[must_use]
pub fn parse_ss_listen_ports(output: &str, pids: &BTreeSet<u32>) -> Vec<u16> {
    let mut ports = BTreeSet::new();
    for line in output.lines() {
        let Some(local) = line.split_whitespace().nth(3) else {
            continue;
        };
        let Some(cut) = local.rfind(':') else {
            continue;
        };
        let (host, port) = (&local[..cut], &local[cut + 1..]);
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        if port == 0 || is_loopback(host) {
            continue;
        }
        if !line
            .split_whitespace()
            .filter_map(|field| field.strip_prefix("pid="))
            .filter_map(|pid| pid.trim_end_matches([')', ',']).parse::<u32>().ok())
            .any(|pid| pids.contains(&pid))
        {
            continue;
        }
        ports.insert(port);
    }
    ports.into_iter().collect()
}

/// The port of a `…:PORT (LISTEN)` bind, or `None` when the line is not one,
/// or when the bind is loopback-only.
fn reachable_bind(line: &str) -> Option<u16> {
    let name = line.split_whitespace().last()?;
    let name = name.strip_suffix("(LISTEN)")?.trim_end();
    let cut = name.rfind(':')?;
    let (host, port) = (&name[..cut], &name[cut + 1..]);
    let port = port.parse::<u16>().ok()?;
    if port == 0 || host.is_empty() || is_loopback(host) {
        return None;
    }
    Some(port)
}

/// Whether a bind address answers only on this machine.
fn is_loopback(host: &str) -> bool {
    matches!(host, "[::1]" | "::1") || host.starts_with("127.")
}

/// The reachable LISTEN ports held by `root_pid`'s process tree.
///
/// `None` for a session with no pid yet, which is a session that has not
/// spawned: there is no tree to ask about, and that is not a failure.
#[must_use]
pub fn read_listening_ports(root_pid: Option<u32>, platform: HostPlatform) -> Vec<u16> {
    let Some(root) = root_pid.filter(|pid| *pid > 0) else {
        return Vec::new();
    };
    let pids: BTreeSet<u32> = descendant_pids(root).into_iter().collect();
    let ports = match platform {
        HostPlatform::Linux => run("/usr/sbin/ss", &["-ltnpH"], None)
            .map(|out| parse_ss_listen_ports(&out, &pids))
            .unwrap_or_default(),
        // `-a` ANDs the pid filter with LISTEN. Without it `lsof` ORs the
        // selection types and returns every listening socket on the host.
        HostPlatform::MacOs => {
            let pids = pids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            run(
                "lsof",
                &["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pids],
                None,
            )
            .map(|out| parse_reachable_listen_ports(&out))
            .unwrap_or_default()
        }
        HostPlatform::Windows => Vec::new(),
    };
    if ports.is_empty() {
        return ports;
    }
    tracing::debug!(
        root_pid = root,
        ?ports,
        "a session's process tree is listening"
    );
    ports
}

/// Whether two readings of a session's ports are the same, so a poll emits only
/// on a real change.
#[must_use]
pub fn ports_eq(left: &[u16], right: &[u16]) -> bool {
    left == right
}
