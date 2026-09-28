//! Ordered, coalescing outbox for volatile identified agent status. Ports v2
//! `apps/worker/src/transport/coord-link-agent-status.ts`: a bounded ordered
//! history of possibly-lost retirements plus the latest active occupant per
//! session, so reconnect repair covers every remotely possible prefix without
//! letting backpressure invert replacement edges. Owned by
//! [`super::LinkLoop`]: `link_drain` admits the uplink's statuses and writes
//! [`AgentStatusOutbox::next_bytes`] ahead of the control lane; the link's
//! detach calls [`AgentStatusOutbox::disconnect`].

use std::collections::{HashMap, VecDeque};

use roost_protocol::wire::agent_status::{AgentStatus, AgentStatusUpdate, agent_status_identity};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::{AgentStatusFrame, CoordWorkerUpstream};

use super::super::link_wire::LinkWire;
use super::{AdmitRefusal, LinkLoop};

/// v2 `AGENT_STATUS_REPAIR_CAP` (`WORKER_SNAPSHOT_MAX_SESSIONS`): how many
/// possibly-lost retirements a reconnect replays.
pub const AGENT_STATUS_REPAIR_CAP: usize = 1_024;

/// v2 `EncodedAgentStatus`: one status, its occupant, and its wire bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAgentStatus {
    pub session_id: SessionId,
    pub active: bool,
    /// `status_epoch:occupant_id`.
    pub occupant_key: String,
    pub bytes: Vec<u8>,
}

/// Per-session queues drained in first-queued order (v2's insertion-ordered
/// `Map`s).
#[derive(Debug, Default)]
struct SessionQueues {
    queues: Vec<(SessionId, VecDeque<EncodedAgentStatus>)>,
}

impl SessionQueues {
    fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    fn take(&mut self, session_id: &SessionId) -> Vec<EncodedAgentStatus> {
        match self.queues.iter().position(|(held, _)| held == session_id) {
            Some(index) => self.queues.remove(index).1.into(),
            None => Vec::new(),
        }
    }

    fn put(&mut self, session_id: SessionId, items: Vec<EncodedAgentStatus>) {
        if !items.is_empty() {
            self.queues.push((session_id, items.into()));
        }
    }

    fn push(&mut self, item: EncodedAgentStatus) {
        match self
            .queues
            .iter_mut()
            .find(|(held, _)| *held == item.session_id)
        {
            Some((_, queue)) => queue.push_back(item),
            None => self
                .queues
                .push((item.session_id.clone(), VecDeque::from([item]))),
        }
    }

    fn front(&self) -> Option<&EncodedAgentStatus> {
        self.queues.first().and_then(|(_, queue)| queue.front())
    }

    fn pop_front(&mut self) -> Option<EncodedAgentStatus> {
        let (_, queue) = self.queues.first_mut()?;
        let item = queue.pop_front();
        if queue.is_empty() {
            self.queues.remove(0);
        }
        item
    }
}

/// v2 `CoordLinkAgentStatusOutbox`.
#[derive(Debug, Default)]
pub struct AgentStatusOutbox {
    pending: SessionQueues,
    repair_replay: SessionQueues,
    possibly_sent: HashMap<SessionId, String>,
    retirement_repairs: VecDeque<EncodedAgentStatus>,
}

/// v2 `compactPending`: the one retirement the coordinator may already need
/// and the latest active occupant, in that order; nothing else survives.
fn compact_pending(
    possibly_sent: Option<&str>,
    pending: Vec<EncodedAgentStatus>,
) -> Vec<EncodedAgentStatus> {
    let mut current = possibly_sent.map(str::to_owned);
    let mut retirement: Option<usize> = None;
    for (index, item) in pending.iter().enumerate() {
        if item.active {
            current = Some(item.occupant_key.clone());
            continue;
        }
        if possibly_sent == Some(item.occupant_key.as_str()) {
            retirement = Some(index);
        }
        if current.as_deref() == Some(item.occupant_key.as_str()) {
            current = None;
        }
    }
    let latest_active = current.as_ref().and_then(|current| {
        pending
            .iter()
            .rposition(|item| item.active && item.occupant_key == *current)
    });
    let keep_retirement = retirement.filter(|_| current.as_deref() != possibly_sent);
    let mut pending: Vec<Option<EncodedAgentStatus>> = pending.into_iter().map(Some).collect();
    keep_retirement
        .into_iter()
        .chain(latest_active)
        .filter_map(|index| pending[index].take())
        .collect()
}

