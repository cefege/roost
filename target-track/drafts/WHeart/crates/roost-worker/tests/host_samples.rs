//! The host samplers against v2 `apps/worker/src/host/host-sample-linux.ts`
//! and `host-sample-darwin.ts` (v2 ships no tests for them): the Linux reader
//! finds ANY interface row of `/proc/net/dev`, and the macOS parsers read
//! `top`'s idle share and `netstat -ibn`'s byte columns the way v2's regexes
//! and column indices do.

use roost_worker::host::samples_darwin::{parse_netstat_bytes, top_idle_pct};

/// v2 loops every `/proc/net/dev` line: the gateway interface is rarely the
/// first row (that is usually `lo`), and a reader that only looked at the first
/// row reported no bandwidth on every real host.
#[test]
#[cfg(target_os = "linux")]
fn every_interface_row_of_proc_net_dev_yields_counters() {
    use roost_worker::host::samples::sample_linux_net;
    let dev = std::fs::read_to_string("/proc/net/dev").expect("linux exposes /proc/net/dev");
    let interfaces: Vec<String> = dev
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.trim().to_string())
        .collect();
    assert!(!interfaces.is_empty());
    for interface in &interfaces {
        assert!(
            sample_linux_net(interface).is_some(),
            "no counters for interface {interface}"
        );
    }
    assert_eq!(sample_linux_net("roost-no-such-interface"), None);
}

#[test]
fn tops_idle_share_is_read_from_its_cpu_usage_line() {
    let top = "Processes: 612 total, 3 running, 609 sleeping, 3297 threads\n\
               Load Avg: 2.10, 2.31, 2.40\n\
               CPU usage: 5.26% user, 10.52% sys, 84.21% idle\n\
               SharedLibs: 612M resident, 101M data, 72M linkedit.\n";
    assert_eq!(top_idle_pct(top), Some(84.21));
    assert_eq!(top_idle_pct("CPU usage: n/a"), None);
    assert_eq!(top_idle_pct(""), None);
}

#[test]
fn netstat_bytes_come_from_the_first_non_link_row_that_parses() {
    let netstat = "\
Name       Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll
en0        1500  <Link#6>    aa:bb:cc:dd:ee:ff  9000     0   11111111     8000     0   22222222     0
en0        1500  fe80::1%en0 fe80::1              x     -          -        x     -          -     -
en0        1500  192.168.1     192.168.1.20       9000     -   33333333     8000     -   44444444     -
lo0        16384 <Link#1>                          500     0      60000      500     0      60000     0
";
    let counters = parse_netstat_bytes(netstat, "en0").expect("en0 has a parseable row");
    assert_eq!((counters.rx_bytes, counters.tx_bytes), (33_333_333, 44_444_444));
    assert_eq!(parse_netstat_bytes(netstat, "en9"), None);
}
