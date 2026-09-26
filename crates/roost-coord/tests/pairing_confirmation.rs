//! The confirmation ceremony end to end against a real database: a request is
//! created, an approver binds a code, the requester guesses, and only the right
//! code produces a device.
//!
//! This is the path where a wrong answer is a wrong user's terminal, so every
//! branch that must NOT authorize a key has a named test, and the one that must
//! has a test that also checks the two rows the ceremony is supposed to write.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_coord::auth::authorized_keys::fingerprint_of_raw_public_key;
use roost_coord::auth::pairing::account::{
    PairRequestCreate, apply_approval, create_pair_request, read_pair_request, terminalize,
};
use roost_coord::auth::pairing::authority::associate_paired_browser;
use roost_coord::auth::pairing::confirmation::confirm_pair_request;
use roost_coord::auth::pairing::provenance::PairRequestProvenance;
use roost_coord::auth::pairing::secrets::{PAIR_VERIFICATION_ATTEMPT_LIMIT, pairing_secret_digest};
use roost_coord::auth::pairing::status::{
    ApprovalAuthority, LiveRequest, TerminalRequest,
};
use roost_coord::auth::pairing::PairingRefusal;
use roost_coord::db::CoordDb;
use sqlx::AssertSqlSafe;

const NOW: i64 = 1_700_000_000_000;
const TTL: i64 = 600_000;
const HANDLE: &str = "00112233445566778899aabbccddeeff";
const TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const CODE: &str = "483920";
const ACCOUNT: &str = "acct-1";
const APPROVER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const REQUESTER_KEY: [u8; 32] = [7; 32];

struct CeremonyFixture {
    database: CoordDb,
    root: PathBuf,
}

impl CeremonyFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-pairing-confirm-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let fixture = Self { database, root };
        fixture.seed_account().await;
        fixture
    }

    /// One active account and one approving device: the minimum a pairing
    /// ceremony needs to name an authority.
    async fn seed_account(&self) {
        self.exec(
            "INSERT INTO accounts (id, email_normalized, status, created_at_ms) \
             VALUES ('acct-1', 'owner@example.invalid', 'active', 0)",
        )
        .await;
        self.exec(&format!(
            "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
             VALUES ('{APPROVER}', x'01', 'operator laptop', 0)"
        ))
        .await;
        self.exec(&format!(
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             VALUES ('{APPROVER}', '{ACCOUNT}', 0, 0)"
        ))
        .await;
    }

    async fn exec(&self, statement: &str) {
        sqlx::query(AssertSqlSafe(statement))
            .execute(self.database.pool())
            .await
            .expect("a seed statement to apply");
    }

    /// Post a request and approve it, so the fixture starts at
    /// `verification_required` with the bound code in place.
    async fn approved_request(&self, now_ms: i64) {
        let outcome = create_pair_request(
            &self.database,
            &PairRequestCreate {
                ephemeral_id: HANDLE,
                requester_token_hash: &pairing_secret_digest(TOKEN),
                public_key: REQUESTER_KEY,
                label: "Alice's laptop",
                now_ms,
                expires_at_ms: now_ms + TTL,
                provenance: &provenance,
                edge_identity_provider: None,
                edge_identity: None,
            },
        )
        .await
        .expect("a created request");
        assert!(matches!(
            outcome,
            roost_coord::auth::pairing::account::CreateOutcome::Created { .. }
        ));
        let row = read_pair_request(&self.database.pool(), HANDLE)
            .await
            .expect("a read")
            .expect("the request");
        let live = row.live().expect("a live request");
        let authority = ApprovalAuthority {
            account_id: ACCOUNT.to_string(),
            approver_fingerprint: Some(APPROVER.to_string()),
        };
        apply_approval(
            &self.database,
            live,
            &authority,
            &pairing_secret_digest(CODE),
            now_ms,
        )
        .await
        .expect("an approval");
    }

    async fn confirm(&self, code: &str, now_ms: i64) -> Result<bool, String> {
        confirm_pair_request(
            &self.database,
            HANDLE,
            &pairing_secret_digest(TOKEN),
            &pairing_secret_digest(code),
            now_ms,
        )
        .await
        .map(|result| result.ok)
        .map_err(|error| error.reason)
    }

    async fn status(&self) -> Option<String> {
        sqlx::query_as::<_, (String,)>("SELECT status FROM pair_requests WHERE ephemeral_id = ?")
            .bind(HANDLE)
            .fetch_optional(self.database.pool())
            .await
            .expect("a status read")
            .map(|(status,)| status)
    }

    async fn authorized_keys(&self) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM authorized_keys")
            .fetch_one(self.database.pool())
            .await
            .expect("a key count")
            .0
    }

    async fn account_devices(&self) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM account_devices")
            .fetch_one(self.database.pool())
            .await
            .expect("a device count")
            .0
    }

    async fn fingerprint_of_requester(&self) -> String {
        fingerprint_of_raw_public_key(&REQUESTER_KEY)
    }
}

impl Drop for CeremonyFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

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
        Some(fingerprint_of_requester(&fixture).await.as_str()),
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
    assert_eq!(fixture.status().await.as_deref(), Some("verification_required"));
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
        assert_eq!(fixture.status().await.as_deref(), Some("verification_required"));
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
    assert_eq!(last.terminal_status, Some(TerminalRequest::VerificationFailed));
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
    .map_err(|error| error.reason);

    assert_eq!(
        wrong, absent,
        "a wrong token and a missing request must be the same answer"
    );
    assert_eq!(fixture.authorized_keys().await, before_keys);
}

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
    assert!(second.is_err(), "a completed request must refuse a second code");
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
    let row = read_pair_request(&fixture.database.pool(), HANDLE)
        .await
        .expect("a read")
        .expect("the request");
    let live: LiveRequest = row.live().expect("a live request");
    terminalize(
        &fixture.database.pool(),
        roost_coord::auth::pairing::account::LiveSelector::ById(row.id),
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
    assert_eq!(error.reason, PairingRefusal::WorkerKeyConflict.to_string());
    assert_eq!(
        PairingRefusal::WorkerKeyConflict.code(),
        connectrpc::ErrorCode::AlreadyExists
    );
}
