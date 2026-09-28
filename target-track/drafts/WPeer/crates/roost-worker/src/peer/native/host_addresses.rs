//! The local addresses the native peer offers as host candidates, read the way
//! the kernel reports them without `unsafe`: Linux's procfs routing and
//! address tables, macOS's `ifconfig`. Called by `peer::native::gather` when no
//! bind address is configured. Stands in for the interface walk libjuice did
//! inside node-datachannel for v2 `apps/worker/src/terminal/peer/terminal-peer-native.ts`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[cfg(target_os = "linux")]
const FIB_TRIE: &str = "/proc/net/fib_trie";
#[cfg(target_os = "linux")]
const IF_INET6: &str = "/proc/net/if_inet6";
#[cfg(target_os = "macos")]
const IFCONFIG: &str = "/sbin/ifconfig";
/// if_inet6 flags an address must not carry: DAD failed, deprecated, tentative.
const UNUSABLE_INET6_FLAGS: u32 = 0x08 | 0x20 | 0x40;

/// Every usable unicast address of this host, in the kernel's order. Loopback
/// is offered only when nothing else exists, so a same-host browser on a
/// network-less machine can still connect.
pub(super) async fn host_addresses() -> Vec<IpAddr> {
    let mut found = platform_addresses().await;
    found.retain(|ip| !ip.is_unspecified() && !ip.is_multicast() && !is_link_local(ip));
    let mut offerable: Vec<IpAddr> = Vec::with_capacity(found.len());
    for ip in found {
        if !ip.is_loopback() && !offerable.contains(&ip) {
            offerable.push(ip);
        }
    }
    if offerable.is_empty() {
        tracing::debug!("no non-loopback address was found; the peer offers loopback");
        offerable.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    offerable
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(target_os = "linux")]
async fn platform_addresses() -> Vec<IpAddr> {
    let mut found = match tokio::fs::read_to_string(FIB_TRIE).await {
        Ok(table) => parse_fib_trie(&table),
        Err(error) => {
            tracing::warn!(%error, "the IPv4 address table could not be read");
            Vec::new()
        }
    };
    if let Ok(table) = tokio::fs::read_to_string(IF_INET6).await {
        found.extend(parse_if_inet6(&table));
    }
    found
}

#[cfg(target_os = "macos")]
async fn platform_addresses() -> Vec<IpAddr> {
    match tokio::process::Command::new(IFCONFIG).output().await {
        Ok(output) if output.status.success() => {
            parse_ifconfig(&String::from_utf8_lossy(&output.stdout))
        }
        _ => {
            tracing::warn!("the interface list could not be read");
            Vec::new()
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn platform_addresses() -> Vec<IpAddr> {
    Vec::new()
}

/// The addresses the kernel's local routing table marks `/32 host LOCAL`,
/// each named on the `|-- <address>` line just above that marker.
pub(super) fn parse_fib_trie(table: &str) -> Vec<IpAddr> {
    let mut last: Option<Ipv4Addr> = None;
    let mut found = Vec::new();
    for line in table.lines() {
        let line = line.trim_start();
        if let Some(address) = line.strip_prefix("|-- ") {
            last = address.trim().parse().ok();
        } else if line.starts_with("/32 host LOCAL") {
            if let Some(address) = last.take() {
                found.push(IpAddr::V4(address));
            }
        }
    }
    found
}

/// Global-scope IPv6 addresses whose duplicate detection has settled:
/// `<32 hex> <index> <prefix> <scope> <flags> <name>` per line.
pub(super) fn parse_if_inet6(table: &str) -> Vec<IpAddr> {
    table
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [address, _, _, scope, flags, _] = fields.as_slice() else {
                return None;
            };
            let flags = u32::from_str_radix(flags, 16).ok()?;
            if *scope != "00" || flags & UNUSABLE_INET6_FLAGS != 0 {
                return None;
            }
            let bits = u128::from_str_radix(address, 16).ok()?;
            Some(IpAddr::V6(Ipv6Addr::from(bits)))
        })
        .collect()
}

/// `inet`/`inet6` lines of interfaces flagged UP; scoped (`%`) and
/// tentative, deprecated or detached addresses are skipped.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn parse_ifconfig(listing: &str) -> Vec<IpAddr> {
    let mut up = false;
    let mut found = Vec::new();
    for line in listing.lines() {
        if !line.starts_with(char::is_whitespace) {
            up = line.contains("<UP") || line.contains(",UP");
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(kind), Some(address)) = (words.next(), words.next()) else {
            continue;
        };
        if !up || !(kind == "inet" || kind == "inet6") || address.contains('%') {
            continue;
        }
        if line.contains("tentative") || line.contains("deprecated") || line.contains("detached") {
            continue;
        }
        if let Ok(ip) = address.parse() {
            found.push(ip);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_table_yields_each_host_address_once_per_marker() {
        let table = "Main:\n  +-- 0.0.0.0/0 3 0 5\n     |-- 0.0.0.0\n        /0 universe UNICAST\n     \
            |-- 10.1.2.3\n        /32 host LOCAL\n     |-- 10.1.2.255\n        /32 link BROADCAST\n     \
            |-- 127.0.0.1\n        /32 host LOCAL\n";
        let found = parse_fib_trie(table);
        assert_eq!(found, vec!["10.1.2.3".parse::<IpAddr>().unwrap(), "127.0.0.1".parse().unwrap()]);
    }

    #[test]
    fn only_settled_global_inet6_addresses_are_offered() {
        let table = "20010db8000000000000000000000001 02 40 00 80     eth0\n\
            fe800000000000000000000000000001 02 40 20 80     eth0\n\
            20010db8000000000000000000000002 02 40 00 c0     eth0\n\
            00000000000000000000000000000001 01 80 10 80       lo\n";
        assert_eq!(parse_if_inet6(table), vec!["2001:db8::1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn ifconfig_addresses_of_down_interfaces_are_skipped() {
        let listing = "en0: flags=8863<UP,BROADCAST,RUNNING> mtu 1500\n\
            \tinet6 fe80::1%en0 prefixlen 64 scopeid 0x6\n\
            \tinet 192.168.1.5 netmask 0xffffff00 broadcast 192.168.1.255\n\
            en1: flags=8822<BROADCAST,SMART,SIMPLEX> mtu 1500\n\
            \tinet 10.0.0.9 netmask 0xff000000\n";
        assert_eq!(parse_ifconfig(listing), vec!["192.168.1.5".parse::<IpAddr>().unwrap()]);
    }
}
