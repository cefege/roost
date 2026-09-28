//! The worker's boot CONFIGURATION: what `WorkerBoot::resolve` refuses before
//! anything is started, the installer-layout defaults, the command-line overlay
//! and its re-check, and the per-activation process epoch. The step ORDER those
//! resolved values feed is `worker_boot_order.rs`; this file never starts one.

// A test unwraps the value it is asserting about: a failure there IS the
// assertion failing, which is what a test wants. The workspace denies
// unwrap/expect because a panic on a bad wire value in a running component is a
// fleet-visible outage, and that reasoning does not reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use roost_host::{HostPlatform, MapEnv, supported_host_platform};
use roost_worker::runtime::boot::{
    BootConfigError, ENV_COORDINATOR_URL, ENV_KEEPER_EXECUTABLE, ENV_KEEPER_SOCKET,
    KEEPER_PID_NAME, KEEPER_SOCKET_NAME, WORKER_KEY_NAME, WorkerBoot, WorkerOverrides,
};
use scratch::Scratch;

/// A worker's environment, and the scratch home it resolves into.
///
/// Resolution derives the identity from the key file and creates one when the
/// machine has none, so a fixture whose home cannot be written into tests the
/// wrong refusal. The scratch outlives the environment because the environment
/// names a path inside it.
struct Fixture {
    _scratch: Scratch,
    env: MapEnv,
}

impl Fixture {
    fn new() -> Self {
        let scratch = Scratch::new("boot-order");
        // `MapEnv::with` takes `&str`, and the value is built here rather than
        // borrowed from a temporary: `&scratch.path(..).display().to_string()`
        // would borrow a `String` that dies at the end of the statement.
        let home = scratch.path("home").display().to_string();
        let env = MapEnv::new().with("HOME", &home);
        Self {
            _scratch: scratch,
            env,
        }
    }

    fn with(mut self, key: &str, value: &str) -> Self {
        self.env = self.env.with(key, value);
        self
    }
}

fn platform() -> HostPlatform {
    supported_host_platform().expect("this test only runs where v3 runs")
}

/// A configuration is refused here, before a socket is bound or a keeper is
/// probed. The refusal is the point; what is refused is incidental.
#[test]
fn a_configuration_is_refused_before_anything_is_started() {
    // A worker with no key is given one and boots; the identity refusals that
    // used to live here are now the key's, and they are tested against a real
    // key file in `worker_boot_identity.rs`. What is left is a bad base, which
    // must still be a boot refusal rather than a reconnect loop.
    let bad_url = WorkerBoot::resolve(
        &Fixture::new()
            .with(ENV_COORDINATOR_URL, "coordinator.example:4113")
            .env,
        platform(),
    )
    .unwrap_err();
    assert!(
        matches!(bad_url, BootConfigError::BadCoordinatorUrl { .. }),
        "a base with no scheme is refused at boot rather than becoming a \
         reconnect loop an operator has to debug: {bad_url:?}"
    );

    // And the same environment with nothing wrong resolves, which is what
    // makes the refusal above a refusal rather than an accident of the fixture.
    WorkerBoot::resolve(&Fixture::new().env, platform())
        .expect("an identity derived from a key it was given is not a refusal");
}

/// Paths come from the installer layout when the environment names none, so a
/// bare run and an installed service reach the same files.
#[test]
fn the_keeper_and_key_paths_default_to_the_installer_layout() {
    let boot = WorkerBoot::resolve(&Fixture::new().env, platform()).expect("a resolvable worker");
    let data = boot
        .keeper_socket
        .parent()
        .expect("the keeper socket lives in a directory")
        .to_path_buf();
    assert_eq!(boot.keeper_socket, data.join(KEEPER_SOCKET_NAME));
    assert_eq!(boot.keeper_pid_file, data.join(KEEPER_PID_NAME));
    assert_eq!(boot.worker_key_path, data.join(WORKER_KEY_NAME));
    assert_eq!(
        boot.keeper_executable.file_name().unwrap(),
        "roost-keeper",
        "the keeper is the one beside this binary, so an installed release and a \
         developer's checkout agree on which keeper is being admitted"
    );
}

