//! Retained-byte quotas for attachment WebRTC packet queues and reassembly:
//! a control and a data lane per peer, and one worker-wide data ceiling across
//! every attachment peer. Terminal peers never share these counters, so
//! attachment pressure cannot retire or delay a terminal carrier. Built by
//! `peer_owner`, consumed by `peer_packet_port`. Ports
//! `apps/worker/src/attachments/attachment-peer-packet-budget.ts`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::attachment_transfer::{
    AttachmentTransferPacketDirection as Direction, AttachmentTransferPacketQuota,
    PEER_CONTROL_QUEUE_MAX_BYTES, PEER_DATA_QUEUE_MAX_BYTES, PEER_WORKER_DATA_QUEUE_MAX_BYTES,
};

#[derive(Debug, Default)]
struct WorkerData {
    retained: usize,
    disposed: bool,
}

/// The process-owned budget. Clone shares one worker-wide data ceiling.
#[derive(Debug, Clone, Default)]
pub struct AttachmentPeerPacketBudget {
    worker_data: Arc<Mutex<WorkerData>>,
}

impl AttachmentPeerPacketBudget {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_peer_budget(&self) -> AttachmentPeerPacketPeerBudget {
        AttachmentPeerPacketPeerBudget {
            control: AttachmentPeerLaneQuota::new(PEER_CONTROL_QUEUE_MAX_BYTES, None),
            data: AttachmentPeerLaneQuota::new(PEER_DATA_QUEUE_MAX_BYTES, Some(self.clone())),
        }
    }

    pub fn dispose(&self) {
        let mut worker_data = lock(&self.worker_data);
        worker_data.disposed = true;
        worker_data.retained = 0;
    }

    /// The worker-wide data bytes every attachment peer currently retains.
    pub fn retained_data_bytes(&self) -> usize {
        lock(&self.worker_data).retained
    }

    fn reserve_worker_data(&self, bytes: usize) -> bool {
        let mut worker_data = lock(&self.worker_data);
        if worker_data.disposed
            || bytes > PEER_WORKER_DATA_QUEUE_MAX_BYTES.saturating_sub(worker_data.retained)
        {
            return false;
        }
        worker_data.retained += bytes;
        true
    }

    fn release_worker_data(&self, bytes: usize) {
        let mut worker_data = lock(&self.worker_data);
        worker_data.retained = worker_data.retained.saturating_sub(bytes);
    }
}

/// One peer's two lane quotas.
#[derive(Debug, Clone)]
pub struct AttachmentPeerPacketPeerBudget {
    control: AttachmentPeerLaneQuota,
    data: AttachmentPeerLaneQuota,
}

impl AttachmentPeerPacketPeerBudget {
    pub fn control(&self) -> AttachmentPeerLaneQuota {
        self.control.clone()
    }

    pub fn data(&self) -> AttachmentPeerLaneQuota {
        self.data.clone()
    }

    /// Returns every byte this peer still holds to the worker-wide ceiling.
    pub fn dispose(&self) {
        self.control.dispose();
        self.data.dispose();
    }
}

#[derive(Debug, Default)]
struct LaneBytes {
    incoming: usize,
    outgoing: usize,
    disposed: bool,
}

/// One lane's quota; the queue and assembler of that lane share one clone.
#[derive(Debug, Clone)]
pub struct AttachmentPeerLaneQuota {
    max_bytes: usize,
    shared: Option<AttachmentPeerPacketBudget>,
    bytes: Arc<Mutex<LaneBytes>>,
}

impl AttachmentPeerLaneQuota {
    fn new(max_bytes: usize, shared: Option<AttachmentPeerPacketBudget>) -> Self {
        Self {
            max_bytes,
            shared,
            bytes: Arc::default(),
        }
    }

    fn dispose(&self) {
        let released = {
            let mut lane = lock(&self.bytes);
            if lane.disposed {
                return;
            }
            lane.disposed = true;
            let released = lane.incoming + lane.outgoing;
            lane.incoming = 0;
            lane.outgoing = 0;
            released
        };
        if let Some(shared) = &self.shared {
            shared.release_worker_data(released);
        }
    }
}

impl AttachmentTransferPacketQuota for AttachmentPeerLaneQuota {
    fn reserve(&mut self, direction: Direction, bytes: usize) -> bool {
        let mut lane = lock(&self.bytes);
        let retained = match direction {
            Direction::Incoming => lane.incoming,
            Direction::Outgoing => lane.outgoing,
        };
        if lane.disposed || bytes > self.max_bytes.saturating_sub(retained) {
            return false;
        }
        if let Some(shared) = &self.shared
            && !shared.reserve_worker_data(bytes)
        {
            return false;
        }
        match direction {
            Direction::Incoming => lane.incoming += bytes,
            Direction::Outgoing => lane.outgoing += bytes,
        }
        true
    }

    fn release(&mut self, direction: Direction, bytes: usize) {
        {
            let mut lane = lock(&self.bytes);
            match direction {
                Direction::Incoming => lane.incoming = lane.incoming.saturating_sub(bytes),
                Direction::Outgoing => lane.outgoing = lane.outgoing.saturating_sub(bytes),
            }
        }
        if let Some(shared) = &self.shared {
            shared.release_worker_data(bytes);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_lane_refuses_past_its_own_cap_and_returns_its_bytes_on_dispose() {
        let budget = AttachmentPeerPacketBudget::new();
        let peer = budget.create_peer_budget();
        let mut data = peer.data();
        assert!(data.reserve(Direction::Incoming, PEER_DATA_QUEUE_MAX_BYTES));
        assert!(!data.reserve(Direction::Incoming, 1));
        assert!(data.reserve(Direction::Outgoing, 1));
        assert_eq!(budget.retained_data_bytes(), PEER_DATA_QUEUE_MAX_BYTES + 1);
        peer.dispose();
        assert_eq!(budget.retained_data_bytes(), 0);
        assert!(!data.reserve(Direction::Outgoing, 1));
    }

    #[test]
    fn the_worker_wide_data_ceiling_refuses_a_peer_that_its_own_lane_would_admit() {
        let budget = AttachmentPeerPacketBudget::new();
        let peers: Vec<_> = (0..PEER_WORKER_DATA_QUEUE_MAX_BYTES / PEER_DATA_QUEUE_MAX_BYTES)
            .map(|_| budget.create_peer_budget())
            .collect();
        for peer in &peers {
            assert!(
                peer.data()
                    .reserve(Direction::Incoming, PEER_DATA_QUEUE_MAX_BYTES)
            );
        }
        let extra = budget.create_peer_budget();
        assert!(!extra.data().reserve(Direction::Incoming, 1));
        // Control never draws on the worker-wide data ceiling.
        assert!(
            extra
                .control()
                .reserve(Direction::Incoming, PEER_CONTROL_QUEUE_MAX_BYTES)
        );
        peers[0].data().release(Direction::Incoming, 1);
        assert!(extra.data().reserve(Direction::Incoming, 1));
    }
}
