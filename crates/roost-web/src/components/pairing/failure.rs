//! What a failed pair RPC MEANS, and how long to wait before trying again.
//!
//! The whole ceremony turns on the line between "the coordinator looked at this
//! and said no" and "nothing came back". A requester that discarded its
//! requester token on a network blip has thrown away the only capability that
//! can finish a pairing, and an approver that discarded its generated code on a
//! blip has told a human a six-digit number and then forgotten it. So a
//! transient failure retries and keeps everything, and only a considered refusal
//! retires the ceremony.
//!
//! Ports `apps/web/src/client/auth/pairing-transient-error.ts` and the two
//! `backoffDelayMs` call sites in
//! `apps/web/src/store/auth/pair-approval-lifecycle.ts:68-73,149-155` and
//! `onboarding-pairing-ceremony.ts:28-30,130-140`.

use roost_client_core::client::carriers::retry_delay_ms as carrier_retry_delay_ms;
use roost_client_core::client::rpc::{AuthFailureKind, CallError, ConnectCode};

/// Whether the failure is worth another attempt.
///
/// A transport failure and the five transient Connect codes are. Everything
/// else is the coordinator's considered answer — INCLUDING a response this
/// client could not decode, which carries no code and is not a transport
/// failure: retrying an undecodable answer forever would be a page that never
/// stops asking.
pub fn is_transient(error: &CallError) -> bool {
    match error.code() {
        None => matches!(error, CallError::Network(_)),
        Some(code) => matches!(
            code,
            ConnectCode::Unknown
                | ConnectCode::Unavailable
                | ConnectCode::DeadlineExceeded
                | ConnectCode::Aborted
                | ConnectCode::ResourceExhausted
        ),
    }
}

/// How long to wait before attempt number `attempt + 1`.
///
/// The same 1s/2s/4s doubling capped at 30s the carrier loop uses, so the
/// ceremony and the transports cannot drift into different patience.
pub fn retry_delay_ms(attempt: u32) -> u64 {
    carrier_retry_delay_ms(attempt)
}

/// The sentence a person reads for a failure.
///
/// The transport's own words rather than a rewritten summary: a coordinator
/// refusing a stale ceremony version says `FailedPrecondition: pairing client
/// must reload`, and that sentence IS the instruction.
pub fn describe(error: &CallError) -> String {
    error.to_string()
}

/// What a failure MEANT for an approver's ability to keep following the
/// ceremony.
///
/// `Retry` is not a decision but a request to try again with the code intact.
/// The other three are decisions, and the difference between `Gone` and
/// `Reload` is whether a follow-up status read can still say what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalFailure {
    /// Worth another attempt; keep the generated code on screen.
    Retry,
    /// The request has already left the live states; only a status read can say
    /// whether this very denial already did it.
    Gone,
    /// This client is no longer a principal the ceremony answers to.
    Authority,
    /// Anything else — a stale ceremony version, an older coordinator. The code
    /// stays valid for the requester; only this page can no longer follow it.
    Reload,
}

impl ApprovalFailure {
    /// Classify a failure on one of the approver's three methods.
    ///
    /// An `Unauthenticated` is an authority loss ONLY when the coordinator
    /// classifies it as a device rejection, which `methods.rs:32-42` decides by
    /// method name. On the other methods the same code is a stale credential
    /// behind a front door, and v2 retries it
    /// (`pair-approval-lifecycle.ts:68-73`); treating every 401 as terminal
    /// would strand an approver whose credential had merely aged out.
    pub fn classify(error: &CallError, method: &str) -> Self {
        if error.code() == Some(&ConnectCode::Unauthenticated)
            && error.auth_failure_kind(method) == AuthFailureKind::Device
        {
            return Self::Authority;
        }
        if is_transient(error) {
            return Self::Retry;
        }
        match error.code() {
            Some(ConnectCode::NotFound) => Self::Gone,
            Some(ConnectCode::Unauthenticated | ConnectCode::PermissionDenied) => Self::Authority,
            _ => Self::Reload,
        }
    }
}

#[cfg(test)]
mod tests {
    use roost_client_core::client::rpc::{ConnectError, RpcCodecError};

    use super::*;

    fn connect(code: &ConnectCode) -> CallError {
        CallError::Connect(ConnectError {
            code: code.clone(),
            message: "coordinator said no".to_owned(),
            auth_layer: None,
        })
    }

    #[test]
    fn a_transport_failure_and_the_five_transient_codes_retry() {
        assert!(is_transient(&CallError::Network("offline".to_string())));
        for code in [
            ConnectCode::Unknown,
            ConnectCode::Unavailable,
            ConnectCode::DeadlineExceeded,
            ConnectCode::Aborted,
            ConnectCode::ResourceExhausted,
        ] {
            assert!(is_transient(&connect(&code)), "{code:?} must retry");
        }
    }

    #[test]
    fn a_considered_refusal_never_retries() {
        for code in [
            ConnectCode::InvalidArgument,
            ConnectCode::PermissionDenied,
            ConnectCode::Unauthenticated,
            ConnectCode::NotFound,
            ConnectCode::FailedPrecondition,
        ] {
            assert!(!is_transient(&connect(&code)), "{code:?} must not retry");
        }
        assert!(!is_transient(&CallError::Codec(
            RpcCodecError::MalformedResponse {
                method: "PairPoll",
                detail: "not a message".to_string(),
            }
        )));
    }

    #[test]
    fn the_approvers_own_unauthenticated_is_an_authority_loss() {
        let error = connect(&ConnectCode::Unauthenticated);
        assert_eq!(
            ApprovalFailure::classify(&error, "PairApprovalStatus"),
            ApprovalFailure::Authority
        );
        assert_eq!(
            ApprovalFailure::classify(&error, "PairDeny"),
            ApprovalFailure::Authority
        );
    }

    #[test]
    fn a_missing_request_reads_as_gone_not_as_a_refusal() {
        assert_eq!(
            ApprovalFailure::classify(&connect(&ConnectCode::NotFound), "PairDeny"),
            ApprovalFailure::Gone
        );
    }

    #[test]
    fn only_a_five_second_wait_stays_five_seconds() {
        assert_eq!(retry_delay_ms(0), 1_000);
        assert_eq!(retry_delay_ms(1), 2_000);
        assert_eq!(retry_delay_ms(2), 4_000);
        assert_eq!(retry_delay_ms(30), 30_000);
    }
}
