//! Agent host configuration is optional as a pair, validated before boot, and
//! never exposed through the configuration's diagnostic representation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::{
    CoordConfig, ENV_AGENT_HOST_SECRET, ENV_AGENT_HOST_URL, HostPlatform, MapEnv, load_coord_config,
};

fn with_home() -> MapEnv {
    MapEnv::new().with("HOME", "/home/operator")
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
fn agent_host_url_and_secret_are_an_optional_validated_pair() {
    assert_eq!(config(with_home()).agent_host_url, None);
    assert_eq!(config(with_home()).agent_host_secret, None);

    for env in [
        with_home().with(ENV_AGENT_HOST_URL, "http://127.0.0.1:4115"),
        with_home().with(ENV_AGENT_HOST_SECRET, &"s".repeat(32)),
    ] {
        assert!(
            refused(env)
                .contains("ROOST_AGENT_HOST_URL and ROOST_AGENT_HOST_SECRET must be set together")
        );
    }
    assert!(
        refused(
            with_home()
                .with(ENV_AGENT_HOST_URL, "http://127.0.0.1:4115")
                .with(ENV_AGENT_HOST_SECRET, "short")
        )
        .contains("must contain at least 32 bytes")
    );

    let secret = "s".repeat(32);
    let enabled = config(
        with_home()
            .with(ENV_AGENT_HOST_URL, "http://127.0.0.1:4115")
            .with(ENV_AGENT_HOST_SECRET, &secret),
    );
    assert_eq!(
        enabled.agent_host_url.as_deref(),
        Some("http://127.0.0.1:4115")
    );
    assert_eq!(enabled.agent_host_secret.as_deref(), Some(secret.as_str()));
    assert!(!format!("{enabled:?}").contains(&secret));
}