/// A command line is laid over the environment and checked once afterwards, so
/// an override cannot smuggle in a value resolution just refused.
#[test]
fn an_override_is_applied_and_then_re_checked() {
    let platform = platform();
    let mut boot = WorkerBoot::resolve(
        &Fixture::new()
            .with(ENV_COORDINATOR_URL, "https://coord.example")
            .with(ENV_KEEPER_SOCKET, "/run/roost/mux.sock")
            .with(ENV_KEEPER_EXECUTABLE, "/opt/roost/roost-keeper")
            .env,
        platform,
    )
    .expect("a resolvable worker");
    assert_eq!(boot.coordinator_base, "https://coord.example");

    boot.apply(WorkerOverrides {
        coordinator: Some("http://127.0.0.1:4113".to_string()),
        keeper_socket: Some("/tmp/other.sock".to_string()),
        ..WorkerOverrides::default()
    })
    .expect("valid overrides");
    assert_eq!(boot.coordinator_base, "http://127.0.0.1:4113");
    assert_eq!(
        boot.keeper_socket,
        std::path::PathBuf::from("/tmp/other.sock")
    );
    assert_eq!(
        boot.keeper_executable,
        std::path::PathBuf::from("/opt/roost/roost-keeper")
    );

    // The identity is not overlayable, so the overlay's own check is shown on
    // the one field it can still smuggle: a base resolution accepted is a base
    // resolution checked again after the overlay, not before it.
    let refused = boot
        .apply(WorkerOverrides {
            coordinator: Some("no-scheme-at-all".to_string()),
            ..WorkerOverrides::default()
        })
        .unwrap_err();
    assert!(
        matches!(refused, BootConfigError::BadCoordinatorUrl { .. }),
        "the overlay is checked after it is applied, not before: {refused:?}"
    );
}

/// Two activations of the same binary on one host must not share an epoch, or
/// the coordinator cannot tell a worker from its own restart.
#[test]
fn each_activation_gets_its_own_process_epoch() {
    let fixture = Fixture::new();
    let first = WorkerBoot::resolve(&fixture.env, platform()).expect("a resolvable worker");
    let second = WorkerBoot::resolve(&fixture.env, platform()).expect("a resolvable worker");
    assert_ne!(
        first.process_epoch, second.process_epoch,
        "the coordinator tells one worker process from the next incarnation by \
         this value, so a repeated epoch is a worker it thinks never restarted"
    );
    assert!(
        first
            .process_epoch
            .contains(&std::process::id().to_string())
    );
}

const RESTORE: &str = roost_platform::AGENT_CONVERSATION_RESTORE_ENV;

/// Every supported worker platform, Windows included: its data and log roots
/// are named explicitly because they have no POSIX default.
fn restore_fixture(platform: HostPlatform) -> Fixture {
    let fixture = Fixture::new();
    let data = fixture._scratch.path("data").display().to_string();
    let logs = fixture._scratch.path("logs").display().to_string();
    match platform {
        HostPlatform::Windows => fixture
            .with(roost_host::WORKER_DATA_DIR_ENV, &data)
            .with(roost_host::WORKER_LOG_DIR_ENV, &logs),
        _ => fixture,
    }
}

fn restore_setting(platform: HostPlatform, value: Option<&str>) -> Result<bool, BootConfigError> {
    let fixture = restore_fixture(platform);
    let fixture = match value {
        Some(value) => fixture.with(RESTORE, value),
        None => fixture,
    };
    WorkerBoot::resolve(&fixture.env, platform).map(|boot| boot.agent_conversation_restore)
}

/// v2 `tests/host/config.test.ts` "defaults to disabled on every platform when absent".
#[test]
fn conversation_restore_defaults_to_disabled_on_every_platform() {
    for platform in [HostPlatform::Linux, HostPlatform::MacOs, HostPlatform::Windows] {
        assert_eq!(restore_setting(platform, None), Ok(false), "{platform:?}");
    }
}

/// v2 "accepts only exact 0 and 1 values on POSIX".
#[test]
fn conversation_restore_accepts_exactly_0_and_1_on_posix() {
    for platform in [HostPlatform::Linux, HostPlatform::MacOs] {
        assert_eq!(restore_setting(platform, Some("0")), Ok(false), "{platform:?}");
        assert_eq!(restore_setting(platform, Some("1")), Ok(true), "{platform:?}");
    }
}

/// v2 "rejects every other explicit value".
#[test]
fn conversation_restore_rejects_every_other_explicit_value() {
    for value in ["", "2", "true", "01", " 1 "] {
        let refused = restore_setting(HostPlatform::Linux, Some(value));
        assert_eq!(refused, Err(BootConfigError::BadConversationRestore), "{value:?}");
        assert_eq!(
            refused.unwrap_err().to_string(),
            "ROOST_AGENT_CONVERSATION_RESTORE must be exactly 0 or 1"
        );
    }
}

/// v2 "rejects explicit enablement on Windows".
#[test]
fn conversation_restore_rejects_explicit_enablement_on_windows() {
    let refused = restore_setting(HostPlatform::Windows, Some("1"));
    assert_eq!(refused, Err(BootConfigError::ConversationRestoreOnWindows));
    assert_eq!(
        refused.unwrap_err().to_string(),
        "ROOST_AGENT_CONVERSATION_RESTORE=1 is unsupported on Windows"
    );
    assert_eq!(restore_setting(HostPlatform::Windows, Some("0")), Ok(false));
}
