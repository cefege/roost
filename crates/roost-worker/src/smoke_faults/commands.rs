//! The fault command vocabulary: parsing a harness line into a command,
//! validating its payload, and applying it to the fault state. Called by
//! `control_socket`; depends on `PeerFaultState`. Ports `dispatchCommand`,
//! `parseCommand` and the payload validators of
//! `smoke/terminal/stack-peer-fault-worker-client.ts`.

use serde_json::Value;

use super::PeerFaultState;
use crate::peer::{MalformedPacket, OfferFault};

/// Every action the harness may send (v2 `PeerFaultCommandAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FaultAction {
    SetPacketBlackhole,
    DropNextInputResult,
    AdvanceGrantClock,
    ShrinkGrantSession,
    HoldHistoryResponse,
    ReleaseHistoryResponse,
    DropHistoryResponse,
    InjectMalformedPacket,
    SetHistoryPaused,
    HoldKeeperAdmission,
    ReleaseKeeperAdmission,
    DropNextDirectRetire,
    ArmOfferFault,
}

impl FaultAction {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "set_packet_blackhole" => Self::SetPacketBlackhole,
            "drop_next_input_result" => Self::DropNextInputResult,
            "advance_grant_clock" => Self::AdvanceGrantClock,
            "shrink_grant_session" => Self::ShrinkGrantSession,
            "hold_history_response" => Self::HoldHistoryResponse,
            "release_history_response" => Self::ReleaseHistoryResponse,
            "drop_history_response" => Self::DropHistoryResponse,
            "inject_malformed_packet" => Self::InjectMalformedPacket,
            "set_history_paused" => Self::SetHistoryPaused,
            "hold_keeper_admission" => Self::HoldKeeperAdmission,
            "release_keeper_admission" => Self::ReleaseKeeperAdmission,
            "drop_next_direct_retire" => Self::DropNextDirectRetire,
            "arm_offer_fault" => Self::ArmOfferFault,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SetPacketBlackhole => "set_packet_blackhole",
            Self::DropNextInputResult => "drop_next_input_result",
            Self::AdvanceGrantClock => "advance_grant_clock",
            Self::ShrinkGrantSession => "shrink_grant_session",
            Self::HoldHistoryResponse => "hold_history_response",
            Self::ReleaseHistoryResponse => "release_history_response",
            Self::DropHistoryResponse => "drop_history_response",
            Self::InjectMalformedPacket => "inject_malformed_packet",
            Self::SetHistoryPaused => "set_history_paused",
            Self::HoldKeeperAdmission => "hold_keeper_admission",
            Self::ReleaseKeeperAdmission => "release_keeper_admission",
            Self::DropNextDirectRetire => "drop_next_direct_retire",
            Self::ArmOfferFault => "arm_offer_fault",
        }
    }
}

/// One harness command (v2 `PeerFaultCommand`).
#[derive(Debug, Clone)]
pub(super) struct FaultCommand {
    pub(super) request_id: String,
    pub(super) action: FaultAction,
    payload: Value,
}

/// A line the harness sent, or `None` when it is not a command at all — which
/// ends the connection, as v2's `socket.destroy()` does.
pub(super) fn parse_command(line: &[u8]) -> Option<FaultCommand> {
    let parsed: Value = serde_json::from_slice(line).ok()?;
    if parsed.get("type")?.as_str()? != "command" {
        return None;
    }
    Some(FaultCommand {
        request_id: parsed.get("requestId")?.as_str()?.to_owned(),
        action: FaultAction::parse(parsed.get("action")?.as_str()?)?,
        payload: parsed.get("payload").cloned().unwrap_or(Value::Null),
    })
}

/// The `result` line answering `request_id` (v2 `PeerFaultWorkerResult`).
pub(super) fn result_line(request_id: &str, outcome: Result<Value, String>) -> Vec<u8> {
    let result = match outcome {
        Ok(value) => serde_json::json!({
            "type": "result", "requestId": request_id, "ok": true, "value": value,
        }),
        Err(error) => serde_json::json!({
            "type": "result", "requestId": request_id, "ok": false, "error": error,
        }),
    };
    let mut line = result.to_string().into_bytes();
    line.push(b'\n');
    line
}

/// Apply one command; the value or error is what the harness's promise
/// settles with.
pub(super) async fn apply_command(
    state: &PeerFaultState,
    command: &FaultCommand,
) -> Result<Value, String> {
    tracing::info!(
        action = command.action.as_str(),
        "a terminal peer fault command arrived"
    );
    match command.action {
        FaultAction::ArmOfferFault => {
            state
                .peer
                .offer()
                .arm(offer_fault_payload(&command.payload)?);
            Ok(Value::Null)
        }
        FaultAction::SetPacketBlackhole => {
            state
                .peer
                .set_packet_blackhole(boolean_payload(&command.payload)?);
            Ok(Value::Null)
        }
        FaultAction::DropNextInputResult => {
            state.direct_path.arm_input_result_drop();
            Ok(Value::Null)
        }
        FaultAction::InjectMalformedPacket => {
            let packet = malformed_packet_payload(&command.payload)?;
            if state.direct.peer_owner().inject_malformed_packet(packet) {
                Ok(Value::Null)
            } else {
                Err(
                    "no authenticated terminal peer is available for malformed packet injection"
                        .to_owned(),
                )
            }
        }
        FaultAction::SetHistoryPaused => {
            let paused = boolean_payload(&command.payload)?;
            state.direct.peer_owner().set_history_paused(paused);
            Ok(Value::Null)
        }
        other => Err(format!(
            "terminal peer fault {} is not available in this worker",
            other.as_str()
        )),
    }
}

fn boolean_payload(payload: &Value) -> Result<bool, String> {
    payload
        .as_bool()
        .ok_or_else(|| "terminal peer fault requires a boolean payload".to_owned())
}

fn offer_fault_payload(payload: &Value) -> Result<OfferFault, String> {
    payload
        .as_str()
        .and_then(OfferFault::parse)
        .ok_or_else(|| "terminal peer fault offer kind is invalid".to_owned())
}

fn malformed_packet_payload(payload: &Value) -> Result<MalformedPacket, String> {
    payload
        .as_str()
        .and_then(MalformedPacket::parse)
        .ok_or_else(|| "terminal peer malformed packet kind is invalid".to_owned())
}
