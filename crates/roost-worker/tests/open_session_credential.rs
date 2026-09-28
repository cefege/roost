//! What the coordinator sees when the worker asks which sessions are still open.
//!
//! THE PROPERTY: the open-session read presents this machine's worker
//! credential, and a coordinator that answers `SessionsList` to a worker
//! principal alone is therefore able to answer it at all.
//!
//! WHY IT NEEDS A SERVER RATHER THAN A FAKE CLIENT. The defect this pins is
//! not a value — the rows came back right when the coordinator answered — it is
//! a HEADER the read did not carry. A fake transport that never looks at headers
//! passes with or without the fix, which is the same reason the enrollment suite
//! serves a real Connect server: the credential exists only on the wire.
//!
//! The fixture refuses an anonymous caller the way the coordinator's route table
//! does (`SessionsList` is `DeviceOrOwnWorkerRecovery`,
//! `crates/roost-coord/src/rpc/method_route_rows.rs:79`), so removing the header
//! turns this test red instead of leaving it green over a refused call.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "open_session_credential_support/mod.rs"]
mod support;

use connectrpc::client::{ClientConfig, HttpClient};
use roost_proto::CoordinatorServiceClient;
use support::{Credential, Fixture};

/// A client over the fixture, exactly as the boot builds its own.
fn client(fixture: &Fixture) -> CoordinatorServiceClient<HttpClient> {
    let uri = format!("http://{}", fixture.address)
        .parse()
        .expect("a loopback URL");
    CoordinatorServiceClient::new(HttpClient::plaintext(), ClientConfig::new(uri))
}

/// The read is answered, and the coordinator saw the credential that answered
/// it. Both halves matter: the rows prove the call completed, the recorded
/// bearer proves it completed AS this machine.
#[tokio::test]
async fn the_open_session_read_presents_this_machines_credential() {
    let fixture = Fixture::start().await;
    let credential = Credential::minting("credential-under-test");

    let rows = roost_worker::runtime::reconcile::read_open_sessions(
        &client(&fixture),
        "fp-under-test",
        &credential,
    )
    .await
    .expect("a coordinator that accepts this machine answers the open-session read");

    assert_eq!(
        rows.len(),
        1,
        "the fixture publishes exactly one open row, so a read that invented or \
         dropped rows cannot pass"
    );
    assert_eq!(
        fixture.seen_bearer(),
        Some("credential-under-test".to_owned()),
        "the read reached the coordinator as an authenticated worker, which is the \
         whole property: without the header this coordinator refuses the call"
    );
}

/// The refusal arm, asserted directly rather than through the read: it is what
/// makes the first test mean something. A fixture that answered everyone would
/// let a headerless read pass.
#[tokio::test]
async fn this_coordinator_refuses_an_anonymous_caller() {
    let fixture = Fixture::start().await;
    let refused = roost_worker::runtime::reconcile::read_open_sessions(
        &client(&fixture),
        "fp-under-test",
        &Credential::unspellable(),
    )
    .await;

    assert!(
        refused.is_err(),
        "a credential this worker cannot spell must not travel as a call that \
         succeeds: the coordinator would see no header and refuse it"
    );
    assert_eq!(
        fixture.seen_bearer(),
        None,
        "nothing reached the coordinator, because nothing was sent"
    );
}
