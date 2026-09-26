//! The boot sequence's ORDER, and the configuration refusals that happen before
//! anything is started.
//!
//! Why the slices' own tests cannot cover this: `boot_keeper`, `link_barrier`,
//! `outbox` and `backoff` each test their own decision in isolation, and each of
//! them passes no matter what order the worker calls them in. An order bug —
//! probing a keeper before the identity is settled, announcing readiness before
//! reconciling — is invisible to all of them, and it is the class that ends a
//! user's terminals rather than the class that returns a wrong number.

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
use roost_worker::runtime::boot_order::{BOOT_ORDER, BootSequence, Readiness, ReadyStep, StepId};
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
        let env = MapEnv::new().with("HOME", scratch.path("home").display().to_string());
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

/// The order is written down once, and every step in it says what moving it
/// would break. A step with an empty reason is a step nobody can defend later.
#[test]
fn the_boot_order_is_declared_and_every_step_says_why_it_is_there() {
    let names: Vec<&str> = BOOT_ORDER.iter().map(|step| step.name).collect();
    assert_eq!(
        names,
        vec![
            "identity",
            "keeper-admission",
            "coordinator-link",
            "session-reconcile",
            "ready"
        ],
        "the order is the architecture: identity before any mutation, the keeper's \
         survivor before a session is touched, readiness last"
    );
    for step in BOOT_ORDER {
        assert!(
            step.because.len() > 40,
            "{} carries no reason, so the next person cannot tell what breaks if it moves",
            step.name
        );
    }
}

/// The sequence records what actually ran, in the order it ran, and hands back
/// the declared reason for the step it just recorded.
#[test]
fn the_sequence_records_steps_in_the_order_they_complete() {
    let mut sequence = BootSequence::new();
    assert!(sequence.completed().is_empty());
    for step in [
        StepId::Identity,
        StepId::KeeperAdmission,
        StepId::CoordinatorLink,
    ] {
        let because = sequence.complete(step);
        assert_eq!(because, BOOT_ORDER[step as usize].because);
    }
    assert_eq!(
        sequence.completed(),
        &[
            StepId::Identity,
            StepId::KeeperAdmission,
            StepId::CoordinatorLink
        ]
    );
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

/// v2 put reconciliation, the snapshot provider and readiness in one function
/// for a reason: a snapshot published before reconciliation describes a set the
/// coordinator has not confirmed, and acting on it closes live sessions.
#[test]
fn readiness_cannot_be_announced_before_the_reconcile_it_claims_to_describe() {
    let mut readiness = Readiness::default();
    assert!(!readiness.is_ready());

    let refused = readiness.advance(ReadyStep::MarkedReady).unwrap_err();
    assert_eq!(refused.step, ReadyStep::MarkedReady);
    assert_eq!(refused.at, Readiness::Starting);
    assert!(!readiness.is_ready());

    let refused = readiness
        .advance(ReadyStep::SnapshotProviderActivated)
        .unwrap_err();
    assert_eq!(refused.at, Readiness::Starting);
    assert!(
        !readiness.is_ready(),
        "a snapshot provider activated before reconciliation publishes a set the \
         coordinator never confirmed, and the coordinator closes what is missing"
    );
}

#[test]
fn readiness_advances_only_through_the_three_steps_in_order() {
    let mut readiness = Readiness::default();
    assert_eq!(
        readiness.advance(ReadyStep::Reconciled).unwrap(),
        Readiness::Reconciled
    );
    assert_eq!(
        readiness
            .advance(ReadyStep::SnapshotProviderActivated)
            .unwrap(),
        Readiness::SnapshotActive
    );
    assert_eq!(
        readiness.advance(ReadyStep::MarkedReady).unwrap(),
        Readiness::Ready
    );
    assert!(readiness.is_ready());
    // And it is a one-way door: a second announce is a second claim.
    assert!(readiness.advance(ReadyStep::MarkedReady).is_err());
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
