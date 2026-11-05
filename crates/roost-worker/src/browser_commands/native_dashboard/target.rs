//! Local-address proof and DNS pinning for the native stats dashboard.
//!
//! Kept beside the HTTP proxy so the proxy file stays under the workspace
//! file cap. A target is resolved exactly once; the chosen IP is pinned and
//! re-proved locally before every request.

use std::net::{IpAddr, SocketAddr, TcpListener, ToSocketAddrs};

/// One native dashboard address after the worker has proved it is local and
/// pinned the IP it will dial. The reported host is kept only for the native
/// identity header, never resolved again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PinnedTarget {
    pub(super) reported_host: String,
    pub(super) dial: IpAddr,
    pub(super) port: u16,
}

impl PinnedTarget {
    pub(super) fn authority(&self) -> String {
        if self.dial.is_ipv6() {
            format!("[{}]:{}", self.dial, self.port)
        } else {
            format!("{}:{}", self.dial, self.port)
        }
    }
}

/// Resolve a native `--host` to an address this machine actually owns.
///
/// Binding the address on an ephemeral port is the proof: an address this OS
/// does not hold answers `EADDRNOTAVAIL`. A DNS name is resolved exactly once
/// here and the chosen IP is pinned, so a later answer cannot move the dial.
pub(super) fn resolve_local_target(host: &str, port: u16) -> Option<PinnedTarget> {
    let lower = host.to_ascii_lowercase();
    let (reported_host, candidates): (String, Vec<IpAddr>) = if lower == "0.0.0.0" {
        (
            host.to_owned(),
            vec![IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
        )
    } else if lower == "::" || lower == "[::]" {
        (
            host.to_owned(),
            vec![IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)],
        )
    } else if let Ok(ip) = host.parse::<IpAddr>() {
        (host.to_owned(), vec![ip])
    } else {
        let resolved = (host, 0)
            .to_socket_addrs()
            .ok()?
            .map(|address| address.ip())
            .collect::<Vec<_>>();
        (host.to_owned(), resolved)
    };
    candidates.into_iter().find_map(|dial| {
        if dial.is_unspecified() || dial.is_multicast() || !ip_is_locally_assigned(dial) {
            return None;
        }
        Some(PinnedTarget {
            reported_host: reported_host.clone(),
            dial,
            port,
        })
    })
}

/// An address this OS can bind, which is what makes it a local destination.
pub(super) fn ip_is_locally_assigned(ip: IpAddr) -> bool {
    TcpListener::bind(SocketAddr::new(ip, 0)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addresses_this_machine_owns_are_dialable() {
        assert!(ip_is_locally_assigned(IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST
        )));
        assert!(!ip_is_locally_assigned(IpAddr::V4(
            std::net::Ipv4Addr::new(203, 0, 113, 1)
        )));
        let target = resolve_local_target("127.0.0.1", 3847).expect("loopback is local");
        assert_eq!(target.authority(), "127.0.0.1:3847");
        assert_eq!(target.reported_host, "127.0.0.1");
        let wildcard = resolve_local_target("0.0.0.0", 3847).expect("a wildcard bind is local");
        assert_eq!(wildcard.authority(), "127.0.0.1:3847");
        assert_eq!(wildcard.reported_host, "0.0.0.0");
        assert!(resolve_local_target("203.0.113.1", 3847).is_none());
    }
}
