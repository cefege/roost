//! `roost coord`'s argument handling: that a flag reaches the loader, and that
//! the loader's refusals happen BEFORE anything binds a socket.
//!
//! The refusals are the whole reason the CLI owns boot resolution. A
//! coordinator told to trust `X-Forwarded-For` and bound to a non-loopback
//! address will believe a caller-supplied address, and the failure is invisible
//! until someone spoofs a header; so the check has to happen in the process that
//! was asked to start it, not only in the installer that may not have run.
//!
//! The rules asserted here are `roost-host`'s, not this crate's — this test
//! exists to prove the CLI hands the loader its flags unchanged and surfaces the
//! loader's refusal as a failed boot rather than swallowing it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::daemon::CoordArgs;
use roost_cli::daemon::coord_boot::resolve_from;
use roost_host::{HostPlatform, MapEnv};

fn home() -> MapEnv {
    MapEnv::new().with("HOME", "/tmp/roost-cli-boot-test")
}

fn trusting_proxy() -> MapEnv {
    home().with("ROOST_TRUST_PROXY", "1")
}

fn args(bind: Option<&str>) -> CoordArgs {
    CoordArgs {
        bind: bind.map(str::to_string),
        db: None,
    }
}

#[test]
fn a_loopback_bind_is_accepted_and_reaches_the_config() {
    let boot = resolve_from(&args(Some("127.0.0.1:5000")), &home(), HostPlatform::Linux).unwrap();
    assert_eq!(boot.config.bind, "127.0.0.1:5000");
    assert_eq!(boot.platform, HostPlatform::Linux);
}

#[test]
fn a_trusting_proxy_on_a_non_loopback_bind_is_refused() {
    // `ROOST_TRUST_PROXY=1` means the coordinator believes the caller's address
    // came from a front door, which is only true if the front door is the only
    // thing that can reach the listener.
    for bind in ["0.0.0.0:4113", "192.168.1.10:4113", "localhost:4113"] {
        let error = resolve_from(&args(Some(bind)), &trusting_proxy(), HostPlatform::Linux)
            .expect_err(&format!("{bind} was accepted behind a trusted proxy"));
        assert!(
            error.message.contains("must use 127.0.0.1"),
            "{bind}: {error}"
        );
    }
}

#[test]
fn a_port_no_socket_can_hold_is_refused_when_the_bind_is_checked() {
    for bind in [
        "127.0.0.1:99999",
        "127.0.0.1:0",
        "127.0.0.1:abc",
        "127.0.0.1",
    ] {
        assert!(
            resolve_from(&args(Some(bind)), &trusting_proxy(), HostPlatform::Linux).is_err(),
            "{bind} was accepted"
        );
    }
}

#[test]
fn no_flag_means_the_installed_default_not_a_guess() {
    let boot = resolve_from(&args(None), &home(), HostPlatform::Linux).unwrap();
    assert_eq!(
        boot.config.bind,
        roost_host::coord_config::DEFAULT_COORDINATOR_BIND
    );
}

#[test]
fn the_database_path_comes_from_the_loader_and_never_from_this_process() {
    let env = home().with("ROOST_COORDINATOR_DB", "/srv/roost/coord.db");
    let boot = resolve_from(&args(None), &env, HostPlatform::Linux).unwrap();
    assert_eq!(
        boot.config.db_path,
        std::path::PathBuf::from("/srv/roost/coord.db")
    );
}

#[test]
fn a_half_declared_cloudflare_access_pair_is_refused_rather_than_ignored() {
    // One of the two without the other is a coordinator whose Access audience
    // check silently does not apply, which is a security state and not a
    // configuration one.
    let env = home().with(
        "ROOST_CF_ACCESS_TEAM_DOMAIN",
        "example.cloudflareaccess.com",
    );
    let error = resolve_from(&args(None), &env, HostPlatform::Linux)
        .expect_err("half a Cloudflare Access pair was accepted");
    assert!(error.message.contains("must be set together"), "{error}");
}
