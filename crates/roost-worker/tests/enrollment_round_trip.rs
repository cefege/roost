//! Spending a bootstrap token and registering, end to end, against a real
//! coordinator on a real socket.
//!
//! The two properties under test are the ones a production bring-up fails
//! without. A machine that redeems but does not register is invisible to the
//! fleet; a machine whose token is kept anywhere but the point of use is a
//! credential that outlives its grant. Both are invisible to a unit test that
//! stubs the coordinator, which is why this drives the wire.
//!
//! Depends on `enrollment_support` for the coordinator — nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod enrollment_support;

use std::path::Path;

use enrollment_support::Fixture;
use roost_host::{HostPlatform, MapEnv, supported_host_platform};
use roost_worker::host::install::BOOTSTRAP_TOKEN_ENV;
use roost_worker::runtime::boot::{WorkerBoot, ENV_WORKER_KEY_PATH};
use roost_worker::runtime::bootstrap_redeem::{Redemption, enroll};

/// The token the coordinator issues and the worker is handed.
const TOKEN: &str = "roost-boot-token-for-this-test-only";

/// The key file inside a directory of its own, created by `WorkerBoot::resolve`
/// on the caller's behalf — which is the install step, and is why the boot
/// refuses before anything is bound.
fn scratch_dir(label: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "roost-enrollment-{label}-{}-{ordinal}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("the scratch root is creatable");
    root
}

fn env_for(root: &Path, coordinator: &str, token: Option<&str>) -> MapEnv {
    let mut env = MapEnv::new()
        .with(ENV_WORKER_KEY_PATH, root.join("worker.key").to_str().unwrap())
        .with("ROOST_COORDINATOR_URL", coordinator);
    if let Some(token) = token {
        env.set(BOOTSTRAP_TOKEN_ENV, token);
    }
    env
}

/// A MACHINE THAT REDEEMS IS A MACHINE IN THE FLEET. The redemption binds the
/// token to the key, and the registration has to arrive under a credential
/// signed by that same key — so this asserts the coordinator's own `workers`
/// row, not that two calls were made.
#[tokio::test]
async fn a_redeemed_machine_registers_under_the_key_its_token_named() {
    let fixture = Fixture::start(&[TOKEN]).await;
    let root = scratch_dir("registered");
    let coordinator = format!("http://{}", fixture.address);
    let env = env_for(&root, &coordinator, Some(TOKEN));

    let boot = WorkerBoot::resolve(&env, platform()).expect("a resolvable configuration");
    let client = fixture.client();
    let enrollment = enroll(
        &client,
        &roost_worker::runtime::credential::WorkerKeyCredential::new(boot.worker_key_path.clone()),
        &env,
        platform(),
        &boot.worker_key_path,
    )
    .await
    .expect("the coordinator accepts this machine");

    assert_eq!(
        enrollment.redemption,
        Redemption::Redeemed,
        "the token was spent rather than left unspent for a retry"
    );
    assert!(enrollment.registered, "the registration landed");
    assert_eq!(
        fixture.coordinator.holder_of(TOKEN).as_deref(),
        Some(boot.fingerprint.as_str()),
        "the token is bound to the fingerprint the boot derived from the same key file"
    );
    assert!(
        fixture.coordinator.is_registered(boot.fingerprint.as_str()),
        "the coordinator holds a worker row for this machine"
    );
    assert_eq!(
        fixture.coordinator.label_of(boot.fingerprint.as_str()),
        Some(enrollment.label),
        "the fleet shows it under the label the registration carried"
    );
    // The credential is the whole authority the registration runs on, and the
    // fixture refuses one whose `kid` no redemption bound — so a registration
    // that landed is already proof the key matched. Asserting it arrived at all
    // pins that the header is what made it.
    assert_eq!(
        fixture.coordinator.credentials().len(),
        1,
        "exactly one credential was presented, on the registration"
    );

    std::fs::remove_dir_all(&root).expect("the scratch root is removable");
}

/// THE TOKEN IS A ONE-SHOT GRANT, NOT CONFIGURATION. It is read at the point
/// of use and nowhere else, so the resolved boot cannot be carrying it: a boot
/// that held the token would hand it to every reconnect for the life of the
/// process, which is a longer life than the grant has.
#[test]
fn the_token_never_reaches_the_resolved_boot() {
    let root = scratch_dir("no-token-in-boot");
    let env = env_for(&root, "http://127.0.0.1:1", Some(TOKEN));
    let boot = WorkerBoot::resolve(&env, platform()).expect("a resolvable configuration");

    let rendered = format!("{boot:?}");
    assert!(
        !rendered.contains(TOKEN),
        "the resolved boot carries the one-shot token: {rendered}"
    );
    assert!(
        !boot.worker_key_path.to_string_lossy().contains(TOKEN),
        "the key path is not a place a token belongs either"
    );

    std::fs::remove_dir_all(&root).expect("the scratch root is removable");
}

/// A RE-OFFERED TOKEN IS NOT A FAILURE. v2's message said a redemption "may be
/// already used" and carried on; the coordinator makes that safe by rebinding
/// a spent token to the key that already holds it, so a redeploy that re-offers
/// its token costs one round trip instead of a machine that will not start.
#[tokio::test]
async fn a_token_the_same_key_already_spent_is_redeemed_again() {
    let fixture = Fixture::start(&[TOKEN]).await;
    let root = scratch_dir("respent");
    let coordinator = format!("http://{}", fixture.address);
    let env = env_for(&root, &coordinator, Some(TOKEN));
    let boot = WorkerBoot::resolve(&env, platform()).expect("a resolvable configuration");
    let client = fixture.client();
    let credential =
        roost_worker::runtime::credential::WorkerKeyCredential::new(boot.worker_key_path.clone());

    for attempt in 1..=2 {
        let enrollment = enroll(
            &client,
            &credential,
            &env,
            platform(),
            &boot.worker_key_path,
        )
        .await
        .unwrap_or_else(|error| panic!("attempt {attempt}: {error}"));
        assert_eq!(
            enrollment.redemption,
            Redemption::Redeemed,
            "attempt {attempt}: a token this key already spent is still a redemption"
        );
        assert!(enrollment.registered, "attempt {attempt}: registered");
    }

    std::fs::remove_dir_all(&root).expect("the scratch root is removable");
}

fn platform() -> HostPlatform {
    supported_host_platform().expect("this test only runs where v3 runs")
}
