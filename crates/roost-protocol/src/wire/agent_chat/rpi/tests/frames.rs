//! RPI generated frames round-trip through the actual worker-link codec.
//! Persistent sequence values exercise unsigned boundaries without narrowing.
//! Parent fixtures retain ordered image/file attachment content.

use super::{command, submit};
use crate::proto_adapters::coord_worker_proto::{
    decode_downstream, decode_upstream, encode_downstream, encode_upstream,
};
use crate::wire::agent_chat::RpiCommandPayload;
use crate::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use roost_proto::{self as proto, RpiCommandKind};

#[test]
fn all_rpi_frames_preserve_unsigned_counters_and_attachment_order() {
    for sequence in [0, i64::MAX as u64 + 1, u64::MAX] {
        let downstream = [
            CoordWorkerDownstream::RpiCommand(command(&submit(), sequence)),
            CoordWorkerDownstream::RpiEventsAck(proto::DRpiEventsAck {
                conversation_id: "conversation".into(),
                worker_epoch: "epoch".into(),
                actor_id: "actor".into(),
                committed_seq: sequence,
                ..Default::default()
            }),
            CoordWorkerDownstream::RpiModelsList(proto::DRpiModelsList {
                request_id: "catalog".into(),
                expected_worker_epoch: "epoch".into(),
                ..Default::default()
            }),
        ];
        for frame in downstream {
            let decoded = decode_downstream(&encode_downstream(&frame).unwrap()).unwrap();
            assert_eq!(decoded, frame);
            let json = serde_json::to_value(&frame).unwrap();
            assert_eq!(CoordWorkerDownstream::parse(json).unwrap(), frame);
            if let CoordWorkerDownstream::RpiCommand(command) = decoded {
                assert_eq!(
                    RpiCommandPayload::parse(RpiCommandKind::Submit, &command.payload_json)
                        .unwrap(),
                    submit()
                );
            }
        }
        let upstream = [
            CoordWorkerUpstream::RpiCommandAck(proto::WRpiCommandAck {
                conversation_id: "conversation".into(),
                request_id: "request".into(),
                command_seq: sequence,
                worker_epoch: "epoch".into(),
                status: proto::RpiCommandAckStatus::Accepted.into(),
                ..Default::default()
            }),
            CoordWorkerUpstream::RpiEvents(proto::WRpiEvents {
                conversation_id: "conversation".into(),
                worker_epoch: "epoch".into(),
                actor_id: "actor".into(),
                first_seq: sequence,
                events: vec![proto::RpiEventEnvelope {
                    command_seq: sequence,
                    request_id: "request".into(),
                    events_json: "[]".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            CoordWorkerUpstream::RpiModelsListResult(proto::WRpiModelsListResult {
                request_id: "catalog".into(),
                worker_epoch: "epoch".into(),
                models: vec![proto::RpiModel {
                    provider: "local".into(),
                    model_id: "coder".into(),
                    name: "Coder".into(),
                    supports_images: true,
                    thinking_levels: vec!["off".into(), "high".into()],
                    ..Default::default()
                }],
                error_code: String::new(),
                error_message: String::new(),
                ..Default::default()
            }),
        ];
        for frame in upstream {
            assert_eq!(
                decode_upstream(&encode_upstream(&frame).unwrap()).unwrap(),
                frame
            );
            let json = serde_json::to_value(&frame).unwrap();
            assert_eq!(CoordWorkerUpstream::parse(json).unwrap(), frame);
        }
    }
}

#[test]
fn protobuf_command_admission_refuses_mismatch_and_unknown_enum() {
    let mut invalid = command(&submit(), 1);
    invalid.kind = RpiCommandKind::Close.into();
    assert!(encode_downstream(&CoordWorkerDownstream::RpiCommand(invalid.clone())).is_err());
    invalid.kind = 99.into();
    assert!(CoordWorkerDownstream::RpiCommand(invalid).check().is_err());
}

#[test]
fn command_json_keeps_frame_and_command_kinds_distinct() {
    let frame = CoordWorkerDownstream::RpiCommand(command(&submit(), 1));
    let mut encoded = serde_json::to_value(&frame).unwrap();
    assert_eq!(encoded["kind"], "rpi-command");
    assert!(encoded["command"].get("kind").is_some());
    encoded["extra"] = serde_json::json!(true);
    assert!(CoordWorkerDownstream::parse(encoded).is_err());
}
