//! Where a worker's identity comes from, and the one thing that is gone.
//!
//! `ROOST_WORKER_FINGERPRINT` and `--fingerprint` used to be the worker's
//! identity, and the key it signs with was a separate file that nothing tied to
//! them. A worker could therefore be configured to dial as a fingerprint its own
//! credential could never prove, and the coordinator's answer was an unknown
//! `kid` on every dial with nothing in the worker's log to connect the two. The
//! identity is derived from the key now, and that is the whole of the change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt as _;

use roost_host::{HostPlatform, MapEnv, supported_host_platform};
use roost_worker::host::jwt::read_worker_fingerprint;
use roost_worker::link_dial::CoordinatorEndpoint;
use roost_worker::runtime::boot::{BootConfigError, WORKER_KEY_NAME, WorkerBoot};
use scratch::Scratch;

/// A fingerprint that is well formed and belongs to no key on this machine.
const A_FOREIGN_FINGERPRINT: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn platform() -> HostPlatform {
    supported_host_platform().expect("this test only runs where v3 runs")
}

/// An environment whose home is a scratch directory, so resolution's one write
/// lands somewhere the test owns.
fn environment(scratch: &Scratch) -> MapEnv {
    MapEnv::new().with("HOME", scratch.path("home").display().to_string())
}

/// The identity in the dial and the identity in the token are one derivation.
///
/// A configured fingerprint and a signing key are two facts that can disagree,
/// and the disagreement is invisible from inside the worker: the dial resolves,
/// the link opens, and the coordinator answers 401 because the `kid` it looked
/// up verified nothing.
#[test]
fn a_boot_dials_as_the_key_it_signs_with() {
    let scratch = Scratch::new("boot-identity");
    let boot = WorkerBoot::resolve(&environment(&scratch), platform())
        .expect("a machine with no key is given one and boots");
    let key_path = boot.worker_key_path.clone();
    assert_eq!(
        key_path.file_name().expect("a file name").to_str(),
        Some(WORKER_KEY_NAME),
        "the key the identity comes from is the key the install layout names"
    );
    assert_eq!(
        read_worker_fingerprint(&key_path).expect("the key resolution wrote"),
        boot.fingerprint,
        "the fingerprint boot settles on must be the one the key file derives"
    );
    let endpoint =
        CoordinatorEndpoint::new(boot.coordinator_base.clone(), boot.fingerprint.as_str())
            .expect("a resolvable endpoint");
    assert!(
        endpoint.url().ends_with(boot.fingerprint.as_str()),
        "the coordinator routes on the fingerprint in the path, so the path is \
         where a wrong identity becomes an unroutable dial: {}",
        endpoint.url()
    );
}

/// The override is gone, and gone means gone: a value in the environment does
/// not steer the identity, so an operator who still has it exported gets the
/// machine's real identity rather than a second one that cannot authenticate.
#[test]
fn an_environment_fingerprint_is_not_consulted_any_more() {
    let scratch = Scratch::new("boot-no-override");
    let environment = environment(&scratch).with("ROOST_WORKER_FINGERPRINT", A_FOREIGN_FINGERPRINT);
    let boot = WorkerBoot::resolve(&environment, platform()).expect("a resolvable worker");
    assert_ne!(
        boot.fingerprint.as_str(),
        A_FOREIGN_FINGERPRINT,
        "a configured fingerprint that was honoured would let a worker dial as \
         a machine it holds no key for"
    );
    assert_eq!(
        read_worker_fingerprint(&boot.worker_key_path).expect("the key resolution wrote"),
        boot.fingerprint,
        "the identity came from the key, and from nothing else"
    );
}

/// Two keys, two identities, and the coordinator can tell them apart. This is
/// what derivation buys over configuration: the value in the dial and the row in
/// `authorized_keys` are the same bytes.
#[test]
fn two_workers_with_different_keys_are_two_machines() {
    let first_scratch = Scratch::new("boot-machine-a");
    let second_scratch = Scratch::new("boot-machine-b");
    let first =
        WorkerBoot::resolve(&environment(&first_scratch), platform()).expect("the first machine");
    let second =
        WorkerBoot::resolve(&environment(&second_scratch), platform()).expect("the second machine");
    assert_ne!(
        first.fingerprint, second.fingerprint,
        "a fingerprint that does not depend on the key is one identity shared by \
         every worker on the fleet, and the coordinator cannot tell their \
         sessions apart"
    );
    // And one machine is one machine: resolving twice must not invent a second
    // identity, which is what a regenerating first boot would do.
    let again = WorkerBoot::resolve(&environment(&first_scratch), platform())
        .expect("the first machine, resolved again");
    assert_eq!(
        again.fingerprint, first.fingerprint,
        "a second resolution must not replace the key, or the machine the \
         coordinator knows is gone and a new one has to be enrolled"
    );
}

/// A key another user can read is refused before anything is bound, and the
/// refusal names the file. The alternative — signing with it — is a credential
/// derived from a secret the machine does not own.
#[test]
fn an_unusable_key_is_refused_before_a_socket_is_bound() {
    let scratch = Scratch::new("boot-bad-key");
    let first =
        WorkerBoot::resolve(&environment(&scratch), platform()).expect("the key resolution wrote");
    std::fs::set_permissions(
        &first.worker_key_path,
        std::fs::Permissions::from_mode(0o640),
    )
    .expect("the mode can be widened");

    let refused = WorkerBoot::resolve(&environment(&scratch), platform())
        .expect_err("a shared key is not an identity");
    let BootConfigError::WorkerKey { path, reason } = &refused else {
        panic!("a worker key fault has its own reason, not a generic refusal: {refused:?}");
    };
    assert_eq!(
        path, &first.worker_key_path,
        "the refusal names the file, because the operator has to be able to \
         find it"
    );
    assert!(
        reason.contains("0640"),
        "the refusal carries the mode it refused: {reason}"
    );
}
