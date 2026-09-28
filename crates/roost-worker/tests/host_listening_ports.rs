//! Ports v2 `apps/worker/tests/host/listening-ports.test.ts` and the `portsEq`
//! cases of `tests/host/pr-status.test.ts`: only non-loopback binds survive the
//! `lsof` and `ss` parses (the folder chip opens them on the worker's tailnet
//! address), ascending and distinct, and `ss` rows are filtered to the
//! session's process tree. Rows are v2's verbatim captures.

use std::collections::BTreeSet;

use roost_worker::host::ports::{parse_reachable_listen_ports, parse_ss_listen_ports, ports_eq};

const HEADER: &str = "COMMAND   PID USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME";

fn lsof(rows: &[&str]) -> String {
    std::iter::once(HEADER)
        .chain(rows.iter().copied())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn all_interfaces_ipv4_is_kept_and_loopback_v4_v6_dropped() {
    let out = lsof(&[
        "Python  86069 mike    3u  IPv4 0xc1436adcd6afc2ba      0t0  TCP *:5599 (LISTEN)",
        "Python  86069 mike    4u  IPv4 0x8909521add541cec      0t0  TCP 127.0.0.1:5598 (LISTEN)",
        "Python  86069 mike    5u  IPv6 0x5f3a8d20e6cef821      0t0  TCP [::1]:5597 (LISTEN)",
    ]);
    assert_eq!(parse_reachable_listen_ports(&out), vec![5599]);
}

#[test]
fn any_address_lan_and_tailnet_binds_are_kept_ascending() {
    let out = lsof(&[
        "node   1  m  20u  IPv4 0x0  0t0  TCP 0.0.0.0:3000 (LISTEN)",
        "node   1  m  21u  IPv6 0x0  0t0  TCP [::]:8080 (LISTEN)",
        "node   1  m  22u  IPv4 0x0  0t0  TCP 192.168.1.5:4000 (LISTEN)",
        "node   1  m  23u  IPv4 0x0  0t0  TCP 100.64.1.2:5173 (LISTEN)",
    ]);
    assert_eq!(
        parse_reachable_listen_ports(&out),
        vec![3000, 4000, 5173, 8080]
    );
}

#[test]
fn a_loopback_only_server_yields_no_chip() {
    let out = lsof(&[
        "node  9  m  20u  IPv4 0x0  0t0  TCP 127.0.0.1:5173 (LISTEN)",
        "node  9  m  21u  IPv6 0x0  0t0  TCP [::1]:5173 (LISTEN)",
        "node  9  m  22u  IPv4 0x0  0t0  TCP 127.0.0.1:9229 (LISTEN)",
    ]);
    assert!(parse_reachable_listen_ports(&out).is_empty());
}

#[test]
fn a_port_bound_both_any_and_loopback_is_kept_once() {
    let out = lsof(&[
        "srv  7  m  3u  IPv4 0x0  0t0  TCP *:5173 (LISTEN)",
        "srv  7  m  4u  IPv4 0x0  0t0  TCP 127.0.0.1:5173 (LISTEN)",
    ]);
    assert_eq!(parse_reachable_listen_ports(&out), vec![5173]);
}

#[test]
fn empty_header_only_and_non_listen_noise_are_empty() {
    assert!(parse_reachable_listen_ports("").is_empty());
    assert!(parse_reachable_listen_ports(HEADER).is_empty());
    assert!(
        parse_reachable_listen_ports(
            "node 1 m 5u IPv4 0x0 0t0 TCP 100.64.1.2:5173->100.64.1.9:52233 (ESTABLISHED)",
        )
        .is_empty()
    );
}

#[test]
fn ss_keeps_a_non_loopback_row_owned_by_the_tree() {
    let out = r#"LISTEN 0      511          0.0.0.0:8099       0.0.0.0:*    users:(("python3",pid=4242,fd=3))"#;
    assert_eq!(
        parse_ss_listen_ports(out, &BTreeSet::from([4242])),
        vec![8099]
    );
}

#[test]
fn ss_drops_a_loopback_bind_even_when_owned() {
    let out = r#"LISTEN 0      128        127.0.0.1:5432      0.0.0.0:*    users:(("postgres",pid=4242,fd=6))"#;
    assert!(parse_ss_listen_ports(out, &BTreeSet::from([4242])).is_empty());
}

#[test]
fn ss_drops_a_reachable_row_owned_outside_the_tree() {
    let out = r#"LISTEN 0      511          0.0.0.0:8099       0.0.0.0:*    users:(("nginx",pid=999,fd=3))"#;
    assert!(parse_ss_listen_ports(out, &BTreeSet::from([4242])).is_empty());
}

#[test]
fn ss_reads_any_and_loopback_v6_multi_pid_rows_and_empty_input() {
    let out = [
        r#"LISTEN 0      511             [::]:8080          [::]:*    users:(("bun",pid=10,fd=20),("bun",pid=11,fd=20))"#,
        r#"LISTEN 0      128            [::1]:9229          [::]:*    users:(("node",pid=10,fd=21))"#,
    ]
    .join("\n");
    assert_eq!(
        parse_ss_listen_ports(&out, &BTreeSet::from([11])),
        vec![8080]
    );
    assert!(parse_ss_listen_ports("", &BTreeSet::from([11])).is_empty());
}

#[test]
fn ports_eq_is_order_sensitive_and_treats_unsampled_as_none() {
    assert!(ports_eq(&[5174, 8765], &[5174, 8765]));
    assert!(ports_eq(&[], &[]));
    let unsampled: Option<Vec<u16>> = None;
    assert!(ports_eq(unsampled.as_deref().unwrap_or_default(), &[]));
    assert!(!ports_eq(&[5174], &[5174, 8765]));
    assert!(
        !ports_eq(&[5174, 8765], &[8765, 5174]),
        "the caller pre-sorts"
    );
}
