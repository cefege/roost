// Frame builders and a barrier harness for the announced-channel tests: one
// barrier on channel 7 over a fresh socket budget, with its drops recorded.
// Shared by `announced_barrier.rs` and `announced_retention.rs`; depends only
// on the barrier's public API and the worker-link wire types.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use roost_coord::worker_link::announced_barrier::AnnouncedChannelBarrier;
use roost_coord::worker_link::announced_types::{ChannelDrop, EnqueueOutcome};
use roost_coord::worker_link::retained_budget::RetainedWorkBudget;
use roost_proto::buffa::MessageField;
use roost_proto::{PbCellGridChunk, PbCellGridFrame, WCellGrid, WCellGridChunk};
use roost_protocol::wire::ChannelId;
use roost_protocol::wire::coord_worker::{Binary, CoordWorkerUpstream, TerminalMetadata};
use tokio::time::Instant;

pub const SESSION: &str = "00000000-0000-4000-8000-000000000717";
pub const CHANNEL: u32 = 7;

/// A barrier on a fresh socket budget, recording every drop it reports.
pub struct Barrier {
    pub barrier: AnnouncedChannelBarrier,
    pub budget: RetainedWorkBudget,
    drops: Arc<Mutex<Vec<ChannelDrop>>>,
}

impl Barrier {
    pub fn new() -> Self {
        Self::with_budget(RetainedWorkBudget::new())
    }

    pub fn with_budget(budget: RetainedWorkBudget) -> Self {
        let drops = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&drops);
        let barrier = AnnouncedChannelBarrier::new(Box::new(move |drop: &ChannelDrop| {
            recorder.lock().unwrap().push(drop.clone());
        }));
        Self {
            barrier,
            budget,
            drops,
        }
    }

    pub fn announce(&mut self, session_id: &str) {
        self.barrier
            .announce(CHANNEL, session_id, now(), &mut self.budget);
    }

    pub fn enqueue(&mut self, frame: CoordWorkerUpstream, encoded_bytes: u64) -> EnqueueOutcome {
        self.barrier
            .enqueue(CHANNEL, frame, encoded_bytes, now(), &mut self.budget)
    }

    /// Retain a metadata fact for the unannounced channel.
    pub fn retain_early(&mut self, frame: CoordWorkerUpstream, encoded_bytes: u64) -> bool {
        let CoordWorkerUpstream::TerminalMetadata(metadata) = frame else {
            panic!("only metadata is retained before an announcement");
        };
        self.barrier.retain_unannounced_metadata(
            CHANNEL,
            &metadata,
            encoded_bytes,
            now(),
            &mut self.budget,
        )
    }

    pub fn commit(
        &mut self,
        session_id: &str,
        mapping_matches: bool,
        deliver: &mut dyn FnMut(CoordWorkerUpstream),
    ) -> bool {
        let mut infallible = |frame| {
            deliver(frame);
            Ok::<(), ()>(())
        };
        self.barrier
            .commit(
                CHANNEL,
                session_id,
                mapping_matches,
                &mut self.budget,
                &mut infallible,
            )
            .unwrap()
    }

    pub fn drops(&self) -> Vec<ChannelDrop> {
        self.drops.lock().unwrap().clone()
    }
}

/// Commit and collect the delivered frames' labels in delivery order.
pub fn commit_labels(b: &mut Barrier, session_id: &str, mapping: bool) -> (bool, Vec<String>) {
    let mut delivered = Vec::new();
    let committed = b.commit(session_id, mapping, &mut |frame| {
        delivered.push(label(&frame))
    });
    (committed, delivered)
}

pub fn now() -> Instant {
    Instant::now()
}

fn grid_frame(seq: u64, full: bool) -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: SESSION.to_owned(),
        seq,
        full,
        cols: 80,
        rows: 24,
        grid_epoch: "announced".to_owned(),
        ..Default::default()
    }
}

pub fn cell(seq: u64, full: bool) -> CoordWorkerUpstream {
    CoordWorkerUpstream::CellGrid(WCellGrid {
        channel_id: CHANNEL,
        frame: MessageField::some(grid_frame(seq, full)),
        ..Default::default()
    })
}

pub fn chunk(seq: u64) -> CoordWorkerUpstream {
    CoordWorkerUpstream::CellGridChunk(WCellGridChunk {
        channel_id: CHANNEL,
        chunk: MessageField::some(PbCellGridChunk {
            snapshot_id: SESSION.to_owned(),
            chunk_index: 0,
            chunk_count: 1,
            part: MessageField::some(grid_frame(seq, true)),
            ..Default::default()
        }),
        ..Default::default()
    })
}

pub fn binary(seq: u64, text: &str) -> CoordWorkerUpstream {
    CoordWorkerUpstream::Binary(Binary {
        channel_id: ChannelId::try_from(i64::from(CHANNEL)).unwrap(),
        direction: 1,
        data: text.as_bytes().to_vec(),
        seq,
    })
}

pub fn metadata(
    title: &str,
    title_changed: bool,
    activity_changed: bool,
    activity_ts_ms: u64,
) -> CoordWorkerUpstream {
    CoordWorkerUpstream::TerminalMetadata(TerminalMetadata {
        channel_id: ChannelId::try_from(i64::from(CHANNEL)).unwrap(),
        title_changed,
        title: title.to_owned(),
        activity_changed,
        activity_ts_ms,
        clipboard_changed: false,
        clipboard: String::new(),
        command_finished: false,
        command_exit_code: None,
        command_duration_ms: 0,
        bell: false,
    })
}

pub fn label(frame: &CoordWorkerUpstream) -> String {
    match frame {
        CoordWorkerUpstream::CellGrid(grid) => {
            let cell = grid.frame.as_option().unwrap();
            let kind = if cell.full { "full" } else { "delta" };
            format!("cell:{}:{kind}", cell.seq)
        }
        CoordWorkerUpstream::CellGridChunk(chunk) => {
            let part = chunk.chunk.as_option().unwrap().part.as_option().unwrap();
            format!("chunk:{}", part.seq)
        }
        CoordWorkerUpstream::Binary(binary) => {
            format!("binary:{}", String::from_utf8_lossy(&binary.data))
        }
        CoordWorkerUpstream::TerminalMetadata(metadata) => format!("metadata:{}", metadata.title),
        other => format!("other:{}", other.kind()),
    }
}
