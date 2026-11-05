//! Behaviour tests for the native dashboard origin, path and header rules.
//! Kept beside the module so the implementation stays under the file cap.

use super::*;

#[test]
fn an_exact_reported_origin_is_a_dashboard_target_and_a_path_is_not() {
    assert_eq!(
        parse_dashboard_origin("http://127.0.0.1:3847"),
        Some(DashboardTarget {
            host: "127.0.0.1".into(),
            identity_host: "127.0.0.1".into(),
            port: 3847
        })
    );
    assert_eq!(
        parse_dashboard_origin("http://[::1]:3847/").map(|target| target.authority()),
        Some("[::1]:3847".to_owned())
    );
    assert_eq!(
        parse_dashboard_origin("http://0.0.0.0:3847"),
        Some(DashboardTarget {
            host: "127.0.0.1".into(),
            identity_host: "0.0.0.0".into(),
            port: 3847
        })
    );
    for refused in [
        "https://127.0.0.1:3847",
        "http://127.0.0.1:3847/api",
        "http://user@127.0.0.1:3847",
        "http://127.0.0.1:0",
        "http://127.0.0.1",
        "http://127.0.0.1:3847.evil",
        "http://bad host:3847",
    ] {
        assert_eq!(parse_dashboard_origin(refused), None, "{refused}");
    }
    assert_eq!(
        parse_dashboard_origin("http://192.168.1.10:3847"),
        Some(DashboardTarget {
            host: "192.168.1.10".into(),
            identity_host: "192.168.1.10".into(),
            port: 3847
        })
    );
    assert_eq!(
        parse_dashboard_origin("http://dashboard.worker.internal:3847"),
        Some(DashboardTarget {
            host: "dashboard.worker.internal".into(),
            identity_host: "dashboard.worker.internal".into(),
            port: 3847
        })
    );
}

#[test]
fn dashboard_identity_is_the_reported_host_and_never_a_substitute() {
    assert!(is_dashboard_identity(
        Some("3"),
        Some("192.168.1.10"),
        "192.168.1.10"
    ));
    assert!(is_dashboard_identity(
        Some("3"),
        Some("0.0.0.0"),
        "127.0.0.1"
    ));
    assert!(!is_dashboard_identity(
        Some("3"),
        Some("192.168.1.11"),
        "192.168.1.10"
    ));
    assert!(!is_dashboard_identity(
        Some("2"),
        Some("192.168.1.10"),
        "192.168.1.10"
    ));
    assert!(!is_dashboard_identity(
        None,
        Some("192.168.1.10"),
        "192.168.1.10"
    ));
}

#[test]
fn a_capability_path_is_a_token_and_a_safe_upstream_path() {
    let token = "a".repeat(64);
    let path = capability_path(&token);
    assert_eq!(parse_capability_path(&path), Some((token.as_str(), "/")));
    assert_eq!(
        parse_capability_path(&format!("{path}api/stats?range=24h")),
        Some((token.as_str(), "/api/stats?range=24h"))
    );
    assert_eq!(
        parse_capability_path(&format!("{path}..%2fassets/secret")),
        None
    );
    assert_eq!(parse_capability_path("/omp-stats/short/"), None);
}

#[test]
fn credentials_and_redirect_headers_never_cross_the_bridge() {
    let kept = sanitize_request_headers(vec![
        HttpHeader {
            name: "Cookie".into(),
            value: "device=jwt".into(),
        },
        HttpHeader {
            name: "Authorization".into(),
            value: "Bearer x".into(),
        },
        HttpHeader {
            name: "X-Omp-Stats-Action".into(),
            value: "1".into(),
        },
    ]);
    assert_eq!(
        kept,
        vec![HttpHeader {
            name: "x-omp-stats-action".into(),
            value: "1".into()
        }]
    );
    let kept = sanitize_response_headers(vec![
        HttpHeader {
            name: "Set-Cookie".into(),
            value: "a=b".into(),
        },
        HttpHeader {
            name: "Location".into(),
            value: "https://evil".into(),
        },
        HttpHeader {
            name: "Content-Type".into(),
            value: "text/html".into(),
        },
    ]);
    assert_eq!(
        kept,
        vec![HttpHeader {
            name: "content-type".into(),
            value: "text/html".into()
        }]
    );
}

#[test]
fn a_traversal_is_refused_in_every_spelling() {
    for refused in [
        "/../etc/passwd",
        "/assets/../../secret",
        "/assets/%2e%2e/secret",
        "/assets/%2Fetc",
        "/assets\\..\\secret",
        "",
        "api/stats",
    ] {
        assert!(!path_is_safe(refused), "{refused}");
    }
    assert!(path_is_safe("/api/session/trace?file=a&id=b"));
}
