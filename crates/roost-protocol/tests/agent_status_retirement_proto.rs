//! An inactive agent status crosses the worker link as its occupant's
//! retirement. v2's coordinator folds every update and parses the full status
//! only when it is active (`apps/coord/src/agents/agent-status-hub.ts:136-140`),
//! so `decode_upstream` admits `active: false` under the common bounds.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::proto_adapters::coord_worker_proto::{decode_upstream, encode_upstream};
use roost_protocol::wire::agent_status::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusSource,
    StatusEpoch,
};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::{AgentStatusFrame, CoordWorkerUpstream};

const OCCUPANT: &str = "6f1a0b1e-6c1f-4a3a-9f0e-2b7d5c8e4a11";

fn frame(active: bool, revision: i64, completed_revision: i64) -> CoordWorkerUpstream {
    CoordWorkerUpstream::AgentStatus(AgentStatusFrame {
        status: AgentStatus {
            common: AgentStatusFields {
                session_id: SessionId::try_from("11111111-1111-4111-8111-111111111111").unwrap(),
                agent_id: AgentId::try_from("omp").unwrap(),
                state: AgentRuntimeState::Idle,
                message: None,
                revision,
                completed_revision,
                updated_at: 1_700_000_000_001,
                status_epoch: Some(StatusEpoch::try_from(OCCUPANT).unwrap()),
                occupant_id: Some(AgentOccupantId::try_from(OCCUPANT).unwrap()),
                source: Some(AgentStatusSource::Screen),
                occupant_exited: true,
            },
            active,
        },
    })
}

#[test]
fn an_inactive_status_decodes_as_its_occupants_retirement() {
    let retirement = frame(false, 9, 4);
    let decoded =
        decode_upstream(&encode_upstream(&retirement).unwrap()).expect("a retirement is admitted");
    assert_eq!(decoded, retirement);
    let CoordWorkerUpstream::AgentStatus(decoded) = decoded else {
        panic!("the frame stays an agent status");
    };
    assert!(!decoded.status.active);
}

#[test]
fn a_retirement_is_still_bounded_by_the_common_checks() {
    let refused = decode_upstream(&encode_upstream(&frame(false, 3, 4)).unwrap())
        .expect_err("a completed revision past the revision is refused");
    assert!(
        refused.to_string().contains("completed_revision"),
        "{refused}"
    );
}

#[test]
fn an_active_status_still_decodes_as_a_retained_row() {
    let active = frame(true, 9, 4);
    assert_eq!(
        decode_upstream(&encode_upstream(&active).unwrap()).unwrap(),
        active
    );
}