impl AgentStatusOutbox {
    /// v2 `queue`: append, then keep only what the coordinator can still need.
    pub fn queue(&mut self, item: EncodedAgentStatus) {
        let session_id = item.session_id.clone();
        let mut pending = self.pending.take(&session_id);
        pending.push(item);
        let possibly_sent = self.possibly_sent.get(&session_id).map(String::as_str);
        let repaired = compact_pending(possibly_sent, pending);
        tracing::trace!(session = %session_id, kept = repaired.len(), "an agent status was queued for the link");
        self.pending.put(session_id, repaired);
    }

    /// The next status to write: reconnect repairs first, then pending.
    pub fn next_bytes(&self) -> Option<&[u8]> {
        self.repair_replay
            .front()
            .or_else(|| self.pending.front())
            .map(|item| item.bytes.as_slice())
    }

    /// The status [`AgentStatusOutbox::next_bytes`] named reached the socket.
    pub fn commit_written(&mut self) {
        let written = match self.repair_replay.pop_front() {
            Some(item) => item,
            None => match self.pending.pop_front() {
                Some(item) => item,
                None => return,
            },
        };
        self.note_written(written);
    }

    /// v2 `noteWritten`.
    fn note_written(&mut self, item: EncodedAgentStatus) {
        if item.active {
            self.possibly_sent
                .insert(item.session_id.clone(), item.occupant_key.clone());
        } else if self.possibly_sent.get(&item.session_id) == Some(&item.occupant_key) {
            self.remember_retirement(item);
        }
    }

    /// v2 `rememberRetirement`: bounded, latest per occupant, oldest evicted.
    fn remember_retirement(&mut self, item: EncodedAgentStatus) {
        if let Some(existing) = self
            .retirement_repairs
            .iter_mut()
            .find(|retirement| retirement.occupant_key == item.occupant_key)
        {
            *existing = item;
            return;
        }
        self.retirement_repairs.push_back(item);
        if self.retirement_repairs.len() <= AGENT_STATUS_REPAIR_CAP {
            return;
        }
        if let Some(evicted) = self.retirement_repairs.pop_front()
            && self.possibly_sent.get(&evicted.session_id) == Some(&evicted.occupant_key)
        {
            self.possibly_sent.remove(&evicted.session_id);
        }
    }

    /// v2 `disconnect`: every retirement the old socket may have lost is
    /// replayed, in order, ahead of anything pending on the next socket.
    pub fn disconnect(&mut self) {
        self.repair_replay = SessionQueues::default();
        for retirement in &self.retirement_repairs {
            self.repair_replay.push(retirement.clone());
        }
        tracing::debug!(
            replay = self.retirement_repairs.len(),
            "agent-status retirements were armed for replay on the next link"
        );
    }

    pub fn has_pending(&self) -> bool {
        !self.repair_replay.is_empty() || !self.pending.is_empty()
    }
}

impl EncodedAgentStatus {
    /// v2 `encodeStatus`: an UNIDENTIFIED status is refused, not queued — no
    /// reader could place it.
    pub fn encode(status: &AgentStatusUpdate, wire: &dyn LinkWire) -> Result<Self, AdmitRefusal> {
        let common = &status.common;
        let Some(identity) = agent_status_identity(common) else {
            tracing::warn!(
                session = %common.session_id,
                agent_id = common.agent_id.as_str(),
                revision = common.revision,
                "unidentified_agent_status_dropped: an agent status with no occupant identity was refused"
            );
            return Err(AdmitRefusal::UnidentifiedAgentStatus);
        };
        let frame = CoordWorkerUpstream::AgentStatus(AgentStatusFrame {
            status: AgentStatus {
                common: common.clone(),
                active: status.active,
            },
        });
        let bytes = wire
            .encode_upstream(&frame)
            .map_err(|error| AdmitRefusal::Unencodable {
                label: "agent-status".to_owned(),
                reason: error.to_string(),
            })?;
        Ok(Self {
            session_id: common.session_id.clone(),
            active: status.active,
            occupant_key: format!(
                "{}:{}",
                identity.status_epoch.as_str(),
                identity.occupant_id.as_str()
            ),
            bytes,
        })
    }
}

impl LinkLoop {
    /// v2 `CoordLinkAgentStatusOutbox.send`, reached from the uplink.
    pub fn send_agent_status(&mut self, status: &AgentStatusUpdate) -> Result<(), AdmitRefusal> {
        let item = EncodedAgentStatus::encode(status, &*self.wire)?;
        self.agent_statuses.queue(item);
        self.wake();
        Ok(())
    }
}
