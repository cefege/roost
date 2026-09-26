//! The confirmation ceremony's code path, against a real database: a request
//! is created, an approver binds a code, the requester guesses, and only the
//! right code produces a device.
//!
//! This is the path where a wrong answer is a wrong user's terminal, so every
//! branch that must NOT authorize a key has a named test, and the one that must
//! has a test that also checks the two rows the ceremony is supposed to write.
//! The refusals that must authorize nothing are in
//! `pairing_confirmation_authority.rs`, against the same fixture.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod pairing_support;

use pairing_support::{CODE, CeremonyFixture, HANDLE, NOW, TOKEN};
use roost_coord::auth::pairing::confirmation::confirm_pair_request;
use roost_coord::auth::pairing::secrets::{PAIR_VERIFICATION_ATTEMPT_LIMIT, pairing_secret_digest};
use roost_coord::auth::pairing::status::TerminalRequest;

/// The whole ceremony, and the two rows only it may write.
#[tokio::test]
async fn the_right_code_creates_the_key_and_the_device() {
    let fixture = CeremonyFixture::new("complete").await;
    fixture.approved_request(NOW).await;
    let before_keys = fixture.authorized_keys().await;
    let before_devices = fixture.account_devices().await;

    let result = confirm_pair_request(
        &fixture.database,
        HANDLE,
        &pairing_secret_digest(TOKEN),
        &pairing_secret_digest(CODE),
        NOW + 1_000,
    )
    .await
    .expect("a completed confirmation");

    assert!(result.ok);
    assert_eq!(
        result.newly_authorized_fingerprint.as_deref(),
        Some(fixture.fingerprint_of_requester().await.as_str()),
        "the completing call is the one that names the fingerprint it authorized"
    );
    let notice = result.paired_browser.expect("a paired-browser notice");
    assert_eq!(notice.ephemeral_id, HANDLE);
    assert_eq!(notice.label, "Alice's laptop");
    assert_eq!(notice.paired_at_ms, NOW + 1_000);
    assert_eq!(fixture.status().await.as_deref(), Some("completed"));
    assert_eq!(fixture.authorized_keys().await, before_keys + 1);
    assert_eq!(fixture.account_devices().await, before_devices + 1);
}

/// A wrong code is a RETRY, not a failure: `ok` is false, the request is still
/// live, and nothing was authorized. A requester that mistypes its own code
/// must be able to try again, and a device must not appear because it guessed.
#[tokio::test]
async fn a_wrong_code_is_a_retry_and_authorizes_nothing() {
    let fixture = CeremonyFixture::new("wrong-code").await;
    fixture.approved_request(NOW).await;
    let before_keys = fixture.authorized_keys().await;

    let result = confirm_pair_request(
        &fixture.database,
        HANDLE,
        &pairing_secret_digest(TOKEN),
        &pairing_secret_digest("000000"),
        NOW + 1_000,
    )
    .await
    .expect("a rejected confirmation");

    assert!(!result.ok);
    assert_eq!(result.newly_authorized_fingerprint, None);
    assert_eq!(result.paired_browser, None);
    assert_eq!(result.terminal_status, None, "the series is not over yet");
    assert_eq!(
        fixture.status().await.as_deref(),
        Some("verification_required")
    );
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

/// The attempt series ends at the bound. Before the bound a wrong code is a
/// retry; on the bound it is terminal, and the request stops accepting codes
/// entirely -- which is the difference between a six-digit code and a
/// ten-thousand-guess credential.
#[tokio::test]
async fn the_attempt_series_ends_the_request_at_the_bound() {
    let fixture = CeremonyFixture::new("attempts").await;
    fixture.approved_request(NOW).await;

    for attempt in 1..PAIR_VERIFICATION_ATTEMPT_LIMIT {
        let result = confirm_pair_request(
            &fixture.database,
            HANDLE,
            &pairing_secret_digest(TOKEN),
            &pairing_secret_digest("000000"),
            NOW + 1_000,
        )
        .await
        .expect("a rejected confirmation");
        assert!(!result.ok, "attempt {attempt} must not succeed");
        assert_eq!(
            result.terminal_status, None,
            "attempt {attempt} of {PAIR_VERIFICATION_ATTEMPT_LIMIT} must not end the series"
        );
        assert_eq!(
            fixture.status().await.as_deref(),
            Some("verification_required")
        );
    }

    let last = confirm_pair_request(
        &fixture.database,
        HANDLE,
        &pairing_secret_digest(TOKEN),
        &pairing_secret_digest("000000"),
        NOW + 1_000,
    )
    .await
    .expect("the final rejected confirmation");
    assert_eq!(
        last.terminal_status,
        Some(TerminalRequest::VerificationFailed)
    );
    assert_eq!(
        fixture.status().await.as_deref(),
        Some("verification_failed")
    );

    let after = fixture.confirm(CODE, NOW + 1_000).await;
    assert!(
        after.is_err(),
        "a terminal request must refuse the right code too, got {after:?}"
    );
}

/// The requester token is the requester's only proof it owns the request, and a
/// token that does not match must be indistinguishable from a request that does
/// not exist.
#[tokio::test]
async fn a_wrong_requester_token_finds_nothing() {
    let fixture = CeremonyFixture::new("token").await;
    fixture.approved_request(NOW).await;
    let before_keys = fixture.authorized_keys().await;

    let wrong = fixture.confirm(CODE, NOW + 1_000).await;
    let absent = confirm_pair_request(
        &fixture.database,
        "ffffffffffffffffffffffffffffffff",
        &pairing_secret_digest(TOKEN),
        &pairing_secret_digest(CODE),
        NOW + 1_000,
    )
    .await
    .map(|result| result.ok)
    .map_err(|error| error.to_string());

    assert_eq!(
        wrong, absent,
        "a wrong token and a missing request must be the same answer"
    );
    assert_eq!(fixture.authorized_keys().await, before_keys);
}
