//! The operator-declared STUN list both peers share.
//!
//! The list is normalized to one spelling per server or refused: a second
//! spelling of one server, a credential, a relay scheme, or a control character
//! all reach a browser that gathers ICE, and none of them may get there from
//! configuration.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::terminal_peer::peer::{
    DEFAULT_TERMINAL_PEER_STUN_URLS, TERMINAL_PEER_STUN_URL_MAX_COUNT, TERMINAL_PEER_STUN_URLS_ENV,
    parse_terminal_peer_stun_urls,
};

fn rejected(raw: &str) -> String {
    let error = parse_terminal_peer_stun_urls(Some(raw))
        .expect_err("an operator list that cannot be normalized is refused");
    assert_eq!(error.field, "terminal_peer.stun_urls");
    assert!(
        error.reason.starts_with(TERMINAL_PEER_STUN_URLS_ENV),
        "the reason names the setting: {}",
        error.reason
    );
    error.reason
}

#[test]
fn an_absent_setting_uses_the_account_free_default_and_an_empty_one_uses_none() {
    assert_eq!(
        parse_terminal_peer_stun_urls(None).expect("the default is valid"),
        DEFAULT_TERMINAL_PEER_STUN_URLS.to_vec()
    );
    assert!(
        parse_terminal_peer_stun_urls(Some(""))
            .expect("an empty list is valid")
            .is_empty()
    );
}

#[test]
fn declared_dns_ipv4_and_ipv6_endpoints_normalize_before_they_are_shared() {
    assert_eq!(
        parse_terminal_peer_stun_urls(Some(
            "STUN:Stun.One.Example:3478,stun:192.0.2.8:5349,stun:[2001:DB8::8]:3478"
        ))
        .expect("a mixed list normalizes"),
        vec![
            "stun:stun.one.example:3478",
            "stun:192.0.2.8:5349",
            "stun:[2001:db8::8]:3478",
        ]
    );
    // A host with no port keeps no port: neither end is told a default the
    // other will not derive.
    assert_eq!(
        parse_terminal_peer_stun_urls(Some("stun:Stun.Example"))
            .expect("a portless host normalizes"),
        vec!["stun:stun.example"]
    );
    assert_eq!(
        parse_terminal_peer_stun_urls(Some("stun:[::1]"))
            .expect("the unspecified address is a valid host"),
        vec!["stun:[::1]"]
    );
}

#[test]
fn credentials_relay_schemes_and_url_extensions_are_refused() {
    for raw in [
        "turn:relay.example:3478",
        "turns:relay.example:5349",
        "stuns:stun.example:5349",
        "stun:operator@stun.example:3478",
        "stun:stun.example/path",
        "stun:stun.example?transport=udp",
        "stun:stun.example#fragment",
        "stun:stun.example\n",
        "stun:stun.example ",
        "stun:stun.example:0",
        "stun:stun.example:65536",
        "stun:stun.example:03478",
        "stun:",
        "stun:[not-an-address]:3478",
        "stun:999.0.2.8:3478",
    ] {
        rejected(raw);
    }
}

#[test]
fn a_list_longer_than_the_bound_is_refused_before_it_is_normalized() {
    let five = "stun:one.example,stun:two.example,stun:three.example,\
stun:four.example,stun:five.example";
    let reason = rejected(five);
    assert!(reason.contains("distinct stun: UDP URLs"), "{reason}");
    let four = "stun:one.example,stun:two.example,stun:three.example,stun:four.example";
    assert_eq!(
        parse_terminal_peer_stun_urls(Some(four))
            .expect("the bound itself is admissible")
            .len(),
        TERMINAL_PEER_STUN_URL_MAX_COUNT
    );
}

#[test]
fn a_duplicate_after_normalization_is_refused_rather_than_gathered_twice() {
    let reason = rejected("STUN:Stun.Example:3478,stun:stun.example:3478");
    assert!(reason.contains("distinct stun: UDP URLs"), "{reason}");
}
