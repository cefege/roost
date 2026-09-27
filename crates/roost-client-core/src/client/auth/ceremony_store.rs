//! The two tab-scoped records a ceremony must survive a reload with: the
//! requester's capability, and the approver's generated code.
//!
//! Both live in `sessionStorage` and nowhere else. That is a security property,
//! not a storage preference: the requester token is the only thing that can
//! finish a pairing, and a record that outlived the tab that created it would
//! let a second tab finish somebody else's. The approver's code is persisted
//! for a different reason — so an approval interrupted by a reload binds the
//! same code the human was already told, instead of a second one they have not
//! seen.
//!
//! **A record that does not validate is deleted, not repaired.** Both parsers
//! require an exact key set, the current ceremony version, and the canonical
//! spelling of every value; anything else is removed on read. A half-understood
//! requester token is worse than no token, because it produces polls that can
//! only ever be refused and looks like a coordinator problem.
//!
//! Ported from `apps/web/src/client/auth/pairing-ceremony.ts` and
//! `apps/web/src/client/auth/pairing-approval.ts`.

use serde_json::Value;

use crate::client::auth::ceremony::{
    PAIRING_CEREMONY_VERSION, normalize_pair_request_id, normalize_pair_requester_token,
    normalize_pair_verification_code,
};
use crate::client::auth::pairing_approval::PairApproval;
use crate::client::auth::pairing_session::PairingCeremony;
use crate::platform::KeyValueStore;

/// The session-storage key the requester's capability is retained under.
pub const PAIRING_CEREMONY_STORAGE_KEY: &str = "roost.pairingCeremony.v1";

/// The session-storage key the approver's generated code is retained under.
pub const PAIR_APPROVAL_STORAGE_KEY: &str = "roost.pairApproval.v1";

/// The requester record's key set, exactly. No more, no fewer.
const CEREMONY_KEYS: [&str; 3] = ["ceremonyVersion", "ephemeralId", "requesterToken"];

/// The approver record's key set, exactly.
const APPROVAL_KEYS: [&str; 5] = [
    "ceremonyVersion",
    "ephemeralId",
    "verificationCode",
    "requesterLabel",
    "expiresAtMs",
];

/// The ceremony's tab-scoped storage.
///
/// Wraps a [`KeyValueStore`] rather than naming `sessionStorage`, because the
/// value that matters — that this record is TAB-scoped — is expressed by which
/// store the host hands in, and a type that could be pointed at `localStorage`
/// would eventually be pointed at the wrong one.
pub struct CeremonyStore<'host> {
    storage: &'host dyn KeyValueStore,
}

/// Hand-written for the same reason as `DeviceKeyManager`'s: the storage is a
/// trait object, and a derive would make every host that implements
/// `KeyValueStore` also implement `Debug`.
///
/// Only whether each record is present is printed. The requester token is a
/// capability, and a `Debug` that rendered it would put it in every crash
/// report that ever held this value.
impl std::fmt::Debug for CeremonyStore<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CeremonyStore")
            .field(
                "holds_ceremony",
                &self.storage.get(PAIRING_CEREMONY_STORAGE_KEY).is_some(),
            )
            .field(
                "holds_approval",
                &self.storage.get(PAIR_APPROVAL_STORAGE_KEY).is_some(),
            )
            .finish()
    }
}

impl<'host> CeremonyStore<'host> {
    /// A store over the host's tab-scoped storage.
    pub fn new(storage: &'host dyn KeyValueStore) -> Self {
        Self { storage }
    }

    /// The requester's capability, or `None` when there is none or it is invalid.
    pub fn load_ceremony(&self) -> Option<PairingCeremony> {
        let raw = self.storage.get(PAIRING_CEREMONY_STORAGE_KEY);
        let parsed = parse_ceremony(raw.as_deref());
        if raw.is_some() && parsed.is_none() {
            self.storage.remove(PAIRING_CEREMONY_STORAGE_KEY);
        }
        parsed
    }

