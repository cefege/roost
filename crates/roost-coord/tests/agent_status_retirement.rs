//! An agent's retirement, as a worker actually sends it: an inactive status
//! encoded on the worker link, decoded by `decode_upstream`, dispatched by the
//! coordinator's live-frame arm, and folded by the status hub.
//!
//! Ports the retirement cases of `apps/coord/tests/agents/agent-status-hub.test.ts`
//! ("publishes inactive deletion and keeps its revision floor") and
//! `agent-status-identity.test.ts` ("accepts lower revisions and lexically lower
//! new identities while fencing retirees"), whose v2 source is
//! `agent-status-hub.ts:136-140` (an inactive update DELETES the row) and
//! `agent-status-order.ts:44-55` (the admission order a retirement obeys). The
//! hub-level cases live in `agent_status_ordering.rs`; these drive the same
//! fold through the wire a Rust worker writes, because a retirement the link
//! refused to decode would strand the row with every hub test still green.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod frame_dispatch_support;
mod workers_support;

use std::sync::{Arc, Mutex};

use frame_dispatch_support::{LinkFixture, SESSION_ID, WORKER_FP, live_frame, session_id, worker};
use roost_coord::events::bus::Subscription;
use roost_coord::worker_link::dispatch::{DispatchOutcome, FrameDispatch};
use roost_protocol::proto_adapters::coord_worker_proto::{decode_upstream, encode_upstream};
use roost_protocol::wire::agent_status::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusSource,
    StatusEpoch,
};
use roost_protocol::wire::coord_worker::{AgentStatusFrame, CoordWorkerUpstream};
use roost_protocol::wire::{AgentStatusUpdate, ChannelId};

const EPOCH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT_A: &str = "11111111-aaaa-4aaa-8aaa-111111111111";
const OCCUPANT_B: &str = "22222222-aaaa-4aaa-8aaa-222222222222";

/// A coordinator whose route cache names this worker as the session's owner,
/// past the snapshot barrier, with a sink on the agent status bus.
struct Retirement {
    fixture: LinkFixture,
    published: Arc<Mutex<Vec<(i64, bool)>>>,
    _subscription: Subscription<AgentStatusUpdate>,
}

impl Retirement {
    async fn new(label: &str) -> Self {
        let fixture = LinkFixture::new(label).await;
        fixture.mark_ready();
        fixture.services.byte_hub.prime_channel_map(&[(
            session_id(),
            worker(WORKER_FP),
            ChannelId::try_from(7_i64).expect("a channel"),
        )]);
        let published = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&published);
        let subscription =
            fixture
                .services
                .buses
                .agent_status_bus
                .subscribe(move |update: &AgentStatusUpdate| {
                    sink.lock()
                        .expect("the publication sink")
                        .push((update.common.revision, update.active));
                });
        Self {
            fixture,
            published,
            _subscription: subscription,
        }
    }

    /// One status frame, through the worker's encoder, the coordinator's
    /// decoder, and the live-frame dispatcher.
    fn report(&self, occupant: &str, revision: i64, active: bool) {
        let frame = CoordWorkerUpstream::AgentStatus(AgentStatusFrame {
            status: AgentStatus {
                common: AgentStatusFields {
                    session_id: session_id(),
                    agent_id: AgentId::try_from("omp").expect("an agent id"),
                    state: AgentRuntimeState::Idle,
                    message: None,
                    revision,
                    completed_revision: 0,
                    updated_at: 1_800_000_000_000 + revision,
                    status_epoch: Some(StatusEpoch::try_from(EPOCH).expect("an epoch")),
                    occupant_id: Some(AgentOccupantId::try_from(occupant).expect("an occupant")),
                    source: Some(AgentStatusSource::Integration),
                    occupant_exited: !active,
                },
                active,
            },
        });
        let decoded = decode_upstream(&encode_upstream(&frame).expect("the frame encodes"))
            .expect("the worker's frame decodes on the coordinator");
        let outcome = self
            .fixture
            .dispatcher()
            .handle_now(WORKER_FP, live_frame(0, decoded));
        assert_eq!(outcome, DispatchOutcome::Handled, "rev {revision}");
    }

    /// The retained rows, as `occupant:revision`.
    fn retained(&self) -> Vec<String> {
        self.fixture
            .services
            .agents
            .status
            .snapshot()
            .into_iter()
            .map(|row| {
                assert_eq!(row.common.session_id.as_str(), SESSION_ID);
                format!(
                    "{}:{}",
                    row.common
                        .occupant_id
                        .as_ref()
                        .map_or("-", |id| id.as_str()),
                    row.common.revision
                )
            })
            .collect()
    }

    /// Every `(revision, active)` the hub published, in order.
    fn published(&self) -> Vec<(i64, bool)> {
        self.published.lock().expect("the publication sink").clone()
    }
}

#[tokio::test]
async fn a_retirement_off_the_wire_deletes_the_row_and_keeps_its_revision_floor() {
    // v2 "publishes inactive deletion and keeps its revision floor".
    let link = Retirement::new("retire-delete").await;
    link.report(OCCUPANT_A, 1, true);
    assert_eq!(link.retained(), vec![format!("{OCCUPANT_A}:1")]);

    link.report(OCCUPANT_A, 2, false);
    assert!(
        link.retained().is_empty(),
        "an inactive status is the occupant's retirement: the row is deleted"
    );
    assert_eq!(
        link.published(),
        vec![(1, true), (2, false)],
        "the deletion itself is published, so every browser drops the row too"
    );

    // A retried publish of the retired occupant must not resurrect the row.
    link.report(OCCUPANT_A, 1, true);
    link.report(OCCUPANT_A, 3, true);
    assert!(
        link.retained().is_empty(),
        "a retired occupant stays retired"
    );
    assert_eq!(link.published(), vec![(1, true), (2, false)]);
}

#[tokio::test]
async fn an_out_of_order_retirement_is_ordered_as_v2_orders_it() {
    // v2 "accepts lower revisions ... while fencing retirees" and
    // `agent-status-order.ts:44-55`.
    let link = Retirement::new("retire-order").await;

    // A retirement with no row to delete is refused: `if (!previous) return
    // update.active`. The occupant is NOT fenced by it, so its first active
    // report, arriving late, is still admitted.
    link.report(OCCUPANT_A, 5, false);
    assert!(link.retained().is_empty());
    assert!(
        link.published().is_empty(),
        "a refused retirement publishes nothing"
    );
    link.report(OCCUPANT_A, 4, true);
    assert_eq!(link.retained(), vec![format!("{OCCUPANT_A}:4")]);

    // A retirement below the retained revision of the SAME occupant is late.
    link.report(OCCUPANT_A, 3, false);
    assert_eq!(link.retained(), vec![format!("{OCCUPANT_A}:4")]);

    // A retirement of an occupant that is not the retained one deletes nothing,
    // however high its revision.
    link.report(OCCUPANT_B, 999, false);
    assert_eq!(link.retained(), vec![format!("{OCCUPANT_A}:4")]);
    assert_eq!(link.published(), vec![(4, true)]);
}
