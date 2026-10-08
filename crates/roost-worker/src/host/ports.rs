//! The TCP ports a session's process tree is LISTENing on, so the sidebar can
//! chip a dev server and open it. Only this worker can see the host's sockets.
//! Ports v2 `apps/worker/src/host/listening-ports.ts`; `host::sampling` calls
//! it on v2's 90 s ports poll. `ps`, `ss`, `lsof` and Windows' `netstat` run
//! with the tool `PATH` (v2 `TOOL_PATH`), because a service manager's `PATH`
//! has none of them.
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

use super::tool_path::{process_tool_path, run_on_path};

/// Every descendant pid of `root`, itself included, from one `ps` snapshot run
/// on `tool_path`.
#[cfg(unix)]
#[must_use]
pub fn descendant_pids(root: u32, tool_path: &str) -> Vec<u32> {
    let Some(snapshot) = run_on_path(tool_path, "ps", &["-Ao", "pid,ppid"], None) else {
        return vec![root];
    };
    let mut edges = Vec::new();
    for line in snapshot.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(parent)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let (Ok(pid), Ok(parent)) = (pid.parse::<u32>(), parent.parse::<u32>()) {
            edges.push((pid, parent));
        }
    }
    descendants_of(root, edges)
}

/// Every descendant pid of `root`, itself included, from one process-table
/// snapshot.
#[cfg(windows)]
#[must_use]
pub fn descendant_pids(root: u32, _tool_path: &str) -> Vec<u32> {
    let edges = crate::agents::process_snapshot_windows::windows_process_records()
        .into_iter()
        .map(|record| (record.pid, record.ppid));
    descendants_of(root, edges)
}

/// `root` and everything reachable from it along `(pid, parent)` edges.
fn descendants_of(root: u32, edges: impl IntoIterator<Item = (u32, u32)>) -> Vec<u32> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (pid, parent) in edges {
        children.entry(parent).or_default().push(pid);
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

/// The reachable LISTEN ports in `netstat -ano -p TCP` (or `TCPv6`) rows held
/// by `pids`.
///
/// The state column is localized, so a listening row is told by its foreign
/// address instead: `0.0.0.0:0` or `[::]:0`, which only an unconnected socket
/// has. Ascending and distinct.
#[must_use]
pub fn parse_netstat_listen_ports(output: &str, pids: &BTreeSet<u32>) -> Vec<u16> {
    let mut ports = BTreeSet::new();
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let [protocol, local, foreign, _state, pid] = tokens[..] else {
            continue;
        };
        if protocol != "TCP" || !matches!(foreign, "0.0.0.0:0" | "[::]:0") {
            continue;
        }
        if !pid.parse::<u32>().is_ok_and(|pid| pids.contains(&pid)) {
            continue;
        }
        let Some(colon) = local.rfind(':') else {
            continue;
        };
        if is_loopback(&local[..colon]) {
            continue;
        }
        if let Ok(port) = local[colon + 1..].parse::<u16>() {
            ports.insert(port);
        }
    }
    ports.into_iter().collect()
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
        // v2 `cut <= 0`: a bind with no host before its colon is not a row.
        let Some(cut) = local.rfind(':').filter(|cut| *cut > 0) else {
            continue;
        };
        let (host, port) = (&local[..cut], &local[cut + 1..]);
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        if port == 0 || is_loopback(host) {
            continue;
        }
        if !owning_pids(line).any(|pid| pids.contains(&pid)) {
            continue;
        }
        ports.insert(port);
    }
    ports.into_iter().collect()
}

/// The port of a `…:PORT (LISTEN)` bind, or `None` when the line is not one,
/// or when the bind is loopback-only.
///
/// `(LISTEN)` is its own whitespace token in `lsof` output, so the bind is the
/// token BEFORE it (v2 `listening-ports.ts:101`, `\s(\S+):(\d+)\s*\(LISTEN\)`).
fn reachable_bind(line: &str) -> Option<u16> {
    let before = line.trim_end().strip_suffix("(LISTEN)")?;
    let name = before.split_whitespace().last()?;
    let cut = name.rfind(':')?;
    let (host, port) = (&name[..cut], &name[cut + 1..]);
    let port = port.parse::<u16>().ok()?;
    if port == 0 || host.is_empty() || is_loopback(host) {
        return None;
    }
    Some(port)
}

/// Every `pid=N` an `ss -p` row names, as v2's `/\bpid=(\d+)/g` reads them:
/// the owners sit inside one `users:((…))` field, so a per-field prefix match
/// never sees them.
fn owning_pids(line: &str) -> impl Iterator<Item = u32> + '_ {
    line.match_indices("pid=").filter_map(|(at, marker)| {
        let preceded_by_word = line[..at]
            .chars()
            .next_back()
            .is_some_and(|previous| previous.is_alphanumeric() || previous == '_');
        if preceded_by_word {
            return None;
        }
        let digits = &line[at + marker.len()..];
        let end = digits
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(digits.len());
        digits[..end].parse::<u32>().ok()
    })
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
    let tool_path = process_tool_path(platform);
    let pids: BTreeSet<u32> = descendant_pids(root, &tool_path).into_iter().collect();
    let ports = match platform {
        // `ss` has no pid selector; every listening socket is listed with its
        // owner and the tree filter happens in the parser.
        HostPlatform::Linux => run_on_path(&tool_path, "ss", &["-ltnpH"], None)
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
            run_on_path(
                &tool_path,
                "lsof",
                &["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pids],
                None,
            )
            .map(|out| parse_reachable_listen_ports(&out))
            .unwrap_or_default()
        }
        HostPlatform::Windows => ["TCP", "TCPv6"]
            .iter()
            .filter_map(|protocol| {
                run_on_path(&tool_path, "netstat", &["-ano", "-p", protocol], None)
            })
            .flat_map(|out| parse_netstat_listen_ports(&out, &pids))
            .collect::<BTreeSet<u16>>()
            .into_iter()
            .collect(),
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::parse_netstat_listen_ports;

    #[test]
    fn a_netstat_listener_counts_only_when_reachable_and_in_the_tree() {
        let output = "\
  TCP    0.0.0.0:3000    0.0.0.0:0    LISTENING    42
  TCP    127.0.0.1:5000  0.0.0.0:0    LISTENING    42
  TCP    [::]:8080       [::]:0       LISTENING    7
  TCP    10.0.0.5:3000   10.0.0.9:51000  ESTABLISHED  42
";
        assert_eq!(
            parse_netstat_listen_ports(output, &BTreeSet::from([42])),
            [3000]
        );
    }
}