    /// Retain the requester's capability for this tab.
    pub fn save_ceremony(&self, ceremony: &PairingCeremony) {
        self.storage.set(
            PAIRING_CEREMONY_STORAGE_KEY,
            &encode(&serde_json::json!({
                "ceremonyVersion": ceremony.ceremony_version,
                "ephemeralId": ceremony.ephemeral_id,
                "requesterToken": ceremony.requester_token,
            })),
        );
    }

    /// Forget the requester's capability. Called on every terminal transition.
    pub fn clear_ceremony(&self) {
        self.storage.remove(PAIRING_CEREMONY_STORAGE_KEY);
    }

    /// The approver's generated code, or `None` when there is none or it is
    /// invalid.
    pub fn load_approval(&self) -> Option<PairApproval> {
        let raw = self.storage.get(PAIR_APPROVAL_STORAGE_KEY);
        let parsed = parse_approval(raw.as_deref());
        if raw.is_some() && parsed.is_none() {
            self.storage.remove(PAIR_APPROVAL_STORAGE_KEY);
        }
        parsed
    }

    /// Retain the approver's generated code for this tab.
    pub fn save_approval(&self, approval: &PairApproval) {
        self.storage.set(
            PAIR_APPROVAL_STORAGE_KEY,
            &encode(&serde_json::json!({
                "ceremonyVersion": approval.ceremony_version,
                "ephemeralId": approval.ephemeral_id,
                "verificationCode": approval.verification_code,
                "requesterLabel": approval.requester_label,
                "expiresAtMs": approval.expires_at_ms,
            })),
        );
    }

    /// Forget the approver's code.
    pub fn clear_approval(&self) {
        self.storage.remove(PAIR_APPROVAL_STORAGE_KEY);
    }
}

/// Serialise a record. A value this module built always encodes; the fallback
/// writes nothing rather than a half-record.
fn encode(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Parse the requester's record, or `None`.
fn parse_ceremony(raw: Option<&str>) -> Option<PairingCeremony> {
    let value = exact_object(raw, &CEREMONY_KEYS)?;
    if read_u32(&value, "ceremonyVersion")? != PAIRING_CEREMONY_VERSION {
        return None;
    }
    Some(PairingCeremony {
        ceremony_version: PAIRING_CEREMONY_VERSION,
        ephemeral_id: normalize_pair_request_id(&read_string(&value, "ephemeralId")?)?,
        requester_token: normalize_pair_requester_token(&read_string(&value, "requesterToken")?)?,
    })
}

/// Parse the approver's record, or `None`.
fn parse_approval(raw: Option<&str>) -> Option<PairApproval> {
    let value = exact_object(raw, &APPROVAL_KEYS)?;
    if read_u32(&value, "ceremonyVersion")? != PAIRING_CEREMONY_VERSION {
        return None;
    }
    let expires_at_ms = read_u64(&value, "expiresAtMs")?;
    if expires_at_ms == 0 {
        return None;
    }
    Some(PairApproval {
        ceremony_version: PAIRING_CEREMONY_VERSION,
        ephemeral_id: normalize_pair_request_id(&read_string(&value, "ephemeralId")?)?,
        verification_code: normalize_pair_verification_code(&read_string(
            &value,
            "verificationCode",
        )?)?,
        requester_label: read_string(&value, "requesterLabel")?,
        expires_at_ms,
    })
}

/// A JSON object whose key set is exactly `expected`.
///
/// The exactness is the guard. A record carrying an extra field was written by
/// something that is not this client, and reading the fields this one
/// understands out of it would adopt a value whose provenance is unknown.
fn exact_object(raw: Option<&str>, expected: &[&str]) -> Option<Value> {
    let value: Value = serde_json::from_str(raw?).ok()?;
    let object = value.as_object()?;
    if object.len() != expected.len() || !expected.iter().all(|key| object.contains_key(*key)) {
        return None;
    }
    Some(value)
}

fn read_string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn read_u32(value: &Value, key: &str) -> Option<u32> {
    u32::try_from(read_u64(value, key)?).ok()
}

fn read_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}
