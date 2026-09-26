//! Pair-request provenance: what an operator is shown about the browser that
//! is asking to pair, and the fact that none of it came from the request.
//!
//! The interesting properties are the negative ones. The geo headers are
//! client-settable and must be ignored on a direct listener; the user-agent
//! table is ordered, and a browser that appears late in it must not be reported
//! as the one that appears early; and every value that reaches a row is bounded
//! and control-stripped, because an operator reads these in a browser and an
//! unbounded one is a log-injection path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::http::{HeaderMap, HeaderValue};
use roost_coord::auth::pairing::provenance::{
    ClientDeviceType, MAX_GEO_UTF8_BYTES, MAX_PROVENANCE_UTF8_BYTES, RequestOrigin,
    capture_pair_request_provenance, describe_user_agent,
};
use roost_coord::coord_core::ListenerTrust;

const CHROME_MAC: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const EDGE_WINDOWS: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.0.0";
const SAFARI_IPHONE: &str =
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
const IPAD_AGENT: &str =
    "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/604.1";

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
            HeaderValue::from_str(value).expect("a header value"),
        );
    }
    map
}

fn direct(client_ip: &str) -> RequestOrigin<'_> {
    RequestOrigin {
        listener: ListenerTrust::DirectLoopback,
        client_ip,
    }
}

fn forwarded(client_ip: &str) -> RequestOrigin<'_> {
    RequestOrigin {
        listener: ListenerTrust::Forwarded,
        client_ip,
    }
}

/// The compatibility table is ORDERED, and the order is the contract: an Edge
/// also matches `Chrome/`, and a Chrome also matches `Safari/`, so a browser
/// listed late would otherwise be reported as the one listed early.
#[test]
fn the_user_agent_table_is_ordered_browser_first() {
    let edge = describe_user_agent(EDGE_WINDOWS);
    assert_eq!(edge.browser, Some("Edge"));
    assert_eq!(edge.os, Some("Windows"));
    assert_eq!(edge.device_type, Some(ClientDeviceType::Desktop));

    let chrome = describe_user_agent(CHROME_MAC);
    assert_eq!(chrome.browser, Some("Chrome"));
    assert_eq!(chrome.os, Some("macOS"));
    assert_eq!(chrome.device_type, Some(ClientDeviceType::Desktop));

    let safari = describe_user_agent(SAFARI_IPHONE);
    assert_eq!(safari.browser, Some("Safari"));
    assert_eq!(safari.os, Some("iOS"));
    assert_eq!(safari.device_type, Some(ClientDeviceType::Mobile));
}

/// A tablet is checked before mobile, because an iPad reports itself as mobile
/// in most agents and calling it a phone tells an operator the wrong thing
/// about the device somebody just approved.
#[test]
fn a_tablet_is_not_reported_as_a_phone() {
    let ipad = describe_user_agent(IPAD_AGENT);
    assert_eq!(ipad.os, Some("iOS"));
    assert_eq!(ipad.device_type, Some(ClientDeviceType::Tablet));
}

/// An agent nothing is known about describes nothing. The one inference in
/// this module is that a NAMED browser with no mobile signal is a desktop; an
/// unknown agent is not a desktop by default, because "unknown" is the fact.
#[test]
fn an_unknown_agent_names_nothing_and_guesses_no_device() {
    let unknown = describe_user_agent("curl/8.4.0");
    assert_eq!(unknown.browser, None);
    assert_eq!(unknown.os, None);
    assert_eq!(unknown.device_type, None);
}

/// Client hints win over the user agent, one field at a time, and the GREASE
/// decoys a browser plants in `sec-ch-ua` are dropped before the table is
/// consulted -- otherwise every Chromium browser reports itself as whatever
/// decoy it was told to carry.
#[test]
fn client_hints_win_and_grease_brands_are_ignored() {
    let captured = capture_pair_request_provenance(
        &headers(&[
            ("user-agent", EDGE_WINDOWS),
            (
                "sec-ch-ua",
                "\"Not;A=Brand\";v=\"8\", \"Chromium\";v=\"126\", \"Google Chrome\";v=\"126\"",
            ),
            ("sec-ch-ua-platform", "\"macOS\""),
            ("sec-ch-ua-mobile", "?0"),
        ]),
        direct("10.0.0.4"),
    );
    assert_eq!(captured.client_browser.as_deref(), Some("Chrome"));
    assert_eq!(captured.client_os.as_deref(), Some("macOS"));
    assert_eq!(captured.client_device_type, Some(ClientDeviceType::Desktop));
    assert_eq!(captured.user_agent.as_deref(), Some(EDGE_WINDOWS));
}

