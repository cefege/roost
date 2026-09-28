//! The checks a grant must pass: the coordinator's install frame, and a
//! hello's credential against the stored grant, its secret compared by digest
//! without an early exit. Ports `validGrantFrame`, `verify` and
//! `sameDescriptor` of v2 `apps/worker/src/attachments/attachment-grants.ts`
//! and `verifyLocalEndpointCapability` of `packages/host/src/local-endpoint.ts`.
//! Called by `grants`.

use roost_proto::DLocalAttachmentGrant;
use roost_protocol::attachment_transfer::{GRANT_TTL_MS, is_chunk_sha256};
use sha2::{Digest, Sha256};

use super::file_hash::sha256_hex;
use super::grants::{AttachmentGrant, AttachmentGrantCredential, StoredGrant};
use super::journal::{MAX_SAFE_INTEGER, valid_opaque_id};

const MAX_FILENAME_BYTES: usize = 255;

pub(super) fn verify_stored(
    stored: &StoredGrant,
    credential: &AttachmentGrantCredential,
) -> Result<AttachmentGrant, &'static str> {
    let grant = &stored.grant;
    if credential.total_bytes > MAX_SAFE_INTEGER {
        return Err("attachment grant size is invalid");
    }
    let same_descriptor = grant.session_id == credential.session_id
        && grant.upload_id == credential.upload_id
        && grant.filename == credential.filename
        && grant.short_path == credential.short_path
        && grant.total_bytes == credential.total_bytes
        && grant.worker_epoch == credential.worker_epoch;
    if !same_descriptor
        || grant.device_fingerprint != credential.device_fingerprint
        || grant.tab_id != credential.tab_id
    {
        return Err("attachment grant does not match upload");
    }
    let received = sha256_hex(credential.secret.as_bytes());
    if !capability_matches(&stored.secret_sha256, &received) {
        return Err("attachment grant secret mismatch");
    }
    Ok(grant.clone())
}

/// v2 `verifyLocalEndpointCapability`: both sides hashed to a fixed length and
/// compared without an early exit, so timing reveals nothing about the digest.
fn capability_matches(expected: &str, received: &str) -> bool {
    let expected = Sha256::digest(expected.as_bytes());
    let received = Sha256::digest(received.as_bytes());
    expected
        .iter()
        .zip(received.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

pub(super) fn valid_grant_frame(frame: &DLocalAttachmentGrant, worker_epoch: &str) -> bool {
    valid_opaque_id(&frame.request_id)
        && valid_opaque_id(&frame.grant_id)
        && is_chunk_sha256(&frame.secret_sha256)
        && valid_opaque_id(&frame.session_id)
        && valid_opaque_id(&frame.upload_id)
        && (1..=MAX_FILENAME_BYTES).contains(&frame.filename.len())
        && !frame.filename.contains('\0')
        && frame.ttl_ms > 0
        && u64::from(frame.ttl_ms) <= GRANT_TTL_MS
        && frame.worker_epoch == worker_epoch
        && frame.total_bytes <= MAX_SAFE_INTEGER
        && valid_opaque_id(&frame.device_fingerprint)
        && valid_opaque_id(&frame.tab_id)
}
