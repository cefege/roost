//! The agent harness's provider endpoint overrides are an optional JSON
//! object; malformed values fail boot instead of falling back silently.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::{ENV_AGENT_ENDPOINT_OVERRIDES, HostPlatform, MapEnv, load_coord_config};

fn with_home() -> MapEnv {
    MapEnv::new().with("HOME", "/home/operator")
}

#[test]
fn endpoint_overrides_parse_as_a_provider_url_map() {
    let absent = load_coord_config(&with_home(), HostPlatform::Linux).unwrap();
    assert!(absent.agent_endpoint_overrides.is_empty());

    let declared = load_coord_config(
        &with_home().with(
            ENV_AGENT_ENDPOINT_OVERRIDES,
            r#"{"anthropic":"http://127.0.0.1:9000"}"#,
        ),
        HostPlatform::Linux,
    )
    .unwrap();
    assert_eq!(
        declared
            .agent_endpoint_overrides
            .get("anthropic")
            .map(String::as_str),
        Some("http://127.0.0.1:9000")
    );
}

#[test]
fn malformed_overrides_fail_boot() {
    for raw in ["not json", r#"{"anthropic":"ftp://x"}"#, r#"["http://x"]"#] {
        let error = load_coord_config(
            &with_home().with(ENV_AGENT_ENDPOINT_OVERRIDES, raw),
            HostPlatform::Linux,
        )
        .expect_err("malformed overrides were accepted");
        assert!(
            error.reason.contains("ROOST_AGENT_ENDPOINT_OVERRIDES")
                || error.field.contains("ROOST_AGENT_ENDPOINT_OVERRIDES"),
            "{error:?}"
        );
    }
}
