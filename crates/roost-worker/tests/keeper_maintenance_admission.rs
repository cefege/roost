//! Keeper maintenance admission. An empty keeper is retired through the
//! identity-fenced shutdown, a keeper holding live channels is refused unless
//! the operator authorizes the destruction, an unproven identity is refused
//! either way, and an exit that takes real work still counts as a completed
//! shutdown. Ports `apps/worker/tests/keeper-maintenance-admission.test.ts`
//! (its log-line assertion is not ported: this crate has no log capture).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "keeper_update_support/mod.rs"]
mod keeper_update_support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use keeper_update_support::{
    Exit, KEEPER_EPOCH, KEEPER_PID, SOURCE_DIGEST, ScriptedHost, binding, contract, running,
};
use roost_protocol::keeper_update::KEEPER_EMPTY_BINDING_DIGEST;
use roost_worker::keeper_pool::{
    EmptyKeeperShutdownExpectation, KeeperRuntimeProbe, shutdown_keeper_for_maintenance,
};

fn empty_keeper() -> ScriptedHost {
    ScriptedHost::new(running(contract(SOURCE_DIGEST, 'a'), &[]))
}

fn live_keeper() -> ScriptedHost {
    ScriptedHost::new(running(contract(SOURCE_DIGEST, 'a'), &[binding(7, 5252)]))
}

#[tokio::test]
async fn retires_an_empty_keeper_through_the_identity_fenced_shutdown() {
    let host = empty_keeper();
    assert_eq!(
        shutdown_keeper_for_maintenance(false, &host).await,
        Ok("shutdown")
    );
    assert_eq!(
        *host.empty_shutdowns.lock().unwrap(),
        vec![EmptyKeeperShutdownExpectation {
            keeper_pid: KEEPER_PID,
            process_epoch: KEEPER_EPOCH.to_owned(),
            binding_digest: KEEPER_EMPTY_BINDING_DIGEST.to_owned(),
        }]
    );
    assert_eq!(host.forced_shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refuses_a_keeper_holding_live_channels_without_force_live() {
    let host = live_keeper();
    let error = shutdown_keeper_for_maintenance(false, &host)
        .await
        .unwrap_err();
    assert!(error.contains("live channels"), "{error}");
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn destroys_a_keeper_holding_live_channels_only_when_force_live_is_authorized() {
    let host = live_keeper();
    assert_eq!(
        shutdown_keeper_for_maintenance(true, &host).await,
        Ok("shutdown")
    );
    assert_eq!(host.forced_shutdowns.load(Ordering::SeqCst), 1);
    assert!(host.empty_shutdowns.lock().unwrap().is_empty());
}

#[tokio::test]
async fn refuses_force_live_when_the_keeper_identity_is_unproven() {
    let host = ScriptedHost::new(KeeperRuntimeProbe::unauthenticated());
    let error = shutdown_keeper_for_maintenance(true, &host)
        .await
        .unwrap_err();
    assert!(error.contains("keeper identity is unproven"), "{error}");
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn reports_an_absent_keeper_without_attempting_a_shutdown() {
    let host = ScriptedHost::new(KeeperRuntimeProbe::unreachable());
    assert_eq!(
        shutdown_keeper_for_maintenance(false, &host).await,
        Ok("already-absent")
    );
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn accepts_an_exit_that_takes_longer_than_two_seconds() {
    let host = empty_keeper().exiting(Exit::After(Duration::from_millis(3_000)));
    assert_eq!(
        shutdown_keeper_for_maintenance(false, &host).await,
        Ok("shutdown")
    );
    assert!(host.elapsed_now() >= Duration::from_millis(3_000));
}

#[tokio::test]
async fn fails_only_after_the_full_exit_confirmation_budget() {
    let host = empty_keeper().exiting(Exit::Never);
    let error = shutdown_keeper_for_maintenance(false, &host)
        .await
        .unwrap_err();
    assert!(error.contains("did not exit"), "{error}");
    assert!(host.elapsed_now() >= Duration::from_millis(30_000));
}
