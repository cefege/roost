//! Coordinator configuration: listener trust, declared origins, terminal peer
//! settings, and the defaults a booting coordinator resolves. The suite drives
//! the loader at the environment boundary, so every assertion is what a booting
//! coordinator would see.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use roost_host::{
    AUTHORIZED_KEYS_FILE_NAME, COORD_DB_FILE_NAME, CoordConfig, DEFAULT_AUDIT_RETENTION_DAYS,
    DEFAULT_COORDINATOR_BIND, DEFAULT_JWT_MAX_AGE_SECS, ENV_CF_ACCESS_AUD,
    ENV_CF_ACCESS_TEAM_DOMAIN, ENV_COORDINATOR_BIND, ENV_CORS_ALLOWED_ORIGINS,
    ENV_PUSH_ALLOWED_ORIGINS, ENV_TERMINAL_PEER_ENABLED, ENV_TERMINAL_PEER_STUN_URLS,
    ENV_TRUST_PROXY, ENV_WEB_PUBLIC_URL, HostPlatform, MapEnv, load_coord_config,
};

const ENV_COORDINATOR_PUBLIC_URL: &str = "ROOST_COORDINATOR_PUBLIC_URL";
const ENV_JWT_MAX_AGE_SECS: &str = "ROOST_COORDINATOR_JWT_MAX_AGE_SECS";
const ENV_AUDIT_RETENTION_DAYS: &str = "ROOST_COORDINATOR_AUDIT_RETENTION_DAYS";
const ENV_MEMORY_BUDGET: &str = "ROOST_COORD_TERMINAL_MEMORY_BUDGET_BYTES";
const LINUX_HOME: &str = "/home/operator";

/// The home every default path resolves against. The loader takes its
/// environment as an injected source, so a test that does not name a home is
/// testing a machine with none — and the refusal would mask every other
/// assertion in the suite behind the same home error.
fn with_home() -> MapEnv {
    MapEnv::new().with("HOME", LINUX_HOME)
}

fn config(env: MapEnv) -> CoordConfig {
    load_coord_config(&env, HostPlatform::Linux).unwrap_or_else(|error| panic!("{error}"))
}

fn refused(env: MapEnv) -> String {
    load_coord_config(&env, HostPlatform::Linux)
        .expect_err("a rejected configuration was accepted")
        .reason
}

#[test]
fn trusted_proxy_headers_are_refused_on_a_network_reachable_bind() {
    for bind in ["0.0.0.0:4113", "[::]:4113", "192.168.1.8:4113", "127.0.0.1"] {
        let env = with_home()
            .with(ENV_COORDINATOR_BIND, bind)
            .with(ENV_TRUST_PROXY, "1");
        assert_eq!(
            refused(env),
            "ROOST_COORDINATOR_BIND must use 127.0.0.1:<port>",
            "bind {bind} was accepted behind a trusted proxy"
        );
    }
}

#[test]
fn a_loopback_bind_is_accepted_behind_a_trusted_proxy_and_its_port_is_bounded() {
    let env = with_home()
        .with(ENV_COORDINATOR_BIND, "127.0.0.1:4113")
        .with(ENV_TRUST_PROXY, "1")
        .with(ENV_WEB_PUBLIC_URL, "https://roost.example.com");
    let config = config(env);
    assert!(config.trust_proxy);
    assert_eq!(config.bind, "127.0.0.1:4113");
    assert_eq!(
        config.web_public_url.as_deref(),
        Some("https://roost.example.com")
    );

    for (bind, expected) in [
        (
            "127.0.0.1:70000",
            "ROOST_COORDINATOR_BIND port must be 1-65535",
        ),
        (
            "127.0.0.1:0",
            "ROOST_COORDINATOR_BIND must use 127.0.0.1:<port>",
        ),
    ] {
        let env = with_home()
            .with(ENV_COORDINATOR_BIND, bind)
            .with(ENV_TRUST_PROXY, "1");
        assert_eq!(refused(env), expected, "bind {bind}");
    }
}

#[test]
fn a_network_reachable_bind_is_left_alone_without_a_trusted_proxy() {
    let env = with_home().with(ENV_COORDINATOR_BIND, "0.0.0.0:4102");
    assert_eq!(config(env).bind, "0.0.0.0:4102");
}

#[test]
fn an_unset_bind_resolves_to_the_one_declared_loopback_default() {
    // A plaintext listener must not reach every interface when nothing is set,
    // and every caller that dials a bare coordinator reads this same value.
    assert_eq!(DEFAULT_COORDINATOR_BIND, "127.0.0.1:4113");
    assert_eq!(config(with_home()).bind, DEFAULT_COORDINATOR_BIND);
}

