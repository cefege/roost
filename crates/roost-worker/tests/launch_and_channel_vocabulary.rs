//! The two vocabulary seams outside the record: what may enter a PTY's
//! environment, and which channels a pool announces as live. Both are small,
//! both are load-bearing, and both are the kind of rule that reads as an
//! implementation detail until it is the reason a machine's terminals leaked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use roost_host::HostPlatform;
use roost_keeper::frames::ChannelBinding as KeeperChannelBinding;
use roost_worker::keeper_pool::{PoolChannel, live_bindings};
use roost_worker::session::sinks::ChannelBinding;
use roost_worker::shell_spec::{
    KEEPER_CONTROL_ENV_PREFIX, SESSION_ID_ENV, ShellSpec, is_keeper_control_key,
};

/// Keeper control credentials are worker/keeper-only. A worker that leaks one
/// hands any command the user runs the ability to speak to the keeper as this
/// worker, which is every terminal on the machine.
#[test]
fn a_keeper_control_credential_is_recognised_whatever_its_case() {
    for name in [
        format!("{KEEPER_CONTROL_ENV_PREFIX}CAPABILITY"),
        format!("{}capability", KEEPER_CONTROL_ENV_PREFIX.to_lowercase()),
        "ROOST_KEEPER_ENDPOINT_KIND".to_string(),
        "Roost_Keeper_Capability_Path".to_string(),
    ] {
        assert!(is_keeper_control_key(&name), "{name} must be refused a PTY");
    }
    // The near-misses are the ones a prefix check written by eye gets wrong: a
    // key that merely CONTAINS the marker is not a keeper credential, and
    // refusing it would strip a user's own variable from their shell.
    for name in ["MY_ROOST_KEEPER_TOKEN", "ROOST_KEEPER", "ROOST_KEEPERX"] {
        assert!(!is_keeper_control_key(name), "{name} must reach the PTY");
    }
}

/// The keeper executes the contract verbatim, so the conversion to its wire
/// shape may drop nothing — and the session id has to travel with it.
#[test]
fn the_keeper_command_carries_the_whole_contract_and_its_cwd() {
    let spec = ShellSpec {
        version: 1,
        platform: HostPlatform::MacOs,
        executable: "/bin/zsh".to_string(),
        argv: Vec::new(),
        cwd: "/Users/almalinux/repos/roost".to_string(),
        env: vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            (SESSION_ID_ENV.to_string(), "6f1c0f2e".to_string()),
        ],
    };
    let command = spec.keeper_command();
    assert_eq!(command.program, "/bin/zsh");
    assert!(command.args.is_empty());
    assert_eq!(command.cwd.as_deref(), Some("/Users/almalinux/repos/roost"));
    assert_eq!(command.env, spec.env);
    assert_eq!(spec.env_value(SESSION_ID_ENV), Some("6f1c0f2e"));
    assert_eq!(spec.env_value("TERM"), Some("xterm-256color"));
    assert_eq!(spec.env_value("LANG"), None);
}

/// A killed child still has a pid on the keeper until the keeper reaps it, so
/// the binding a hello announces is not derivable from the wire pair alone.
///
/// Announcing a dead channel is how a survivor's channel list grows a process
/// nothing owns, which is the condition `crate::strays` exists to clean up.
#[test]
fn an_exited_channel_is_not_announced_as_live() {
    let running = PoolChannel::live(
        KeeperChannelBinding {
            channel_id: 3,
            pid: 111,
        },
        Arc::new(Recording),
    );
    let mut finished = PoolChannel::live(
        KeeperChannelBinding {
            channel_id: 4,
            pid: 222,
        },
        Arc::new(Recording),
    );
    finished.mark_exited();

    let announced = live_bindings([&running, &finished]);
    assert_eq!(announced.len(), 1);
    assert_eq!(announced[0].channel_id, 3);
    assert_eq!(announced[0].pid, 111);

    // Marking twice is a no-op rather than a second announcement.
    finished.mark_exited();
    assert!(finished.has_exited());
    assert!(!running.has_exited());
}

/// A binding that records nothing. These tests are about which channels are
/// announced, not about what delivery does — the emit path owns that.
struct Recording;

impl ChannelBinding for Recording {
    fn on_output(&self, _chunk: &[u8]) {}
    fn on_exit(&self, _exit_code: Option<i32>) {}
    fn on_error(&self, _reason: String) {}
}
