//! What the upload driver will do with one chosen file, decided before a byte
//! moves: whether the worker's dedup probe can answer it, and which carrier
//! the bytes take when it cannot.
//!
//! Split out from the host because both answers are facts about the request
//! that a test can pin without a browser, a coordinator or a worker. The host
//! half is `super::upload`, which performs the plan this module decides.
//! Ports the decision half of `apps/web/src/lib/attachments.ts` (the
//! `DEDUP_MAX_BYTES` cap and the direct-then-relay order) and the refusal
//! vocabulary of `client::attachments::direct`.

use roost_client_core::client::attachments::direct::{
    AttachmentDirectUploadRequest, DirectAttempt, DirectRoute, DirectUnavailableReason,
};
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferResult, MAX_SAFE_TOTAL_BYTES,
};

/// Above this the browser skips the content probe. Hashing a multi-gigabyte
/// file to maybe save an upload would defeat the O(chunk) memory design the
/// whole path is built on; the worker still records the content hash on save.
pub const DEDUP_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// What one chosen file is going to become.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPlan {
    /// The upload id every frame, card and chunk names.
    pub upload_id: String,
    /// The file's name, as the worker will store it.
    pub file_name: String,

    /// Its whole length; a size a peer could not read exactly is refused here.
    pub total_bytes: u64,
    /// Whether the dedup probe runs first. A zero-byte file has no content to
    /// compare, and a file past the cap is not worth hashing.
    pub probe_first: bool,
    /// The request the direct route selection runs against.
    pub direct_request: AttachmentDirectUploadRequest,
}

/// Why one file cannot be uploaded at all, before any carrier is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanRefusal {
    /// The size crosses the wire as a number a peer reads back, and this one
    /// is larger than a JavaScript safe integer, so the far end could not
    /// read it exactly. Refusing here is refusing a request that would have
    /// been refused there, after the user waited for it.
    UnrepresentableFileSize,
}

impl PlanRefusal {
    /// The reason the transfer card carries.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::UnrepresentableFileSize => {
                "file is too large to upload: its size is not a number a peer can read exactly"
            }
        }
    }
}

impl UploadPlan {
    /// Decide the plan for one file, or name the refusal.
    ///
    /// The direct request is built even when no route will carry it, because
    /// `upload_attachment_direct` is what decides that, and it decides it from
    /// the same request this hands it.
    pub fn for_file(
        session_id: &str,
        worker_fp: Option<&str>,
        upload_id: String,
        file_name: String,
        total_bytes: u64,
        short_path: bool,
    ) -> Result<Self, PlanRefusal> {
        if total_bytes > MAX_SAFE_TOTAL_BYTES {
            return Err(PlanRefusal::UnrepresentableFileSize);
        }
        let direct_request = AttachmentDirectUploadRequest {
            worker_fp: worker_fp.map(str::to_owned),
            session_id: session_id.to_owned(),
            upload_id: upload_id.clone(),
            file_name: file_name.clone(),
            file_bytes: total_bytes,
            short_path,
        };
        Ok(Self {
            probe_first: total_bytes > 0 && total_bytes <= DEDUP_MAX_BYTES,
            upload_id,
            file_name,
            total_bytes,
            direct_request,
        })
    }
}

/// Which carrier the bytes take, named so a log and a card agree.
///
/// `direct` is a family, not a carrier: the loopback door and the peer are
/// different transports with different failure modes, and a card that said
/// only "direct" would hide which one lost the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierChoice {
    /// A direct carrier is open and the chunk loop runs on it.
    Direct {
        route: DirectRoute,
        upload: Box<roost_client_core::client::attachments::transfer::DirectUpload>,
    },
    /// No direct route carried this upload, and the named reason is why. The
    /// coordinator relay is the carrier.
    Relay { reason: DirectUnavailableReason },
    /// A route put bytes on the wire and failed. Only that route's own status
    /// control may settle them, so nothing else may carry this file. The
    /// route is deliberately unnamed: naming it here would invite a caller to
    /// try the other one, which is exactly the doubled upload this exists to
    /// prevent.
    FailedWithBytes { reason: String },
    /// The route failed in a way that is not a carrier refusal. The upload
    /// fails here and does NOT fall back to the relay.
    Failed { route: DirectRoute, reason: String },
}

impl CarrierChoice {
    /// Read the route decision the client core made.
    #[must_use]
    pub fn from_attempt(attempt: DirectAttempt) -> Self {
        match attempt {
            DirectAttempt::Opened { route, upload } => Self::Direct { route, upload },
            DirectAttempt::Unavailable(reason) => Self::Relay { reason },
            DirectAttempt::FailedWithBytes(error) => Self::FailedWithBytes {
                reason: error.to_string(),
            },
            DirectAttempt::Failed { route, reason } => Self::Failed { route, reason },
        }
    }