/// A hint the browser did not send, or sent as anything but `?1`/`?0`, is no
/// hint at all: this header is the only signal that separates a phone from a
/// small window, so guessing it would be guessing the thing it exists to say.
#[test]
fn an_unusable_mobile_hint_falls_back_to_the_user_agent() {
    for (hint, expected) in [
        ("?1", Some(ClientDeviceType::Mobile)),
        ("?0", Some(ClientDeviceType::Desktop)),
        ("maybe", Some(ClientDeviceType::Desktop)),
    ] {
        let captured = capture_pair_request_provenance(
            &headers(&[
                ("user-agent", CHROME_MAC),
                ("sec-ch-ua-mobile", hint),
            ]),
            direct("10.0.0.4"),
        );
        assert_eq!(
            captured.client_device_type, expected,
            "hint {hint} must not be read as a device class on its own"
        );
    }
}

/// The edge's geo headers are client-settable, so they are read ONLY under a
/// trusted proxy. On a direct listener a request that claims to be in Ottawa
/// is refused the claim, and the address recorded is the one the transport
/// actually saw.
#[test]
fn geo_headers_are_read_only_under_a_trusted_proxy() {
    let geo = [("cf-ipcountry", "CA"), ("cf-region", "ON"), ("cf-ipcity", "Ottawa")];
    let mut direct_pairs = vec![("user-agent", CHROME_MAC)];
    direct_pairs.extend(geo);
    let spoofed = capture_pair_request_provenance(&headers(&direct_pairs), direct("10.0.0.4"));
    assert_eq!(spoofed.country_code, None);
    assert_eq!(spoofed.region, None);
    assert_eq!(spoofed.city, None);
    assert_eq!(spoofed.source_ip, "10.0.0.4");

    let mut proxied_pairs = vec![("user-agent", CHROME_MAC)];
    proxied_pairs.extend(geo);
    let attested = capture_pair_request_provenance(&headers(&proxied_pairs), forwarded("203.0.113.7"));
    assert_eq!(attested.country_code.as_deref(), Some("CA"));
    assert_eq!(attested.region.as_deref(), Some("ON"));
    assert_eq!(attested.city.as_deref(), Some("Ottawa"));
    assert_eq!(attested.source_ip, "203.0.113.7");
}

/// A country is two letters or nothing. A value that is not a country code is
/// dropped rather than stored, because the column is rendered in a table an
/// operator scans and a 4 KB string in it is a denial of service on the UI.
#[test]
fn a_country_must_be_two_letters() {
    let captured = capture_pair_request_provenance(
        &headers(&[("cf-ipcountry", "usa"), ("cf-ipcountry ", "C"), ("cf-ipcountry", "")]),
        forwarded("203.0.113.7"),
    );
    assert_eq!(captured.country_code, None);

    let lowercased = capture_pair_request_provenance(
        &headers(&[("cf-ipcountry", "de")]),
        forwarded("203.0.113.7"),
    );
    assert_eq!(
        lowercased.country_code.as_deref(),
        Some("DE"),
        "a well-behaved proxy that lowercases its country must not lose it"
    );
}

/// Every persisted value is bounded and control-stripped. A newline or an
/// escape in a value an operator reads in a browser and a log is the
/// difference between "Chrome on macOS" and a log line that lies about what
/// happened.
#[test]
fn persisted_values_are_control_stripped_and_bounded() {
    let long_agent = "A".repeat(MAX_PROVENANCE_UTF8_BYTES * 3);
    let captured = capture_pair_request_provenance(
        &headers(&[("user-agent", &long_agent), ("cf-ipcity", "Ottawa\r\nX-Injected: 1")]),
        forwarded("10.0.0.4"),
    );
    let agent = captured.user_agent.expect("a bounded user agent");
    assert_eq!(agent.len(), MAX_PROVENANCE_UTF8_BYTES);
    assert!(!agent.contains('\n') && !agent.contains('\r'));
    let city = captured.city.expect("a city");
    assert_eq!(city, "OttawaX-Injected: 1");
    assert!(city.len() <= MAX_GEO_UTF8_BYTES);
}

/// The bound is on BYTES, not characters, and it never splits a scalar: a
/// truncated value that ends mid-character is not a value anything can store.
#[test]
fn the_bound_counts_bytes_and_never_splits_a_scalar() {
    let multibyte = "é".repeat(MAX_PROVENANCE_UTF8_BYTES);
    let captured = capture_pair_request_provenance(
        &headers(&[("user-agent", &multibyte)]),
        direct("10.0.0.4"),
    );
    let agent = captured.user_agent.expect("a bounded user agent");
    assert!(agent.len() <= MAX_PROVENANCE_UTF8_BYTES);
    assert!(agent.chars().all(|character| character == 'é'));
}

/// A request whose address the transport could not name records a visible
/// "unknown" rather than an empty column, which would read as an address that
/// resolved to nothing.
#[test]
fn an_unnamed_source_is_recorded_as_unknown() {
    let captured = capture_pair_request_provenance(&HeaderMap::new(), RequestOrigin::from_peer(
        ListenerTrust::DirectLoopback,
        None,
    ));
    assert_eq!(captured.source_ip, "unknown");
    assert_eq!(captured.user_agent, None);
    assert_eq!(captured.client_browser, None);
}