#[test]
fn public_origins_are_declared_and_never_derived() {
    let config = config(with_home());
    assert_eq!(config.web_public_url, None);
    assert_eq!(config.public_url, None);

    for env_name in [ENV_WEB_PUBLIC_URL, ENV_COORDINATOR_PUBLIC_URL] {
        for value in [
            "http://roost.example.com",
            "https://roost.example.com/path",
            "https://user@roost.example.com",
            "https://roost.example.com?token=secret",
            "https://roost.example.com#fragment",
            "not a URL",
        ] {
            let reason = refused(with_home().with(env_name, value));
            assert!(
                reason.contains(env_name),
                "{env_name}={value} reported {reason:?} without naming the variable"
            );
        }
    }
}

#[test]
fn one_front_door_can_serve_both_browser_and_worker_traffic() {
    let env = with_home()
        .with(ENV_WEB_PUBLIC_URL, "https://roost.example.com/")
        .with(ENV_COORDINATOR_PUBLIC_URL, "https://roost.example.com");
    let config = config(env);
    assert_eq!(
        config.web_public_url.as_deref(),
        Some("https://roost.example.com")
    );
    assert_eq!(
        config.public_url.as_deref(),
        Some("https://roost.example.com")
    );
}

#[test]
fn a_distinct_worker_origin_is_kept_when_the_operator_declares_one() {
    let env = with_home()
        .with(ENV_WEB_PUBLIC_URL, "https://roost.example.com")
        .with(ENV_COORDINATOR_PUBLIC_URL, "https://coord.example.com:4113");
    assert_eq!(
        config(env).public_url.as_deref(),
        Some("https://coord.example.com:4113")
    );
}

#[test]
fn every_cors_entry_must_be_a_bare_http_origin() {
    for (declared, expected) in [
        (
            "file:///tmp/a",
            "ROOST_CORS_ALLOWED_ORIGINS entries must be bare HTTP(S) origins: file:///tmp/a",
        ),
        (
            "https://example.com/path",
            "ROOST_CORS_ALLOWED_ORIGINS entries must be bare HTTP(S) origins: \
             https://example.com/path",
        ),
        (
            "not a URL",
            "ROOST_CORS_ALLOWED_ORIGINS contains an invalid origin: not a URL",
        ),
    ] {
        let env = with_home().with(ENV_CORS_ALLOWED_ORIGINS, declared);
        assert_eq!(refused(env), expected, "{declared}");
    }

    let env = with_home().with(
        ENV_CORS_ALLOWED_ORIGINS,
        "http://localhost:3000,https://example.com",
    );
    assert_eq!(
        config(env).cors_allowed_origins,
        vec!["http://localhost:3000", "https://example.com"]
    );
    assert!(config(with_home()).cors_allowed_origins.is_empty());
}

#[test]
fn push_origins_are_exact_bare_https_and_default_to_disabled() {
    assert!(config(with_home()).push_allowed_origins.is_empty());

    let env = with_home().with(
        ENV_PUSH_ALLOWED_ORIGINS,
        "https://push.example, https://updates.example:8443",
    );
    assert_eq!(
        config(env).push_allowed_origins,
        vec!["https://push.example", "https://updates.example:8443"]
    );

    for origin in [
        "http://push.example",
        "https://push.example/",
        "https://push.example/path",
        "https://push.example?token=secret",
        "https://user@push.example",
        "not a URL",
    ] {
        let env = with_home().with(ENV_PUSH_ALLOWED_ORIGINS, origin);
        let reason = refused(env);
        assert!(
            reason.contains(ENV_PUSH_ALLOWED_ORIGINS),
            "{origin} reported {reason:?} without naming the variable"
        );
    }
}

#[test]
fn the_terminal_peer_carrier_is_enabled_by_default_and_takes_exact_operator_values() {
    assert!(config(with_home()).terminal_peer_enabled);

    let disabled = config(with_home().with(ENV_TERMINAL_PEER_ENABLED, "0"));
    assert!(!disabled.terminal_peer_enabled);
    let enabled = config(with_home().with(ENV_TERMINAL_PEER_ENABLED, "1"));
    assert!(enabled.terminal_peer_enabled);

    for value in ["", "2", "true", "01", " 1 "] {
        let env = with_home().with(ENV_TERMINAL_PEER_ENABLED, value);
        assert_eq!(
            refused(env),
            "ROOST_TERMINAL_PEER_ENABLED must be exactly 0 or 1",
            "{value:?} was accepted"
        );
    }
}

#[test]
fn stun_urls_default_to_cloudflare_and_may_be_disabled_or_normalized() {
    assert_eq!(
        config(with_home()).terminal_peer_stun_urls,
        vec!["stun:stun.cloudflare.com:3478"]
    );

    let disabled = config(with_home().with(ENV_TERMINAL_PEER_STUN_URLS, ""));
    assert!(disabled.terminal_peer_stun_urls.is_empty());

    let declared = "STUN:Stun.One.Example:3478,stun:192.0.2.8:5349,stun:[2001:DB8::8]:3478";
    let env = with_home().with(ENV_TERMINAL_PEER_STUN_URLS, declared);
    assert_eq!(
        config(env).terminal_peer_stun_urls,
        vec![
            "stun:stun.one.example:3478",
            "stun:192.0.2.8:5349",
            "stun:[2001:db8::8]:3478",
        ]
    );
}

