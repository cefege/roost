//! The live worker metrics path: its unsequenced control lane, the
//! subscription command (deduplicated, re-sent on every new socket), and the
//! latest-only fold that drops unsubscribed workers and prunes on a set change.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::ClientCore;
use roost_client_core::effect::{Effect, SyncCommand};
use roost_client_core::event::ClientEvent;
use roost_client_core::sync::decode::DecodeRefusal;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_protocol::proto_adapters::live_metrics_proto::worker_live_metrics_frame_to_proto;
use roost_protocol::wire::brand::WorkerFp;
use roost_protocol::wire::live_metrics::{LiveHostSnapshot, WorkerLiveMetricsFrame};

use roost_client_core::SyncFrame;
use sync_decode_support::{
    EPOCH, control, decoded, deliver, open_ready_link, ready_core, refused, stamped,
};

fn fingerprint(text: &str) -> WorkerFp {
    WorkerFp::try_from(text).expect("a valid worker fingerprint")
}

fn worker_a() -> WorkerFp {
    fingerprint(&"a".repeat(64))
}

fn worker_b() -> WorkerFp {
    fingerprint(&"b".repeat(64))
}

fn snapshot(sampled_at_ms: u64) -> LiveHostSnapshot {
    LiveHostSnapshot {
        cpu_pct: 12.5,
        mem_used_bytes: 1,
        mem_total_bytes: 2,
        disk_used_bytes: 1,
        disk_total_bytes: 2,
        cpu_count: 4,
        uptime_secs: 60,
        process_count: 0,
        processes: Vec::new(),
        sampled_at_ms,
    }
}

fn live_arm(worker_fp: WorkerFp, sampled_at_ms: u64) -> Frame {
    let frame = WorkerLiveMetricsFrame {
        worker_fp,
        snapshot: snapshot(sampled_at_ms),
    };
    Frame::WorkerLiveMetrics(Box::new(
        worker_live_metrics_frame_to_proto(&frame).expect("the snapshot encodes"),
    ))
}

fn subscription_sent(effects: &[Effect]) -> Option<Vec<WorkerFp>> {
    effects.iter().find_map(|effect| match effect {
        Effect::SendSync(SyncCommand::WorkerLiveSubscription { worker_fps }) => {
            Some(worker_fps.clone())
        }
        _ => None,
    })
}

#[test]
fn a_live_frame_decodes_as_an_unsequenced_control_frame() {
    let event = decoded(&control(live_arm(worker_a(), 7)), 1);
    assert!(matches!(
        event,
        ClientEvent::SyncFrameReceived {
            delivery_seq: 0,
            frame: SyncFrame::WorkerLiveMetrics { .. },
            ..
        }
    ));
}

#[test]
fn a_sequenced_live_frame_is_refused_as_control() {
    let refusal = refused(&stamped(
        roost_proto::SyncDomain::Terminal,
        5,
        0,
        live_arm(worker_a(), 7),
    ));
    assert!(
        matches!(
            refusal,
            DecodeRefusal::SequencedControl {
                arm: "worker_live_metrics",
                ..
            }
        ),
        "{refusal}"
    );
}

#[test]
fn subscribing_sends_the_deduplicated_set_on_an_open_socket() {
    let (mut core, _) = ready_core();
    let effects = core.handle(ClientEvent::WorkerLiveSubscribe {
        worker_fps: vec![worker_a(), worker_b(), worker_a()],
    });
    assert_eq!(
        subscription_sent(&effects),
        Some(vec![worker_a(), worker_b()])
    );
}

#[test]
fn an_empty_subscription_sends_an_empty_set_to_cancel() {
    let (mut core, _) = ready_core();
    core.handle(ClientEvent::WorkerLiveSubscribe {
        worker_fps: vec![worker_a()],
    });
    let effects = core.handle(ClientEvent::WorkerLiveSubscribe {
        worker_fps: Vec::new(),
    });
    assert_eq!(subscription_sent(&effects), Some(Vec::new()));
}

#[test]
fn reopening_the_socket_resends_the_whole_set() {
    let (mut core, previous_generation) = ready_core();
    core.handle(ClientEvent::WorkerLiveSubscribe {
        worker_fps: vec![worker_a(), worker_b()],
    });
    core.handle(ClientEvent::SyncLinkClosed {
        generation: previous_generation,
        close_code: Some(1006),
        close_reason: String::new(),
    });
    let generation = match core.handle(ClientEvent::DialRequested).as_slice() {
        [Effect::DialSync { generation, .. }] => *generation,
        other => panic!("expected exactly one dial, got {other:?}"),
    };
    let effects = core.handle(ClientEvent::SyncLinkOpened {
        generation,
        socket_id: "sock-reopened".to_owned(),
        process_epoch: EPOCH.to_owned(),
    });
    assert_eq!(
        subscription_sent(&effects),
        Some(vec![worker_a(), worker_b()])
    );
}

#[test]
fn a_subscribed_worker_snapshot_is_stored_and_an_unsubscribed_one_is_dropped() {
    let mut core = ClientCore::in_memory("tab-live");
    let generation = open_ready_link(&mut core);
    core.handle(ClientEvent::WorkerLiveSubscribe {
        worker_fps: vec![worker_a()],
    });
    deliver(&mut core, generation, &control(live_arm(worker_a(), 100)));
    deliver(&mut core, generation, &control(live_arm(worker_b(), 100)));
    let snapshots = core.store().worker_live.snapshots();
    assert_eq!(
        snapshots.keys().cloned().collect::<Vec<_>>(),
        vec![worker_a()]
    );
    assert_eq!(
        snapshots.get(&worker_a()).map(|s| s.sampled_at_ms),
        Some(100)
    );
}

#[test]
fn replacing_the_set_prunes_the_snapshots_of_dropped_workers() {
    let mut core = ClientCore::in_memory("tab-live");
    let generation = open_ready_link(&mut core);
    core.handle(ClientEvent::WorkerLiveSubscribe {
        worker_fps: vec![worker_a(), worker_b()],
    });
    deliver(&mut core, generation, &control(live_arm(worker_a(), 1)));
    deliver(&mut core, generation, &control(live_arm(worker_b(), 2)));
    core.handle(ClientEvent::WorkerLiveSubscribe {
        worker_fps: vec![worker_b()],
    });
    let snapshots = core.store().worker_live.snapshots();
    assert_eq!(
        snapshots.keys().cloned().collect::<Vec<_>>(),
        vec![worker_b()]
    );
}
