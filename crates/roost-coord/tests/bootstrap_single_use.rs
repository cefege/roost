// The property a bootstrap token exists for: redeemable exactly once, by
// exactly one principal, inside the same statement that records the claim.
//
// Every test here drives the real handler against a real database, and the two
// race tests drive it through TWO connections to the same file. That is the
// point: `CoordDb` is a pool of one, so a race through a single core is a race
// the pool serialised before it started, and a claim that read-then-wrote would
// pass it. A second handle makes the question real.

mod auth_device_support;

use connectrpc::ErrorCode;
use roost_coord::auth::bootstrap_tokens::BootstrapTokenKind;
use roost_coord::auth::rpc_devices::handle_devices_revoke;
use roost_proto as proto;

use auth_device_support::{
    ACCOUNT_DEVICE_COUNT, AUTHORIZED_KEY_COUNT, DEVICE_FP, PEER_FP, Scratch, UNSPENT_GRANT_COUNT,
    WORKER_COUNT, browser, mint_grant, mint_via_handler, redeem_browser,
    redeem_browser_via_handler, redeem_worker, redeem_worker_via_handler, worker,
};

/// The one refusal every failed redemption gets, whatever predicate fired.
fn is_invalid_grant(error: &connectrpc::ConnectError) -> bool {
    error.code == ErrorCode::Unauthenticated
        && error.message.as_deref() == Some("invalid or expired token")
}

/// TWO BROWSERS, ONE TOKEN, TWO CONNECTIONS: exactly one principal exists
/// afterwards, and the loser's key is nowhere in the database.
#[tokio::test]
async fn two_simultaneous_browser_redemptions_leave_exactly_one_principal() {
    let scratch = Scratch::new("race-browser").await;
    let token = mint_grant(
        scratch.database(),
        scratch.tenancy(),
        BootstrapTokenKind::Browser,
        "one-shot",
        None,
    )
    .await;
    let rival = scratch.second_core().await;

    let (winner, loser) = tokio::join!(
        redeem_browser_via_handler(&scratch.core, redeem_browser(&token, "alpha", "alpha")),
        redeem_browser_via_handler(&rival, redeem_browser(&token, "beta", "beta")),
    );

    let outcomes = [winner, loser];
    let refused = outcomes.iter().filter(|outcome| outcome.is_err()).count();
    let accepted = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert_eq!(accepted, 1, "exactly one redemption may claim the grant");
    assert_eq!(
        refused, 1,
        "the other must be refused, not silently dropped"
    );
    for outcome in outcomes.iter().filter(|outcome| outcome.is_err()) {
        let error = outcome.as_ref().expect_err("a refusal");
        assert!(is_invalid_grant(error), "unexpected refusal: {error:?}");
    }

    assert_eq!(
        scratch.scalar(AUTHORIZED_KEY_COUNT).await,
        1,
        "one claimed key, not two"
    );
    assert_eq!(scratch.scalar(ACCOUNT_DEVICE_COUNT).await, 1);
    let enrolled = scratch
        .scalar(&format!(
            "SELECT count(*) FROM account_devices WHERE fingerprint IN \
             ('{}', '{}')",
            auth_device_support::fingerprint_for("alpha"),
            auth_device_support::fingerprint_for("beta"),
        ))
        .await;
    assert_eq!(enrolled, 1, "the winner is one of the two, not a third key");
}

/// The same race for a machine, where the principal is a `workers` row.
#[tokio::test]
async fn two_simultaneous_worker_redemptions_leave_exactly_one_worker() {
    let scratch = Scratch::new("race-worker").await;
    let token = mint_grant(
        scratch.database(),
        scratch.tenancy(),
        BootstrapTokenKind::Worker,
        "one-shot",
        None,
    )
    .await;
    let rival = scratch.second_core().await;

    let (winner, loser) = tokio::join!(
        redeem_worker_via_handler(&scratch.core, redeem_worker(&token, "alpha", "alpha")),
        redeem_worker_via_handler(&rival, redeem_worker(&token, "beta", "beta")),
    );

    assert!(winner.is_ok() ^ loser.is_ok(), "exactly one may register");
    if let Err(error) = &loser {
        assert!(is_invalid_grant(error), "unexpected refusal: {error:?}");
    }
    assert_eq!(scratch.scalar(WORKER_COUNT).await, 1);
    assert_eq!(scratch.scalar(AUTHORIZED_KEY_COUNT).await, 1);
}

/// A LOST RESPONSE IS NOT A FAILED ONE. The same key re-running the same
/// redemption must succeed, and must not create a second principal -- otherwise
/// a browser that times out can never enroll at all.
#[tokio::test]
async fn a_lost_response_retry_with_the_same_key_is_accepted() {
    let scratch = Scratch::new("retry-same").await;
    let token = mint_grant(
        scratch.database(),
        scratch.tenancy(),
        BootstrapTokenKind::Browser,
        "one-shot",
        None,
    )
    .await;

    redeem_browser_via_handler(&scratch.core, redeem_browser(&token, "alpha", "alpha"))
        .await
        .expect("the first redemption");
    let retried =
        redeem_browser_via_handler(&scratch.core, redeem_browser(&token, "alpha", "alpha")).await;
    assert!(retried.is_ok(), "an exact same-key retry must be accepted");

    assert_eq!(scratch.scalar(AUTHORIZED_KEY_COUNT).await, 1);
    assert_eq!(scratch.scalar(ACCOUNT_DEVICE_COUNT).await, 1);
}

