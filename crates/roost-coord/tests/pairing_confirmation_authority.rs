//! Every branch of the confirmation ceremony that must authorize NOTHING.
//!
//! Owned by the pairing slice. Split from `pairing_confirmation` because the
//! success path and the refusal path fail differently: one is a row that did
//! not appear, the other is a key that appeared when it must not have. Keeping
//! them apart means a reviewer reading the refusals is not also reading the
//! happy path, and a failure names which half broke.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod pairing_support;

use pairing_support::{ACCOUNT, CODE, CeremonyFixture, HANDLE, NOW, REQUESTER_KEY, TOKEN, TTL};
use roost_coord::auth::pairing::PairingRefusal;
use roost_coord::auth::pairing::account::{PairRequestCreate, create_pair_request};
use roost_coord::auth::pairing::rows::{LiveSelector, read_live_pair_request};
use roost_coord::auth::pairing::authority::associate_paired_browser;
use roost_coord::auth::pairing::provenance::PairRequestProvenance;
use roost_coord::auth::pairing::rows::terminalize;
use roost_coord::auth::pairing::secrets::pairing_secret_digest;
use roost_coord::auth::pairing::status::{LiveRequest, TerminalRequest};

/// A confirmation past the deadline expires the request rather than
/// authorizing it. The ten minutes is the whole value of the ceremony's
/// "somebody is watching" property.
#[tokio::test]
async fn a_confirmation_past_the_deadline_expires_the_request() {
    let fixture = CeremonyFixture::new("expired").await;
    fixture.approved_request(NOW).await;
    let before_keys = fixture.authorized_keys().await;

    let error = fixture.confirm(CODE, NOW + TTL).await.unwrap_err();
    assert_eq!(error, PairingRefusal::Expired.to_string());
    assert_eq!(fixture.status().await.as_deref(), Some("expired"));
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

/// A request nobody approved cannot be confirmed, whatever code is presented.
/// This is the state machine's whole point, exercised through the real query.
#[tokio::test]
async fn a_pending_request_cannot_be_confirmed() {
    let fixture = CeremonyFixture::new("unapproved").await;
    let provenance = PairRequestProvenance {
        source_ip: "10.0.0.4".to_string(),
        ..PairRequestProvenance::default()
    };
    create_pair_request(
        &fixture.database,
        &PairRequestCreate {
            ephemeral_id: HANDLE,
            requester_token_hash: &pairing_secret_digest(TOKEN),
            public_key: REQUESTER_KEY,
            label: "Alice's laptop",
            now_ms: NOW,
            expires_at_ms: NOW + TTL,
            provenance: &provenance,
            edge_identity_provider: None,
            edge_identity: None,
        },
    )
    .await
    .expect("a created request");
    let before_keys = fixture.authorized_keys().await;

    let error = fixture.confirm(CODE, NOW + 1_000).await.unwrap_err();
    assert_eq!(
        error,
        PairingRefusal::NotAwaitingVerification.to_string(),
        "a pending request must be refused as the wrong stage, not completed"
    );
    assert_eq!(fixture.status().await.as_deref(), Some("pending"));
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

/// An approval whose account is no longer active authorizes nothing. The
/// approval is a decision taken minutes ago about the world as it was.
#[tokio::test]
async fn an_approval_under_a_disabled_account_authorizes_nothing() {
    let fixture = CeremonyFixture::new("authority").await;
    fixture.approved_request(NOW).await;
    let before_keys = fixture.authorized_keys().await;
    fixture
        .exec("UPDATE accounts SET status = 'disabled' WHERE id = 'acct-1'")
        .await;

    let error = fixture.confirm(CODE, NOW + 1_000).await.unwrap_err();
    assert_eq!(error, PairingRefusal::AuthorityInvalid.to_string());
    assert_eq!(
        fixture.status().await.as_deref(),
        Some("verification_failed")
    );
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

/// A revoked requester key authorizes nothing, and the revocation is checked
/// at the moment of confirmation rather than at the moment of approval.
#[tokio::test]
async fn a_requester_key_revoked_after_approval_authorizes_nothing() {
    let fixture = CeremonyFixture::new("revoked").await;
    fixture.approved_request(NOW).await;
    let before_keys = fixture.authorized_keys().await;
    fixture
        .exec(&format!(
            "INSERT INTO authorized_key_revocations (fingerprint, revoked_at_ms) \
             VALUES ('{}', 0)",
            fixture.fingerprint_of_requester().await
        ))
        .await;

    let error = fixture.confirm(CODE, NOW + 1_000).await.unwrap_err();
    assert_eq!(error, PairingRefusal::AuthorityInvalid.to_string());
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

/// A key that is a worker's identity never becomes a device. One fingerprint
/// carrying two kinds of authority is the failure `auth::principal` refuses at
/// resolution time; the ceremony refuses it earlier, where an operator can read
/// why.
#[tokio::test]
async fn a_worker_key_never_becomes_a_device() {
    let fixture = CeremonyFixture::new("worker-key").await;
    fixture.approved_request(NOW).await;
    let requester_fp = fixture.fingerprint_of_requester().await;
    fixture
        .exec(&format!(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms) \
             VALUES ('{requester_fp}', 'a machine', 'linux', 0, 0)"
        ))
        .await;
    let before_keys = fixture.authorized_keys().await;

    let error = fixture.confirm(CODE, NOW + 1_000).await.unwrap_err();
    assert_eq!(error, PairingRefusal::AuthorityInvalid.to_string());
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

/// A completed request is single-use. Confirming it a second time must not
/// authorize a second device, because the first confirmation cleared the code
/// digest that made the second one possible.
#[tokio::test]
async fn a_completed_request_is_single_use() {
    let fixture = CeremonyFixture::new("single-use").await;
    fixture.approved_request(NOW).await;
    assert!(fixture.confirm(CODE, NOW + 1_000).await.unwrap());
    let after_first = fixture.account_devices().await;

    let second = fixture.confirm(CODE, NOW + 2_000).await;
    assert!(
        second.is_err(),
        "a completed request must refuse a second code"
    );
    assert_eq!(
        fixture.account_devices().await,
        after_first,
        "no second device may appear"
    );
}

/// Denying a request revokes it: no code, right or wrong, completes it after.
#[tokio::test]
async fn a_denied_request_confirms_nothing() {
    let fixture = CeremonyFixture::new("denied").await;
    fixture.approved_request(NOW).await;
    // The deny test mirrors production's deny, which reads the LIVE row: a
    // request that was already decided is not something an approver denies.
    let row = read_live_pair_request(fixture.database.pool(), HANDLE)
        .await
        .expect("a read")
        .expect("the request");
    let live: LiveRequest = row.live().expect("a live request");
    terminalize(
        fixture.database.pool(),
        LiveSelector::ById(row.id),
        TerminalRequest::Denied,
        NOW + 1_000,
    )
    .await
    .expect("a denial");
    assert!(matches!(live, LiveRequest::AwaitingConfirmation(_)));

    let before_keys = fixture.authorized_keys().await;
    let error = fixture.confirm(CODE, NOW + 2_000).await.unwrap_err();
    assert_eq!(error, PairingRefusal::NotAwaitingVerification.to_string());
    assert_eq!(fixture.status().await.as_deref(), Some("denied"));
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

/// A device key that already belongs to a worker is refused by name, and the
/// refusal is `AlreadyExists` rather than a silent success: the operator who
/// approved this is being told the request they just approved is not the one
/// that will land.
#[tokio::test]
async fn associating_a_worker_key_is_refused_by_name() {
    let fixture = CeremonyFixture::new("association").await;
    let fingerprint = fixture.fingerprint_of_requester().await;
    fixture
        .exec(&format!(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms) \
             VALUES ('{fingerprint}', 'a machine', 'linux', 0, 0)"
        ))
        .await;

    let error = associate_paired_browser(&fixture.database, &fingerprint, ACCOUNT, NOW)
        .await
        .unwrap_err();
    assert_eq!(error.refusal(), Some(PairingRefusal::WorkerKeyConflict));
    assert_eq!(
        error.to_string(),
        PairingRefusal::WorkerKeyConflict.to_string()
    );
    assert_eq!(
        PairingRefusal::WorkerKeyConflict.code(),
        connectrpc::ErrorCode::AlreadyExists
    );
}
