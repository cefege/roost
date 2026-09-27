//! The worker registry's two frames: registration, heartbeat and removal on
//! one stream, and the live routable set on the other.
//!
//! Ported from `apps/coord/src/sync/sync-feed-frames.ts:245-291` and
//! `sync-feed.ts:265-271`. They are a file of their own because they are the
//! only frames whose payload is a STORED machine record: a worker row read here
//! and the same row read by `workers::projection` must not disagree, or a
//! browser's fleet view contradicts itself one heartbeat later.
//!
//! `workers::projection::worker_row_to_proto` is the row-to-message direction
//! and reads columns a wire value does not have; the mapping below is the
//! wire-value-to-message direction, and it exists because the presence bus
//! already carries a decoded `Worker`. They meet on the same message, not on
//! the same function.

use std::collections::BTreeSet;

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::worker_presence_proto::Kind as PresenceKind;
use roost_proto::buffa::MessageField;
use roost_proto::{
    FirehoseFrame, HostMetrics as PbHostMetrics, Worker as PbWorker, WorkerHeartbeat,
    WorkerPresenceProto, WorkerRoutableFrame,
};
use roost_protocol::proto_adapters::{
    host_identity_to_proto, keeper_runtime_observation_to_proto,
    terminal_core_capacity_report_to_proto,
};
use roost_protocol::wire::{HostMetrics, Worker as WireWorker, WorkerFp, WorkerPresenceEvent};

use crate::events::bus_messages::WorkerRoutableSet;
use crate::sync_ws::feed::{FeedFrame, FeedRefusal, as_u64};

/// One worker presence event as its frame.
///
/// The three arms carry deliberately different amounts: a registration carries
/// the whole machine record because that is the only time it is sent, a
/// heartbeat carries freshness and a sample, and a removal carries nothing but
/// the fingerprint.
pub fn worker_presence_frame(event: &WorkerPresenceEvent) -> Result<FeedFrame, FeedRefusal> {
    let kind = match event {
        WorkerPresenceEvent::Registered { worker } => {
            PresenceKind::Registered(Box::new(worker_to_proto(worker)?))
        }
        WorkerPresenceEvent::Heartbeat {
            fp,
            last_seen_ms,
            host_metrics,
            terminal_core_capacity,
        } => PresenceKind::Heartbeat(Box::new(WorkerHeartbeat {
            worker_fp: fp.as_str().to_owned(),
            last_seen_ms: as_u64(*last_seen_ms),
            host_metrics: message_field(host_metrics.as_ref().map(metrics_to_proto)),
            terminal_core_capacity: message_field(
                terminal_core_capacity
                    .as_ref()
                    .map(terminal_core_capacity_report_to_proto)
                    .transpose()?,
            ),
            ..WorkerHeartbeat::default()
        })),
        WorkerPresenceEvent::Removed { fp } => PresenceKind::RemovedFp(fp.as_str().to_owned()),
    };
    Ok(FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::WorkerPresence(Box::new(WorkerPresenceProto {
            kind: Some(kind),
            ..WorkerPresenceProto::default()
        }))),
        ..FirehoseFrame::default()
    }))
}

/// The live routable set, narrowed to the machines this socket may see.
///
/// The narrowing is v2's (`sync-feed.ts:268`): a worker-owned socket is shown
/// its own reachability and nothing else, and the set is published in FULL on
/// every connect and disconnect precisely so a browser replaces its set
/// wholesale. The empty `snapshot_id` marks that live full-set replacement, as
/// opposed to a chunked retained seed.
pub fn worker_routable_frame(
    routable: &WorkerRoutableSet,
    visible: &BTreeSet<WorkerFp>,
) -> FeedFrame {
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::WorkerRoutable(Box::new(WorkerRoutableFrame {
            fps: routable
                .fps
                .iter()
                .filter(|fp| visible.contains(*fp))
                .map(|fp| fp.as_str().to_owned())
                .collect(),
            ..WorkerRoutableFrame::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// A decoded worker record as the message a browser renders.
fn worker_to_proto(worker: &WireWorker) -> Result<PbWorker, FeedRefusal> {
    let keeper_runtime = worker
        .keeper_runtime
        .as_ref()
        .map(keeper_runtime_observation_to_proto)
        .transpose()?;
    let terminal_core_capacity = worker
        .terminal_core_capacity
        .as_ref()
        .map(terminal_core_capacity_report_to_proto)
        .transpose()?;
    Ok(PbWorker {
        fp: worker.fp.as_str().to_owned(),
        label: worker.label.clone(),
        os: worker.os.as_str().to_owned(),
        host_identity: message_field(
            worker
                .host_identity
                .as_ref()
                .map(|identity| host_identity_to_proto(Some(identity))),
        ),
        git_sha: worker.git_sha.clone(),
        host_metrics: message_field(worker.host_metrics.as_ref().map(metrics_to_proto)),
        registered_at_ms: as_u64(worker.registered_at_ms),
        last_seen_ms: as_u64(worker.last_seen_ms),
        reachable_addr: worker.reachable_addr.clone(),
        keeper_runtime: message_field(keeper_runtime),
        terminal_core_capacity: message_field(terminal_core_capacity),
        ..PbWorker::default()
    })
}

fn metrics_to_proto(metrics: &HostMetrics) -> PbHostMetrics {
    PbHostMetrics {
        cpu_pct: metrics.cpu_pct,
        mem_used_bytes: as_u64(metrics.mem_used_bytes),
        mem_total_bytes: as_u64(metrics.mem_total_bytes),
        disk_used_bytes: as_u64(metrics.disk_used_bytes),
        disk_total_bytes: as_u64(metrics.disk_total_bytes),
        net_rx_bps: as_u64(metrics.net_rx_bps),
        net_tx_bps: as_u64(metrics.net_tx_bps),
        sampled_at_ms: as_u64(metrics.sampled_at_ms),
        ..PbHostMetrics::default()
    }
}

/// An absent optional message, or the one that is present.
///
/// A present-but-empty message and an absent one are different states on the
/// wire, which is why this is not `Option`: `host_identity_to_proto` answers a
/// missing identity with an EMPTY message, and only the caller knows whether
/// the field should be set at all.
fn message_field<T: Default>(value: Option<T>) -> MessageField<T, roost_proto::buffa::Inline<T>> {
    value.map_or_else(MessageField::none, MessageField::some)
}