/// The retry arm is a retry, not a second chance: a DIFFERENT key presenting a
/// spent token is a competitor, and is refused.
#[tokio::test]
async fn a_competitor_cannot_redeem_a_token_already_spent() {
    let scratch = Scratch::new("retry-competitor").await;
    let token = mint_grant(
        scratch.database(),
        scratch.tenancy(),
        BootstrapTokenKind::Browser,
        "one-shot",
        None,
    )
    .await;
    redeem_browser_via_handler(&scratch.core, redeem_browser(&token, "alpha", "alpha"))
        .await
        .expect("the first redemption");

    let competitor =
        redeem_browser_via_handler(&scratch.core, redeem_browser(&token, "beta", "beta"))
            .await
            .expect_err("a spent grant admits nobody else");
    assert!(is_invalid_grant(&competitor));
    assert_eq!(scratch.scalar(AUTHORIZED_KEY_COUNT).await, 1);
    assert_eq!(
        scratch
            .scalar(&format!(
                "SELECT count(*) FROM account_devices WHERE fingerprint = '{}'",
                auth_device_support::fingerprint_for("beta"),
            ))
            .await,
        0,
        "the competitor's key must not be installed"
    );
}

/// A grant spent once cannot be spent again BY THE SAME KEY EITHER, once the
/// principal it created is gone: a deleted device's retry is not a retry.
#[tokio::test]
async fn a_retry_whose_principal_is_gone_is_refused() {
    let scratch = Scratch::new("retry-inconsistent").await;
    let token = mint_grant(
        scratch.database(),
        scratch.tenancy(),
        BootstrapTokenKind::Worker,
        "one-shot",
        None,
    )
    .await;
    redeem_worker_via_handler(&scratch.core, redeem_worker(&token, "alpha", "alpha"))
        .await
        .expect("the first redemption");
    scratch
        .exec(&format!(
            "DELETE FROM workers WHERE fp = '{}'",
            auth_device_support::fingerprint_for("alpha"),
        ))
        .await;

    let inconsistent =
        redeem_worker_via_handler(&scratch.core, redeem_worker(&token, "alpha", "alpha"))
            .await
            .expect_err("a grant whose principal vanished cannot be re-claimed");
    assert!(is_invalid_grant(&inconsistent));
    assert_eq!(scratch.scalar(WORKER_COUNT).await, 0);
}

/// What is persisted is a digest. A stolen database file must yield no bearer.
#[tokio::test]
async fn a_minted_grant_is_stored_as_a_digest_and_never_as_its_bearer() {
    let scratch = Scratch::new("digest-only").await;
    scratch.enroll_device(DEVICE_FP, "operator").await;
    let token = mint_via_handler(&scratch.core, &scratch.device(), "worker", "new machine").await;

    assert!(token.starts_with("roost_bt_"), "the minted form");
    let digest = roost_coord::auth::bootstrap_tokens::bootstrap_token_digest(&token);
    let stored = scratch
        .scalar(&format!(
            "SELECT count(*) FROM bootstrap_tokens WHERE token_hash = '{digest}'"
        ))
        .await;
    assert_eq!(stored, 1, "the row is keyed by the digest of the bearer");
    let bearer_rows = scratch
        .scalar(&format!(
            "SELECT count(*) FROM bootstrap_tokens WHERE token_hash = '{token}'"
        ))
        .await;
    assert_eq!(bearer_rows, 0, "the plaintext bearer is never a row");
}

/// A grant outlives nothing: revoking the device that minted it takes the
/// unspent grant with it, so a stolen token cannot outlive the trust that
/// issued it.
#[tokio::test]
async fn revoking_a_minter_takes_its_unspent_grants_with_it() {
    let scratch = Scratch::new("minter-revoked").await;
    scratch.enroll_device(DEVICE_FP, "operator").await;
    scratch.enroll_device(PEER_FP, "peer").await;
    let stolen = mint_via_handler(&scratch.core, &scratch.device(), "worker", "for a peer").await;
    assert_eq!(scratch.scalar(UNSPENT_GRANT_COUNT).await, 1);

    handle_devices_revoke(
        &scratch.core,
        &browser(DEVICE_FP, &scratch.account_id),
        proto::DevicesRevokeRequest {
            fingerprint: DEVICE_FP.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a revoked device");

    assert_eq!(
        scratch.scalar(UNSPENT_GRANT_COUNT).await,
        0,
        "an unspent grant must not outlive its minter"
    );
    let claimed =
        redeem_worker_via_handler(&scratch.core, redeem_worker(&stolen, "alpha", "alpha")).await;
    assert!(claimed.is_err(), "the grant is gone, not merely unusable");
}

/// A machine may not call the operator surface: the same revoke a browser may
/// run is refused for a worker principal, so a compromised machine cannot evict
/// the fleet's browsers.
#[tokio::test]
async fn a_worker_may_not_revoke_a_paired_browser() {
    let scratch = Scratch::new("worker-revoke").await;
    scratch.enroll_device(PEER_FP, "peer").await;

    let refused = handle_devices_revoke(
        &scratch.core,
        &worker(auth_device_support::WORKER_FP),
        proto::DevicesRevokeRequest {
            fingerprint: PEER_FP.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("a worker is not an operator");
    assert_eq!(refused.code, ErrorCode::Unauthenticated);
    assert_eq!(
        scratch
            .scalar(&format!(
                "SELECT count(*) FROM account_devices WHERE fingerprint = '{PEER_FP}'"
            ))
            .await,
        1,
        "the peer's key survives a machine's attempt"
    );
}