    /// The carrier name a log line and the worker receipt agree on. The direct
    /// routes carry the worker's own `Carrier::as_str()` spelling, because the
    /// worker is what writes the receipt.
    #[must_use]
    pub fn carrier_name(&self) -> &'static str {
        match self {
            Self::Direct { route, .. } | Self::Failed { route, .. } => match route {
                DirectRoute::Loopback => "loopback",
                DirectRoute::Peer => "webrtc",
            },
            // A route that lost bytes is named by the route that had them, and
            // the core does not carry which one it was: the only honest answer
            // is that the carrier is unknown, and a log that says so is more
            // useful than one that guesses.
            Self::FailedWithBytes { .. } => "unknown",
            Self::Relay { .. } => "coordinator",
        }
    }
}

/// How a finished upload reached the user, which is what the card shows and
/// whether anything may be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadOutcome {
    /// The worker committed the file at this path.
    Accepted(AttachmentTransferResult),
    /// The worker already held these exact bytes; nothing was uploaded.
    Deduplicated { abs_path: String },
    /// The upload failed with nothing on a wire that could have landed.
    Rejected { reason: String },
    /// The bytes may or may not have been committed: the acknowledgement was
    /// lost and no receipt could confirm it. NEVER retried — a retried
    /// ambiguous write is a doubled upload.
    Ambiguous { reason: String },
}

impl UploadOutcome {
    /// Whether an automatic retry is ever permitted for this outcome.
    ///
    /// False for all four, and that is the point of the method rather than a
    /// per-variant answer: a caller that grew a fifth outcome cannot inherit
    /// permission to retry by forgetting to think about it.
    #[must_use]
    pub fn permits_automatic_retry(&self) -> bool {
        false
    }

    /// The path to insert into a PTY, when there is one to insert.
    #[must_use]
    pub fn abs_path(&self) -> Option<&str> {
        match self {
            Self::Accepted(result) => Some(result.abs_path.as_str()),
            Self::Deduplicated { abs_path } => Some(abs_path.as_str()),
            Self::Rejected { .. } | Self::Ambiguous { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(total_bytes: u64) -> UploadPlan {
        UploadPlan::for_file(
            "session-1",
            Some("worker-1"),
            "upload-1".to_owned(),
            "notes.txt".to_owned(),
            total_bytes,
            false,
        )
        .expect("a representable size plans")
    }

    #[test]
    fn a_size_a_peer_cannot_read_exactly_is_refused_before_a_carrier_is_chosen() {
        let refusal = UploadPlan::for_file(
            "session-1",
            Some("worker-1"),
            "upload-1".to_owned(),
            "huge.bin".to_owned(),
            MAX_SAFE_TOTAL_BYTES + 1,
            false,
        );
        assert_eq!(refusal, Err(PlanRefusal::UnrepresentableFileSize));
        assert!(
            refusal
                .unwrap_err()
                .message()
                .contains("not a number a peer")
        );
    }

    #[test]
    fn a_zero_byte_file_skips_the_probe_because_it_has_no_content_to_compare() {
        assert!(!plan(0).probe_first);
        assert!(plan(1).probe_first);
    }

    #[test]
    fn a_file_past_the_probe_cap_is_uploaded_without_being_hashed() {
        assert!(!plan(DEDUP_MAX_BYTES + 1).probe_first);
    }

    #[test]
    fn the_cap_itself_still_probes_because_the_bound_is_inclusive() {
        assert!(plan(DEDUP_MAX_BYTES).probe_first);
    }

    #[test]
    fn the_direct_request_names_the_same_upload_the_card_does() {
        let plan = plan(12);
        assert_eq!(plan.direct_request.upload_id, plan.upload_id);
        assert_eq!(plan.direct_request.file_name, plan.file_name);
        assert_eq!(plan.direct_request.file_bytes, plan.total_bytes);
        assert_eq!(plan.direct_request.session_id, "session-1");
    }

    #[test]
    fn a_session_with_no_worker_names_no_worker_so_no_route_can_match_a_door() {
        let plan = UploadPlan::for_file(
            "session-1",
            None,
            "upload-1".to_owned(),
            "notes.txt".to_owned(),
            12,
            false,
        )
        .expect("a representable size plans");
        assert_eq!(plan.direct_request.worker_fp, None);
    }

    #[test]
    fn no_outcome_ever_permits_an_automatic_retry() {
        let outcomes = [
            UploadOutcome::Accepted(AttachmentTransferResult {
                abs_path: "/tmp/a".to_owned(),
            }),
            UploadOutcome::Deduplicated {
                abs_path: "/tmp/a".to_owned(),
            },
            UploadOutcome::Rejected {
                reason: "refused".to_owned(),
            },
            UploadOutcome::Ambiguous {
                reason: "unconfirmed".to_owned(),
            },
        ];
        for outcome in outcomes {
            assert!(!outcome.permits_automatic_retry());
        }
    }

    #[test]
    fn an_ambiguous_outcome_offers_no_path_to_insert_because_it_may_not_exist() {
        let outcome = UploadOutcome::Ambiguous {
            reason: "the acknowledgement was lost".to_owned(),
        };
        assert_eq!(outcome.abs_path(), None);
    }

    #[test]
    fn a_deduplicated_outcome_still_offers_the_path_the_worker_already_holds() {
        let outcome = UploadOutcome::Deduplicated {
            abs_path: "/tmp/held".to_owned(),
        };
        assert_eq!(outcome.abs_path(), Some("/tmp/held"));
    }
}
