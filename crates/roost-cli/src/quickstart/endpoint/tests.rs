//! Endpoint preservation regressions for installed coordinator profiles.
//! Exercises local reruns and explicit front-door promotions.
//! Uses isolated environment maps rather than installed services.

use super::{EndpointMode, installed_endpoint, port_of, validate_installed_coordinator};
use crate::status::service_definition::InstalledEnvironment;
use roost_host::{HostPlatform, MapEnv};

fn base() -> MapEnv {
    MapEnv::new().with("HOME", "/home/operator")
}

fn installed(pairs: &[(&str, &str)]) -> InstalledEnvironment {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn a_host_port_bind_yields_its_port_and_bare_or_out_of_range_text_does_not() {
    assert_eq!(port_of("127.0.0.1:4113"), Some(4113));
    assert_eq!(port_of("0.0.0.0:4113"), Some(4113));
    assert_eq!(port_of("127.0.0.1"), None);
    assert_eq!(port_of("127.0.0.1:0"), None);
    assert_eq!(port_of("127.0.0.1:99999"), None);
}

#[test]
fn an_install_with_no_front_door_keeps_its_own_loopback_origin() {
    let endpoint = installed_endpoint(
        &installed(&[("ROOST_COORDINATOR_BIND", "127.0.0.1:4200")]),
        None,
        &base(),
        HostPlatform::Linux,
    )
    .expect("an installed bind resolves");
    assert_eq!(endpoint.mode, EndpointMode::Local);
    assert_eq!(endpoint.origin, "http://127.0.0.1:4200");
    assert_eq!(endpoint.loopback_origin(), "http://127.0.0.1:4200");
}

#[test]
fn an_install_that_declares_a_front_door_sends_the_browser_through_it() {
    let endpoint = installed_endpoint(
        &installed(&[
            ("ROOST_COORDINATOR_BIND", "127.0.0.1:4200"),
            ("ROOST_WEB_PUBLIC_URL", "https://roost.example.com"),
        ]),
        None,
        &base(),
        HostPlatform::Linux,
    )
    .expect("an installed front door resolves");
    assert_eq!(endpoint.mode, EndpointMode::FrontDoor);
    assert_eq!(endpoint.origin, "https://roost.example.com");
    assert_eq!(endpoint.loopback_port, 4200);
}

#[test]
fn a_promotion_keeps_the_installed_worker_door_and_adds_the_local_origin() {
    let endpoint = installed_endpoint(
        &installed(&[
            ("ROOST_COORDINATOR_BIND", "127.0.0.1:4200"),
            ("ROOST_COORDINATOR_PUBLIC_URL", "https://api.example.com"),
            ("ROOST_WEB_PUBLIC_URL", "https://old.example.com"),
            (
                "ROOST_RETAINED_FRONTDOOR_ORIGINS",
                "https://earlier.example.com",
            ),
            ("ROOST_CORS_ALLOWED_ORIGINS", "https://old.example.com"),
        ]),
        Some("https://new.example.com"),
        &base(),
        HostPlatform::Linux,
    )
    .expect("a promotion resolves");
    assert_eq!(endpoint.mode, EndpointMode::FrontDoor);
    assert_eq!(
        endpoint.web_public_url.as_deref(),
        Some("https://new.example.com")
    );
    assert_eq!(
        endpoint.coordinator_public_url.as_deref(),
        Some("https://api.example.com")
    );
    assert_eq!(
        endpoint.cors_allowed_origins,
        vec![
            "https://old.example.com".to_string(),
            "http://127.0.0.1:4200".to_string()
        ]
    );
    assert_eq!(
        endpoint.retained_frontdoor_origins,
        vec!["https://earlier.example.com", "https://old.example.com"]
    );
}

#[test]
fn a_front_door_that_is_not_https_is_refused_before_anything_is_touched() {
    let failure = installed_endpoint(
        &installed(&[("ROOST_COORDINATOR_BIND", "127.0.0.1:4200")]),
        Some("http://roost.example.com"),
        &base(),
        HostPlatform::Linux,
    )
    .expect_err("a plaintext front door is refused");
    assert!(
        failure.to_string().contains("--coordinator-url"),
        "{failure}"
    );
}

#[test]
fn a_cors_entry_that_is_not_a_bare_origin_is_refused() {
    let failure = validate_installed_coordinator(
        &installed(&[
            ("ROOST_COORDINATOR_BIND", "127.0.0.1:4200"),
            (
                "ROOST_CORS_ALLOWED_ORIGINS",
                "https://roost.example.com/app",
            ),
        ]),
        &base(),
        HostPlatform::Linux,
    )
    .expect_err("a CORS entry with a path is refused");
    assert!(
        failure
            .to_string()
            .contains(roost_host::ENV_CORS_ALLOWED_ORIGINS),
        "{failure}"
    );
}

#[test]
fn a_definition_that_says_nothing_about_the_bind_still_resolves_to_the_default() {
    let config = validate_installed_coordinator(
        &installed(&[("ROOST_WEB_PUBLIC_URL", "https://roost.example.com")]),
        &base(),
        HostPlatform::Linux,
    )
    .expect("a definition with no bind resolves to the default");
    assert_eq!(config.bind, roost_host::DEFAULT_COORDINATOR_BIND);
}
