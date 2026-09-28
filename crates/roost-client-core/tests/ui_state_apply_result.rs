//! The acknowledgement a target tab sends for an acknowledged layout apply, as
//! the coordinator's Sync socket decodes it.
//!
//! Pins `sendApplyResult` from `apps/web/src/lib/uiLayoutApply.ts`: the
//! correlation travels whole, the outcome maps to its wire enum, and the
//! reason is present only on a refusal.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::sync::encode::encode_sync_command;
use roost_client_core::client::ui_state::{LayoutApplyOutcome, LayoutApplyResult};
use roost_client_core::effect::SyncCommand;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::buffa::Message;

fn sent(
    result: LayoutApplyResult,
) -> (
    roost_proto::SyncClientFrame,
    roost_proto::UiApplyLayoutResult,
) {
    let bytes = encode_sync_command(&SyncCommand::UiApplyLayoutResult(result), "socket-current");
    let frame = roost_proto::SyncClientFrame::decode_from_slice(&bytes).unwrap();
    let Some(Command::UiApplyLayoutResult(answer)) = frame.command.clone() else {
        panic!("{frame:?}");
    };
    (frame, *answer)
}

#[test]
fn an_applied_answer_travels_on_the_named_socket_with_no_reason() {
    let correlation = "c".repeat(300);
    let (frame, answer) = sent(LayoutApplyResult {
        correlation_id: correlation.clone(),
        outcome: LayoutApplyOutcome::Applied,
        reason: None,
    });
    assert_eq!(frame.socket_id, "socket-current");
    assert_eq!(
        frame.ack_delivery_seq, None,
        "a control command never advances the ACK window"
    );
    assert_eq!(
        answer.correlation_id, correlation,
        "the answer is never truncated"
    );
    assert_eq!(answer.outcome, roost_proto::UiApplyLayoutOutcome::Applied);
    assert_eq!(answer.reason, None);
}

#[test]
fn a_refusal_carries_its_outcome_and_fixed_reason() {
    let (_, answer) = sent(LayoutApplyResult {
        correlation_id: "correlation-1".to_owned(),
        outcome: LayoutApplyOutcome::Rejected,
        reason: Some("The target tab UI bridge is unavailable.".to_owned()),
    });
    assert_eq!(answer.outcome, roost_proto::UiApplyLayoutOutcome::Rejected);
    assert_eq!(
        answer.reason.as_deref(),
        Some("The target tab UI bridge is unavailable.")
    );
}
