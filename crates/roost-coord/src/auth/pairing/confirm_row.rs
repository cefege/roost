//! The one row the confirmation path reads, and the only projection that
//! carries a requester key.
//!
//! Owned by the pairing slice. Split out of `confirmation` because the read and
//! the transitions it feeds are the two halves of a security decision, and
//! because a projection that carried more columns than the decision needs is a
//! projection a later caller can reach for.

use super::secrets::PAIRING_CEREMONY_VERSION;
use super::status::{ApprovedRequest, LiveRequest, RequestIdentity, StoredStatus};
use super::{PairingRefusal, PairingResult, refuse};
use crate::db::CoordDb;

/// Read the request this confirmation may act on, under its requester token.
pub(crate) async fn read_confirmable(
    database: &CoordDb,
    ephemeral_id: &str,
    requester_token_hash: &str,
) -> PairingResult<ConfirmableRow> {
    let row = sqlx::query_as::<_, ConfirmableRow>(
        "SELECT id, ephemeral_id, status, ceremony_version, expires_at_ms, label, \
                public_key, approved_account_id, approved_by_fp, \
                verification_code_hash, verification_attempts, user_agent, client_browser, \
                client_os, client_device_type, source_ip, country_code, region, city, \
                edge_identity \
           FROM pair_requests \
          WHERE ephemeral_id = ? AND requester_token_hash = ? AND requester_token_hash != ''",
    )
    .bind(ephemeral_id)
    .bind(requester_token_hash)
    .fetch_optional(database.pool())
    .await
    .map_err(|error| super::sqlx_error("pairing.confirm_read", error))?
    .ok_or_else(|| refuse(PairingRefusal::NotFound))?;
    if row.ceremony_version != i64::from(PAIRING_CEREMONY_VERSION) {
        return Err(refuse(PairingRefusal::CeremonyVersion));
    }
    Ok(row)
}

/// A request the confirmation path needs in full, read under its token.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ConfirmableRow {
    pub(crate) id: i64,
    pub(crate) ephemeral_id: String,
    pub(crate) status: String,
    pub(crate) ceremony_version: i64,
    pub(crate) expires_at_ms: i64,
    pub(crate) label: String,
    pub(crate) public_key: Vec<u8>,
    pub(crate) approved_account_id: Option<String>,
    pub(crate) approved_by_fp: Option<String>,
    pub(crate) verification_code_hash: Option<String>,
    pub(crate) verification_attempts: i64,
    pub(crate) user_agent: Option<String>,
    pub(crate) client_browser: Option<String>,
    pub(crate) client_os: Option<String>,
    pub(crate) client_device_type: Option<String>,
    pub(crate) source_ip: Option<String>,
    pub(crate) country_code: Option<String>,
    pub(crate) region: Option<String>,
    pub(crate) city: Option<String>,
    pub(crate) edge_identity: Option<String>,
}

impl ConfirmableRow {
    /// The live value this row is, or `None` because it is decided.
    pub(crate) fn live(&self) -> Option<LiveRequest> {
        match StoredStatus::parse(&self.status).ok()? {
            StoredStatus::Pending => Some(LiveRequest::AwaitingApproval(self.identity())),
            StoredStatus::VerificationRequired => {
                Some(LiveRequest::AwaitingConfirmation(ApprovedRequest {
                    identity: self.identity(),
                    approved_account_id: self.approved_account_id.clone(),
                    approved_by_fingerprint: self.approved_by_fp.clone(),
                    verification_code_hash: self.verification_code_hash.clone(),
                    verification_attempts: self.verification_attempts,
                }))
            }
            _ => None,
        }
    }

    pub(crate) fn identity(&self) -> RequestIdentity {
        RequestIdentity {
            id: self.id,
            ephemeral_id: self.ephemeral_id.clone(),
            expires_at_ms: self.expires_at_ms,
        }
    }

    /// The stored key, or the refusal a wrong-length column is.
    pub(crate) fn raw_public_key(&self) -> PairingResult<[u8; 32]> {
        self.public_key.clone().try_into().map_err(|_| {
            super::PairingError::fault(
                "coord.pairing.confirm: stored pair_requests.public_key is not 32 bytes",
            )
        })
    }

    pub(crate) fn source_ip(&self) -> String {
        self.source_ip
            .clone()
            .unwrap_or_else(|| crate::middleware::caller_origin::UNKNOWN_PEER.to_string())
    }

    pub(crate) fn client_browser(&self) -> String {
        self.client_browser.clone().unwrap_or_default()
    }

    pub(crate) fn client_os(&self) -> String {
        self.client_os.clone().unwrap_or_default()
    }

    pub(crate) fn client_device_type(&self) -> String {
        self.client_device_type.clone().unwrap_or_default()
    }
}
