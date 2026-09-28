//! The capture request's wire validation: which request becomes a command, and
//! the code and field every other one is refused with. The bounds are
//! v2 `validateTerminalCaptureRequest` (`packages/protocol/src/terminal-capture.ts`);
//! a control call that could carry evidence or a non-manual reason would slip
//! a payload past the CAPTURE size gate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_proto::{TerminalCaptureAction, TerminalCaptureRequest};
use roost_protocol::terminal_capture::bundle::TerminalCaptureReason;
use roost_protocol::terminal_capture::command::{
    TerminalCaptureActionName, validate_terminal_capture_request,
};
use roost_protocol::terminal_capture::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode as Code};

const SESSION: &str = "22222222-2222-4222-8222-222222222222";
const CAPTURE: &str = "33333333-3333-4333-8333-333333333333";
const RECORDING: &str = "44444444-4444-4444-8444-444444444444";

fn request(action: TerminalCaptureAction, reason: &str, evidence: &str) -> TerminalCaptureRequest {
    TerminalCaptureRequest {
        action: action.into(),
        session_id: SESSION.to_owned(),
        recording_id: RECORDING.to_owned(),
        capture_id: CAPTURE.to_owned(),
        reason: reason.to_owned(),
        browser_evidence_json: evidence.to_owned(),
        ..Default::default()
    }
}

#[test]
fn a_capture_with_evidence_becomes_a_command() {
    let command = validate_terminal_capture_request(&request(
        TerminalCaptureAction::Capture,
        "history_identity",
        "{}",
    ))
    .expect("a well-formed capture");

    assert_eq!(command.action, TerminalCaptureActionName::Capture);
    assert_eq!(command.reason, TerminalCaptureReason::HistoryIdentity);
    assert_eq!(command.session_id, SESSION);
    assert_eq!(command.browser_evidence_json, "{}");
}

#[test]
fn every_out_of_bound_request_names_its_code_and_field() {
    let mut bad_recording = request(TerminalCaptureAction::Start, "manual", "");
    bad_recording.recording_id = "not-a-uuid".to_owned();
    let oversized = "x".repeat(TERMINAL_CAPTURE_LIMITS.browser_evidence_bytes + 1);
    let cases = [
        (
            request(TerminalCaptureAction::Unspecified, "manual", ""),
            (Code::InvalidArgument, "action"),
        ),
        (bad_recording, (Code::InvalidArgument, "recording_id")),
        (
            request(TerminalCaptureAction::Capture, "because", "{}"),
            (Code::InvalidArgument, "reason"),
        ),
        (
            request(TerminalCaptureAction::Start, "manual", "{}"),
            (Code::InvalidArgument, "browser_evidence_json"),
        ),
        (
            request(TerminalCaptureAction::Stop, "pre_repair", ""),
            (Code::InvalidArgument, "reason"),
        ),
        (
            request(TerminalCaptureAction::Capture, "manual", &oversized),
            (Code::EvidenceTooLarge, "browser_evidence_json"),
        ),
    ];
    for (request, refusal) in cases {
        assert_eq!(
            validate_terminal_capture_request(&request).expect_err("out of bound"),
            refusal
        );
    }
}
