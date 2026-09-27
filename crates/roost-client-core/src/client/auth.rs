//! The authentication ceremony: a non-extractable device key, the signed
//! coordinator credential derived from it, and the pairing requests that enroll
//! it.
//!
//! Nine files, one concern each. `keystore` names what a host must be able to
//! do with the key; `memory_keystore` is the reference implementation of that
//! discipline and the only one a unit test can drive; `jwt` builds the EdDSA
//! token the coordinator verifies and holds the one rule about a signing
//! failure; `key_rotation` is the vocabulary and the decision table a rotation
//! turns on; `device_key` is the load-or-generate, rotate and reset lifecycle
//! that decides which key signs; `ceremony` owns the portable entropy and the
//! canonical validation of the pairing values; `ceremony_store` retains the two
//! tab-scoped records; `pairing_requests` and `pairing_session` are the wire
//! bodies and the stages that order them; and `redeem` spends a one-time grant.
//!
//! The load-bearing property is the one the trait shape enforces: no method
//! anywhere under this module returns private key material, and none of the
//! requests built here can carry any. The second is that there is no `put` —
//! first boot and rotation both write with an `add` that refuses an occupied
//! slot, so two browsers opening the ceremony at once converge on one key
//! instead of one silently replacing the other.
//!
//! Contract: `protocol/spec/auth-and-pairing.md`. Ported from
//! `apps/web/src/client/auth/*`.

pub mod ceremony;
pub mod ceremony_store;
pub mod device_key;
pub mod jwt;
pub mod key_rotation;
pub mod keystore;
pub mod memory_keystore;
pub mod pairing_approval;
pub mod pairing_requests;
pub mod pairing_session;
pub mod redeem;

pub use ceremony::{
    CeremonyError, CountingRandomSource, FixedRandomSource, PAIRING_CEREMONY_VERSION,
    PAIR_REQUESTER_TOKEN_BYTES, PAIR_REQUEST_ID_BYTES, PAIR_VERIFICATION_CODE_LENGTH,
    RandomSource, compact_pair_verification_code, generate_pair_request_id,
    generate_pair_requester_token, generate_pair_verification_code, normalize_pair_request_id,
    normalize_pair_requester_token, normalize_pair_verification_code,
};
pub use ceremony_store::{CeremonyStore, PAIRING_CEREMONY_STORAGE_KEY, PAIR_APPROVAL_STORAGE_KEY};
pub use device_key::{CurrentKeyInfo, DeviceKeyManager};
pub use jwt::{
    CoordinatorJwt, JWT_ALGORITHM, JWT_AUDIENCE, JWT_CACHE_TTL_MS, JWT_LIFETIME_SECS, JWT_TYPE,
    UnsignedJwt, bearer_for_signing, build_unsigned_jwt, public_key_b64,
};
pub use key_rotation::{
    DeviceKeyProbe, DeviceKeyRotator, ResetOutcome, RotationError, RotationOutcome, RotationRecovery,
    RotationRefusal, RotationRequest, recover_rotation,
};
pub use keystore::{
    DEVICE_KEY_SLOT, DeviceKey, KEY_MINTED_FLAG, KeyAdmission, KeyDescriptor, KeyStoreError,
    ROTATION_STAGE_SLOT, RotationStage, SecureKeyStore,
};
pub use memory_keystore::MemorySecureKeyStore;
pub use pairing_requests::{
    PairApprovalStatusRequest, PairApproveRequest, PairConfirmRequest, PairConfirmResponse,
    PairCreateRequest, PairCreateResponse, PairDenyRequest, PairPollRequest, PairPollResponse,
};
pub use pairing_approval::PairApproval;
pub use pairing_session::{
    PairPollStatus, PairStage, PairingCeremony, PairingError, PairingSession,
};
pub use redeem::{
    AuthRedeemBrowserRequest, RedeemCall, RedeemOutcome, RedeemRefusal, RefusalCode,
    redeem_pair_token,
};
