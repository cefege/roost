//! RPI worker-link fixtures exercise each allocated protobuf arm.
//! Shared by field-number and byte-roundtrip tests through the parent module.
//! Persistent sequence counters deliberately exceed signed 64-bit limits.

use roost_proto as proto;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};

pub fn upstream_arms() -> Vec<(&'static str, CoordWorkerUpstream)> {
    vec![
        (
            "rpi-command-ack",
            CoordWorkerUpstream::RpiCommandAck(proto::WRpiCommandAck {
                conversation_id: "rpi-1".into(),
                request_id: "submit-1".into(),
                command_seq: u64::MAX,
                worker_epoch: "epoch-1".into(),
                status: proto::RpiCommandAckStatus::Settled.into(),
                result_json: r#"{"kind":"submitted"}"#.into(),
                ..Default::default()
            }),
        ),
        (
            "rpi-events",
            CoordWorkerUpstream::RpiEvents(proto::WRpiEvents {
                conversation_id: "rpi-1".into(),
                worker_epoch: "epoch-1".into(),
                actor_id: "actor-1".into(),
                first_seq: u64::MAX,
                events: vec![proto::RpiEventEnvelope {
                    command_seq: u64::MAX,
                    request_id: "submit-1".into(),
                    events_json: "[]".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ),
        (
            "rpi-models-list-result",
            CoordWorkerUpstream::RpiModelsListResult(proto::WRpiModelsListResult {
                request_id: "catalog-1".into(),
                worker_epoch: "epoch-1".into(),
                models: vec![proto::RpiModel {
                    provider: "local".into(),
                    model_id: "coder".into(),
                    name: "Local coder".into(),
                    supports_images: true,
                    thinking_levels: vec!["off".into(), "high".into()],
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ),
    ]
}

pub fn downstream_arms() -> Vec<(&'static str, CoordWorkerDownstream)> {
    vec![
        (
            "rpi-command",
            CoordWorkerDownstream::RpiCommand(proto::DRpiCommand {
                conversation_id: "rpi-1".into(),
                request_id: "submit-1".into(),
                command_seq: u64::MAX,
                expected_worker_epoch: "epoch-1".into(),
                kind: proto::RpiCommandKind::Submit.into(),
                payload_json: r#"{"kind":"submit","text":"Read these","attachments":[{"filename":"second.png","mime_type":"image/png","data":[2],"path":"/work/second.png"},{"filename":"first.txt","mime_type":"text/plain","data":[],"path":"/work/first.txt"}],"when_busy":"follow_up"}"#.into(),
                ..Default::default()
            }),
        ),
        (
            "rpi-events-ack",
            CoordWorkerDownstream::RpiEventsAck(proto::DRpiEventsAck {
                conversation_id: "rpi-1".into(),
                worker_epoch: "epoch-1".into(),
                actor_id: "actor-1".into(),
                committed_seq: u64::MAX,
                ..Default::default()
            }),
        ),
        (
            "rpi-models-list",
            CoordWorkerDownstream::RpiModelsList(proto::DRpiModelsList {
                request_id: "catalog-1".into(),
                expected_worker_epoch: "epoch-1".into(),
                ..Default::default()
            }),
        ),
    ]
}
