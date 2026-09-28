//! Terminal-control result shaping: the pre-write / ambiguous truth mapping for
//! `input-result`, and the reason bound. Called by `runtime::downstream` for the
//! answers it gives itself and by the input and stream owners for theirs.
//! Ports v2 `apps/worker/src/transport/coord-link-terminal-results.ts`
//! (`sendTerminalInputResult`, `boundedTerminalReason`). v2's
//! `terminalStreamFailureKind` has no port: the worker's stream outcome already
//! carries the wire enum, and `None` is the wire's UNSPECIFIED.

use roost_proto::{DAgentPrompt, DInputRequest};
use roost_protocol::ProtocolError;
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::{InputResult, TerminalInputStatus, TerminalWritePhase};

/// The longest reason, in UTF-8 bytes, an agent-originated result may carry.
const TERMINAL_REASON_MAX_BYTES: usize = 200;

/// Cut a reason to at most 200 bytes on a UTF-8 boundary. `None` and `""` are
/// both "no reason", which the wire spells as the empty string.
pub fn bounded_terminal_reason(reason: Option<&str>) -> String {
    let Some(reason) = reason else {
        return String::new();
    };
    if reason.len() <= TERMINAL_REASON_MAX_BYTES {
        return reason.to_owned();
    }
    let mut end = TERMINAL_REASON_MAX_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

/// The three request fields an `input-result` echoes, held past the await that
/// consumed the request itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputResultKey {
    pub request_id: String,
    pub session_id: String,
    pub input_seq: u64,
}

impl From<&DInputRequest> for InputResultKey {
    fn from(request: &DInputRequest) -> Self {
        Self {
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
            input_seq: request.input_seq,
        }
    }
}

impl From<&DAgentPrompt> for InputResultKey {
    fn from(request: &DAgentPrompt) -> Self {
        Self {
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
            input_seq: request.input_seq,
        }
    }
}

impl InputResultKey {
    /// The one `input-result` for this request. The phase is DERIVED from the
    /// status, never chosen: `Rejected` is only ever pre-write and everything
    /// uncertain is `Ambiguous`/unknown, because the coordinator unwinds
    /// provisional state only on pre-write and a phase that overstated
    /// certainty would license a duplicate write. An accepted result carries no
    /// reason. Refused when the coordinator's session id is not a uuid, which
    /// the wire record cannot carry.
    pub fn to_result(
        &self,
        status: TerminalInputStatus,
        written_bytes: u32,
        reason: &str,
    ) -> Result<InputResult, ProtocolError> {
        let phase = match status {
            TerminalInputStatus::Accepted => TerminalWritePhase::Written,
            TerminalInputStatus::Rejected => TerminalWritePhase::PreWrite,
            TerminalInputStatus::Ambiguous => TerminalWritePhase::Unknown,
        };
        let reason = match status {
            TerminalInputStatus::Accepted => String::new(),
            TerminalInputStatus::Rejected | TerminalInputStatus::Ambiguous => reason.to_owned(),
        };
        Ok(InputResult {
            request_id: self.request_id.clone(),
            session_id: SessionId::try_from(self.session_id.as_str())?,
            input_seq: self.input_seq,
            status,
            written_bytes,
            reason,
            phase,
        })
    }
}
