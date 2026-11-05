//! Actor-owned sequence allocation and bounded unacknowledged ACP output.
//! A run id survives child-process restarts; link snapshots resend its frames.
//! Dropping old output is explicit in the projected conversation transcript.

use crate::uplink::Uplink;
use roost_proto::WOmpEvents;
use roost_protocol::wire::{agent_chat::OmpWorkerEvent, coord_worker::CoordWorkerUpstream};
use std::collections::VecDeque;

const RETAINED_LIMIT: usize = 8 * 1024 * 1024;
const BATCH_LIMIT: usize = 64 * 1024;

pub(super) struct Emitter {
    pub conversation_id: String,
    pub run_id: String,
    uplink: Uplink,
    next_seq: u64,
    pending: Vec<String>,
    pending_bytes: usize,
    retained: VecDeque<(WOmpEvents, usize)>,
    retained_bytes: usize,
    loss_reported: bool,
}
impl Emitter {
    pub fn new(conversation_id: String, run_id: String, uplink: Uplink) -> Self {
        Self {
            conversation_id,
            run_id,
            uplink,
            next_seq: 1,
            pending: vec![],
            pending_bytes: 0,
            retained: VecDeque::new(),
            retained_bytes: 0,
            loss_reported: false,
        }
    }
    pub fn last_sequence(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }
    /// One private effect request. It never enters the durable event run.
    pub fn host_service(&self, request: roost_proto::WOmpHostServiceRequest) -> bool {
        self.uplink
            .send(CoordWorkerUpstream::OmpHostServiceRequest(request))
    }
    /// One private action receipt, fenced by the browser on canonical chat sequence.
    pub fn receipt(&self, receipt: roost_proto::WOmpUiActionReceipt) -> bool {
        self.uplink
            .send(CoordWorkerUpstream::OmpUiActionReceipt(receipt))
    }
    pub fn emit(&mut self, event: OmpWorkerEvent) {
        match serde_json::to_string(&event) {
            Ok(json) => {
                self.pending_bytes += json.len();
                self.pending.push(json);
                self.next_seq += 1;
                if self.pending_bytes >= BATCH_LIMIT {
                    self.flush();
                }
            }
            Err(error) => {
                tracing::error!(%error,conversation_id=%self.conversation_id,"OMP event serialization failed")
            }
        }
    }
    pub fn notice(&mut self, level: &str, title: &str, body: &str) {
        self.emit(OmpWorkerEvent::Notice {
            level: level.into(),
            title: title.into(),
            body: body.into(),
        });
    }
    pub fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending_bytes);
        let frame = WOmpEvents {
            conversation_id: self.conversation_id.clone(),
            run_id: self.run_id.clone(),
            first_seq: self.next_seq - self.pending.len() as u64,
            events_json: std::mem::take(&mut self.pending),
            ..Default::default()
        };
        self.uplink
            .send(CoordWorkerUpstream::OmpEvents(frame.clone()));
        self.retained.push_back((frame, bytes));
        self.retained_bytes += bytes;
        let mut lost = false;
        while self.retained_bytes > RETAINED_LIMIT {
            if let Some((_, bytes)) = self.retained.pop_front() {
                self.retained_bytes -= bytes;
                lost = true;
            } else {
                break;
            }
        }
        if lost && !self.loss_reported {
            self.loss_reported = true;
            self.notice(
                "warning",
                "Some OMP output was lost",
                "The coordinator was unreachable for too long.",
            );
        }
    }
    pub fn ack(&mut self, run_id: &str, seq: u64) {
        if run_id != self.run_id {
            return;
        }
        while self.retained.front().is_some_and(|(frame, _)| {
            frame
                .first_seq
                .saturating_add(frame.events_json.len() as u64)
                .saturating_sub(1)
                <= seq
        }) {
            if let Some((_, bytes)) = self.retained.pop_front() {
                self.retained_bytes -= bytes;
            }
        }
        if self.retained.is_empty() {
            self.loss_reported = false;
        }
    }
    pub fn resend(&mut self) {
        for (frame, _) in &self.retained {
            self.uplink
                .send(CoordWorkerUpstream::OmpEvents(frame.clone()));
        }
    }
}
