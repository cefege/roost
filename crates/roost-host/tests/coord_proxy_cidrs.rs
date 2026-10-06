//! The proxy networks a coordinator believes `X-Forwarded-For` from: declaring
//! one lifts the loopback-bind rule, and it means nothing without
//! `ROOST_TRUST_PROXY=1`. Driven at the environment boundary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::{
    ENV_COORDINATOR_BIND, ENV_TRUST_PROXY, ENV_TRUSTED_PROXY_CIDRS, HostPlatform, MapEnv,
    load_coord_config,
};

fn with_home() -> MapEnv {
    MapEnv::new().with("HOME", "/home/operator")
}

fn refused(env: MapEnv) -> String {
    load_coord_config(&env, HostPlatform::Linux)
        .expect_err("a rejected configuration was accepted")
        .reason
}

#[test]
fn a_declared_proxy_network_admits_a_routable_bind() {
    let config = load_coord_config(
        &with_home()
            .with(ENV_COORDINATOR_BIND, "0.0.0.0:4113")
            .with(ENV_TRUST_PROXY, "1")
            .with(
                ENV_TRUSTED_PROXY_CIDRS,
                "10.0.0.0/8, 172.16.0.0/12,,fd00::/8",
            ),
        HostPlatform::Linux,
    )
    .unwrap();
    let rendered: Vec<String> = config
        .trusted_proxy_cidrs
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(rendered, ["10.0.0.0/8", "172.16.0.0/12", "fd00::/8"]);
    assert_eq!(config.bind, "0.0.0.0:4113");
}

#[test]
fn without_a_proxy_network_a_trusted_proxy_still_pins_the_bind_to_loopback() {
    let env = with_home()
        .with(ENV_COORDINATOR_BIND, "0.0.0.0:4113")
        .with(ENV_TRUST_PROXY, "1");
    assert_eq!(
        refused(env),
        "ROOST_COORDINATOR_BIND must use 127.0.0.1:<port>"
    );
}

#[test]
fn a_proxy_network_without_a_trusted_proxy_is_refused() {
    let env = with_home().with(ENV_TRUSTED_PROXY_CIDRS, "10.0.0.0/8");
    assert_eq!(refused(env), "requires ROOST_TRUST_PROXY=1");
}

#[test]
fn a_malformed_entry_is_refused_by_name() {
    let env = with_home()
        .with(ENV_TRUST_PROXY, "1")
        .with(ENV_TRUSTED_PROXY_CIDRS, "10.0.0.0/8,10.0.0.300/8");
    assert_eq!(
        refused(env),
        "`10.0.0.300/8` is not a CIDR such as 10.0.0.0/8"
    );
}