#[test]
fn at_most_four_distinct_stun_urls_are_accepted() {
    let declared = "stun:one.example,stun:two.example,stun:three.example,stun:four.example";
    let env = with_home().with(ENV_TERMINAL_PEER_STUN_URLS, declared);
    assert_eq!(
        config(env).terminal_peer_stun_urls,
        vec![
            "stun:one.example",
            "stun:two.example",
            "stun:three.example",
            "stun:four.example",
        ]
    );

    for too_many in [
        "stun:one.example,stun:one.example",
        "stun:one.example,stun:two.example,stun:three.example,stun:four.example,stun:five.example",
    ] {
        let env = with_home().with(ENV_TERMINAL_PEER_STUN_URLS, too_many);
        assert_eq!(
            refused(env),
            "ROOST_TERMINAL_PEER_STUN_URLS must contain 1 to 4 distinct stun: UDP URLs",
            "{too_many} was accepted"
        );
    }
}

#[test]
fn relay_schemes_and_unsafe_stun_components_are_refused() {
    for declared in [
        "turn:turn.example:3478",
        "turns:turn.example:5349",
        "stuns:stun.example:5349",
        "stun:user@stun.example:3478",
        "stun:stun.example/path",
        "stun:stun.example?transport=udp",
        "stun:stun.example#fragment",
        "stun:stun.example:0",
        "stun:stun.example:65536",
        "stun:stun.example:abc",
        "stun:stun.example,",
        "stun:stun.example,\nstun:other.example",
    ] {
        let env = with_home().with(ENV_TERMINAL_PEER_STUN_URLS, declared);
        assert_eq!(
            refused(env),
            "ROOST_TERMINAL_PEER_STUN_URLS contains an invalid STUN URL",
            "{declared:?} was accepted"
        );
    }
}

#[test]
fn a_half_declared_cloudflare_access_pair_is_refused_before_anything_is_parsed() {
    for env in [
        with_home().with(ENV_CF_ACCESS_TEAM_DOMAIN, "team.cloudflareaccess.com"),
        with_home().with(ENV_CF_ACCESS_AUD, &"a".repeat(64)),
    ] {
        assert_eq!(
            refused(env),
            "ROOST_CF_ACCESS_TEAM_DOMAIN and ROOST_CF_ACCESS_AUD must be set together"
        );
    }

    let env = with_home()
        .with(ENV_CF_ACCESS_TEAM_DOMAIN, "team.cloudflareaccess.com")
        .with(ENV_CF_ACCESS_AUD, &"a".repeat(64));
    let config = config(env);
    assert_eq!(
        config.cf_access_team_domain.as_deref(),
        Some("team.cloudflareaccess.com")
    );
    assert_eq!(config.cf_access_aud, Some("a".repeat(64)));
}

#[test]
fn the_database_and_authorized_keys_default_into_the_data_directory() {
    let config = config(with_home().with("HOME", LINUX_HOME));
    let data_dir = Path::new(LINUX_HOME).join(".local/share/RoostCoordinatorV3");
    assert_eq!(config.db_path, data_dir.join(COORD_DB_FILE_NAME));
    assert_eq!(
        config.authorized_keys_path,
        data_dir.join(AUTHORIZED_KEYS_FILE_NAME)
    );
    assert_eq!(
        config.log_dir,
        Path::new(LINUX_HOME).join(".local/state/RoostCoordV3")
    );
}

#[test]
fn the_retention_and_toggle_defaults_are_the_declared_ones() {
    let config = config(with_home().with("HOME", LINUX_HOME));
    assert_eq!(config.jwt_max_age_secs, DEFAULT_JWT_MAX_AGE_SECS);
    assert_eq!(config.audit_retention_days, DEFAULT_AUDIT_RETENTION_DAYS);
    assert!(!config.relaxed_csp);
    assert!(!config.trust_proxy);
    assert_eq!(config.terminal_memory_budget_bytes, None);
    assert_eq!(config.web_dist_path, None);
}

#[test]
fn a_window_that_is_not_an_integer_is_refused_rather_than_defaulted() {
    for declared in ["abc", "1.5"] {
        let env = with_home().with(ENV_JWT_MAX_AGE_SECS, declared);
        assert_eq!(
            refused(env),
            "ROOST_COORDINATOR_JWT_MAX_AGE_SECS must be an integer",
            "{declared} was accepted"
        );
    }
    // A whole number written with a zero fraction is still a whole number, and
    // a non-positive one is a policy refusal rather than a parse failure.
    let env = with_home().with(ENV_JWT_MAX_AGE_SECS, "600.0");
    assert_eq!(config(env).jwt_max_age_secs, 600);

    let env = with_home().with(ENV_AUDIT_RETENTION_DAYS, "0");
    assert_eq!(refused(env), "must be a positive integer");

    let env = with_home().with(ENV_MEMORY_BUDGET, " 2147483648 ");
    assert_eq!(
        config(env).terminal_memory_budget_bytes,
        Some(2_147_483_648),
        "a padded integer must parse like a bare one"
    );
}
