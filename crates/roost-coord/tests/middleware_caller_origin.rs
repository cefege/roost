// The caller's origin profile: what the transport under a request proves about
// who is calling, and which routes insist on it.
//
// The decision is a pure function, so it is asserted directly here; the one
// thing only the stack can show -- that the export route reads the profile the
// layer resolved rather than deciding locality for itself -- is asserted in
// `middleware_admission_stack.rs`.
use roost_coord::coord_core::ListenerTrust;
use roost_coord::middleware::caller_origin::{
    UNKNOWN_PEER, is_loopback_peer, listener_trust, peer_is_proxy, resolve_caller_origin,
};

/// A directly-observed caller on this host.
#[test]
fn a_listener_that_saw_the_peer_asserts_locality_from_the_address() {
    let origin =
        resolve_caller_origin(ListenerTrust::DirectLoopback, true, Some("127.0.0.1"), None);
    assert!(origin.on_host);
    assert_eq!(origin.client_ip, "127.0.0.1");
    assert_eq!(origin.listener, ListenerTrust::DirectLoopback);
}

/// A peer the listener could not name is not a local operator. The address
/// reported is one every such caller shares, so they spend one budget rather
/// than one each.
#[test]
fn a_caller_with_no_observed_peer_is_never_local() {
    let origin = resolve_caller_origin(ListenerTrust::DirectLoopback, true, None, None);
    assert!(!origin.on_host);
    assert_eq!(origin.client_ip, UNKNOWN_PEER);
}

/// A front door's forwarded header supplies the caller's address, and only the
/// FIRST entry is the caller: the rest of the chain is what the proxy learned,
/// which is the part a client can write.
#[test]
fn a_trusted_proxy_supplies_the_address_from_the_first_forwarded_entry() {
    let origin = resolve_caller_origin(
        ListenerTrust::Forwarded,
        true,
        Some("127.0.0.1"),
        Some("203.0.113.9, 70.41.3.18, 150.172.238.178"),
    );
    assert_eq!(origin.client_ip, "203.0.113.9");
    assert_eq!(origin.listener, ListenerTrust::Forwarded);
}

/// The presence of the forwarded header -- not the address in it -- is what
/// proves a proxy was traversed, so a proxied request from a browser on the
/// coordinator's own host is still not on the host.
#[test]
fn a_traversed_proxy_disqualifies_on_host_even_from_a_loopback_peer() {
    let proxied =
        resolve_caller_origin(ListenerTrust::Forwarded, true, Some("127.0.0.1"), Some(""));
    assert!(
        !proxied.on_host,
        "a present header proves a proxy was traversed"
    );
    assert_eq!(
        proxied.client_ip, "127.0.0.1",
        "a blank entry asserts no address"
    );

    let untouched = resolve_caller_origin(ListenerTrust::Forwarded, true, Some("127.0.0.1"), None);
    assert!(
        untouched.on_host,
        "no header at all means the connection was observed directly"
    );
}

/// The trust profile is chosen at boot from the operator's setting, and the
/// setting picks the profile for every request: a profile read out of a request
/// would be a profile the client wrote.
#[test]
fn the_trust_profile_is_chosen_by_the_operators_setting() {
    assert_eq!(listener_trust(false), ListenerTrust::DirectLoopback);
    assert_eq!(listener_trust(true), ListenerTrust::Forwarded);
    assert!(listener_trust(false).asserts_locality());
    assert!(!listener_trust(true).asserts_locality());
}

/// The three spellings a loopback peer arrives in are this host. One that still
/// carries a port is not: the layer strips the port before asking, because a
/// budget belongs to a client and a client reconnects on a new port.
#[test]
fn loopback_is_recognised_in_every_spelling_and_never_with_a_port() {
    for address in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
        assert!(is_loopback_peer(address), "{address}");
    }
    for address in ["127.0.0.1:51000", "10.0.0.4", "::2", ""] {
        assert!(!is_loopback_peer(address), "{address}");
    }
}

/// Behind a network front door the forwarded header is believed only from a
/// declared proxy network: from inside it the first entry is the caller, from
/// outside it the header is ignored and the socket peer is the caller.
#[test]
fn a_forwarded_header_is_believed_only_from_a_declared_proxy_network() {
    let cidrs: Vec<ipnet::IpNet> = vec!["10.42.0.0/16".parse().unwrap()];

    let through_ingress = resolve_caller_origin(
        ListenerTrust::Forwarded,
        peer_is_proxy(&cidrs, "10.42.0.9"),
        Some("10.42.0.9"),
        Some("203.0.113.5"),
    );
    assert_eq!(through_ingress.client_ip, "203.0.113.5");
    assert!(!through_ingress.on_host);

    let spoofed = resolve_caller_origin(
        ListenerTrust::Forwarded,
        peer_is_proxy(&cidrs, "192.0.2.7"),
        Some("192.0.2.7"),
        Some("127.0.0.1"),
    );
    assert_eq!(
        spoofed.client_ip, "192.0.2.7",
        "an outsider's header is ignored"
    );
    assert!(!spoofed.on_host);
}

/// An IPv4-mapped IPv6 peer is the IPv4 address it carries, and an empty list
/// is the loopback-only front door the bind policy already enforces.
#[test]
fn proxy_membership_reads_mapped_addresses_and_an_empty_list_believes_all() {
    let cidrs: Vec<ipnet::IpNet> = vec!["10.0.0.0/8".parse().unwrap()];
    assert!(peer_is_proxy(&cidrs, "::ffff:10.1.2.3"));
    assert!(!peer_is_proxy(&cidrs, "::ffff:192.0.2.1"));
    assert!(!peer_is_proxy(&cidrs, "not-an-address"));
    assert!(peer_is_proxy(&[], "192.0.2.1"));
}
