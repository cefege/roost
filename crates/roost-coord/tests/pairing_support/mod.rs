//! The confirmation ceremony's fixture: a real migrated database holding one
//! active account, one approving device, and the helpers a pairing test needs.
//!
//! Owned by the pairing slice. Shared by `pairing_confirmation` and
//! `pairing_confirmation_authority` so the two halves of the ceremony's test
//! suite cannot drift onto different accounts or different keys -- a drift there
//! would make one half's "authorizes nothing" assertion pass for the wrong
//! reason.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_coord::auth::authorized_keys::fingerprint_of_raw_public_key;
use roost_coord::auth::pairing::account::{PairRequestCreate, apply_approval, create_pair_request};
use roost_coord::auth::pairing::confirmation::confirm_pair_request;
use roost_coord::auth::pairing::rows::read_pair_request;
use roost_coord::auth::pairing::provenance::PairRequestProvenance;
use roost_coord::auth::pairing::secrets::pairing_secret_digest;
use roost_coord::auth::pairing::status::ApprovalAuthority;
use roost_coord::db::CoordDb;
use sqlx::AssertSqlSafe;

pub const NOW: i64 = 1_700_000_000_000;
pub const TTL: i64 = 600_000;
pub const HANDLE: &str = "00112233445566778899aabbccddeeff";
pub const TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
pub const CODE: &str = "483920";
pub const ACCOUNT: &str = "acct-1";
pub const APPROVER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
pub const REQUESTER_KEY: [u8; 32] = [7; 32];

pub struct CeremonyFixture {
    /// The migrated database every assertion reads.
    pub database: CoordDb,
    root: PathBuf,
}

impl CeremonyFixture {
    pub async fn new(label: &str) -> Self {
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
    pub async fn seed_account(&self) {
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

    pub async fn exec(&self, statement: &str) {
        sqlx::query(AssertSqlSafe(statement))
            .execute(self.database.pool())
            .await
            .expect("a seed statement to apply");
    }

    /// Post a request and approve it, so the fixture starts at
    /// `verification_required` with the bound code in place.
    pub async fn approved_request(&self, now_ms: i64) {
        // A request with no front door and a plain desktop client: enough for
        // the ceremony's own rules, and nothing a requester could assert.
        let provenance = PairRequestProvenance {
            source_ip: "10.0.0.4".to_string(),
            client_device_type: Some(roost_coord::auth::pairing::provenance::ClientDeviceType::Desktop),
            ..PairRequestProvenance::default()
        };
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
        let row = read_pair_request(self.database.pool(), HANDLE)
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

    pub async fn confirm(&self, code: &str, now_ms: i64) -> Result<bool, String> {
        confirm_pair_request(
            &self.database,
            HANDLE,
            &pairing_secret_digest(TOKEN),
            &pairing_secret_digest(code),
            now_ms,
        )
        .await
        .map(|result| result.ok)
        .map_err(|error| error.to_string())
    }

    pub async fn status(&self) -> Option<String> {
        sqlx::query_as::<_, (String,)>("SELECT status FROM pair_requests WHERE ephemeral_id = ?")
            .bind(HANDLE)
            .fetch_optional(self.database.pool())
            .await
            .expect("a status read")
            .map(|(status,)| status)
    }

    pub async fn authorized_keys(&self) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM authorized_keys")
            .fetch_one(self.database.pool())
            .await
            .expect("a key count")
            .0
    }

    pub async fn account_devices(&self) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM account_devices")
            .fetch_one(self.database.pool())
            .await
            .expect("a device count")
            .0
    }

    pub async fn fingerprint_of_requester(&self) -> String {
        fingerprint_of_raw_public_key(&REQUESTER_KEY)
    }
}

impl Drop for CeremonyFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
