//! The XDG roots, as an OUTSIDE crate sees them, and the one place the XDG
//! specification is deliberately not followed.
//!
//! These live in their own file rather than inside `paths.rs`'s for one reason:
//! the first question they answer is **reachability**, and only a test in
//! another crate can answer that. `cargo check -p roost-host` compiles a
//! crate's internals happily whether or not `lib.rs` re-exports a name, so a
//! function that is `pub` inside the crate and absent from the export list
//! passes every check inside it and fails on the first import from outside. That
//! is not hypothetical: it is exactly what happened to `config_root` and
//! `state_root` when they were first added.
//!
//! The second question is the one that matters for review: **`config_root` does
//! not read `XDG_CONFIG_HOME`, on purpose.** The XDG base-directory
//! specification says it should. v2 hardcodes `$HOME/.config` for the systemd
//! user unit and names a literal `~/.config/logrotate.d`, so honouring the
//! variable would move a unit off the path the oracle, the documentation and
//! `systemctl --user` all expect. The parity rule outranks the specification,
//! and the test that pins the divergence is what stops a well-meaning reader
//! from "fixing" it.

use roost_host::{
    HOME_ENV, HostPlatform, MapEnv, XDG_STATE_HOME_ENV, config_root, coord_service_label,
    coord_service_path, state_root,
};
use std::path::PathBuf;

const LINUX_HOME: &str = "/home/operator";

/// The variable this crate deliberately does NOT read. Spelled as a literal
/// rather than imported, because exporting a constant nothing reads would be the
/// same mistake in a different place: the test's whole claim is that setting it
/// has no effect, and a dead public constant would imply it might.
const XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";

fn env_with_home() -> MapEnv {
    MapEnv::new().with(HOME_ENV, LINUX_HOME)
}

#[test]
fn the_config_root_is_dot_config_under_home() {
    assert_eq!(
        config_root(&env_with_home()).expect("the config root resolves"),
        PathBuf::from(LINUX_HOME).join(".config")
    );
}

#[test]
fn the_config_root_is_not_under_local() {
    // Data is `~/.local/share` and state is `~/.local/state`; config is
    // `~/.config`. A helper carrying all three as `~/.local/<leaf>` would produce
    // a directory nothing reads.
    let resolved = config_root(&env_with_home()).expect("the config root resolves");
    assert!(
        !resolved.starts_with(PathBuf::from(LINUX_HOME).join(".local")),
        "the config root landed under .local, which is where data and state live"
    );
}

#[test]
fn the_config_root_ignores_xdg_config_home_because_v2_does() {
    // THE PARITY PIN. v2 hardcodes `join(homedir(), ".config", ...)` and never
    // reads the variable, so an install that honoured it would place the unit
    // and the logrotate.d fragments somewhere `systemctl --user` and v2's own
    // tooling do not look. If this test ever fails, the change is real and the
    // parity argument has to be re-made deliberately rather than silently.
    let env = env_with_home().with(XDG_CONFIG_HOME, "/etc/roost-config");
    assert_eq!(
        config_root(&env).expect("the config root resolves"),
        PathBuf::from(LINUX_HOME).join(".config"),
        "config_root started honouring XDG_CONFIG_HOME, which diverges from v2"
    );
}

#[test]
fn the_state_root_is_under_local_share_state() {
    assert_eq!(
        state_root(&env_with_home()).expect("the state root resolves"),
        PathBuf::from(LINUX_HOME).join(".local").join("state")
    );
}

#[test]
fn the_state_root_does_honour_xdg_state_home() {
    // The opposite of the config root, and deliberately so: this is the
    // pre-existing behaviour of the data and state roots in this crate, and
    // changing it is a different decision from the one the config root records.
    let env = env_with_home().with(XDG_STATE_HOME_ENV, "/var/lib/roost-state");
    assert_eq!(
        state_root(&env).expect("the state root resolves"),
        PathBuf::from("/var/lib/roost-state")
    );
}

#[test]
fn a_systemd_user_unit_lands_under_the_one_shared_config_root() {
    // ONE rule, not two. `systemd_user_path` used to build
    // `$HOME/.config/systemd/user` itself, so the config root and the unit path
    // were two independent statements of the same thing and could drift.
    //
    // The unit's NAME is asked of `coord_service_label` rather than written out
    // here. This test is about where the unit sits, not what it is called —
    // restating the name would make a rename look like a regression here, and
    // that is exactly how the first draft of this test failed.
    let env = env_with_home();
    let platform = HostPlatform::Linux;
    let expected_name = format!(
        "{}.service",
        coord_service_label(&env, platform).expect("the unit label resolves")
    );
    assert_eq!(
        coord_service_path(&env, platform).expect("the unit path resolves"),
        config_root(&env)
            .expect("the config root resolves")
            .join("systemd")
            .join("user")
            .join(&expected_name)
    );
}
