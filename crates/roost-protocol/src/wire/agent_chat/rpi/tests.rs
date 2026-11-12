//! RPI payload admission, semantic identity and typed outcome behavior.
//! Exercises the real protobuf codec with persistent unsigned sequence bounds.
//! No provider, worker runtime or coordinator database is required.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use roost_proto as proto;
use serde_json::json;
mod frames;

fn submit() -> RpiCommandPayload {
    RpiCommandPayload::Submit {
        text: "inspect both".into(),
        attachments: vec![
            AgentChatAttachment {
                filename: "second.png".into(),
                mime_type: "image/png".into(),
                data: vec![255, 0, 128],
                path: "/work/second.png".into(),
            },
            AgentChatAttachment {
                filename: "first.txt".into(),
                mime_type: "text/plain".into(),
                data: Vec::new(),
                path: "/work/first.txt".into(),
            },
        ],
        when_busy: "follow_up".into(),
    }
}

fn command(payload: &RpiCommandPayload, sequence: u64) -> proto::DRpiCommand {
    proto::DRpiCommand {
        conversation_id: "conversation".into(),
        request_id: "request".into(),
        command_seq: sequence,
        expected_worker_epoch: "epoch".into(),
        kind: payload.kind().into(),
        payload_json: serde_json::to_string(payload).unwrap(),
        ..Default::default()
    }
}

#[test]
fn every_command_payload_has_one_strict_kind() {
    for payload in [
        RpiCommandPayload::Open {
            cwd: "/work".into(),
            model_provider: "local".into(),
            model_id: "coder".into(),
            thinking_level: Some("high".into()),
            resume_pending: false,
        },
        submit(),
        RpiCommandPayload::Abort { background: None },
        RpiCommandPayload::Abort {
            background: Some(false),
        },
        RpiCommandPayload::Withdraw {
            submission_id: "submit".into(),
        },
        RpiCommandPayload::Configure {
            model_provider: Some("local".into()),
            model_id: Some("coder".into()),
            thinking_level: Some("off".into()),
            title: Some("Title".into()),
        },
        RpiCommandPayload::Close {},
    ] {
        let encoded = serde_json::to_string(&payload).unwrap();
        assert_eq!(
            RpiCommandPayload::parse(payload.kind(), &encoded).unwrap(),
            payload
        );
        assert!(RpiCommandPayload::parse(RpiCommandKind::Unspecified, &encoded).is_err());
        let mut extra = serde_json::to_value(&payload).unwrap();
        extra["unrecognized"] = json!(true);
        assert!(RpiCommandPayload::parse(payload.kind(), &extra.to_string()).is_err());
    }
    assert!(RpiCommandPayload::parse(RpiCommandKind::Abort, r#"{"kind":"close"}"#).is_err());
    assert!(
        RpiCommandPayload::parse(RpiCommandKind::Close, r#"{"kind":"rpc","method":"bash"}"#)
            .is_err()
    );
    assert!(RpiCommandPayload::parse(RpiCommandKind::Close, "invalid").is_err());
}

#[test]
fn strict_submission_rejects_unknown_nested_attachment_fields() {
    let mut payload = serde_json::to_value(submit()).unwrap();
    payload["attachments"][0]["secret"] = json!("not admitted");
    assert!(RpiCommandPayload::parse(RpiCommandKind::Submit, &payload.to_string()).is_err());
}

#[test]
fn unsupported_busy_policies_and_unpaired_or_cleared_models_are_refused() {
    for policy in ["steer", "unknown"] {
        let mut payload = serde_json::to_value(submit()).unwrap();
        payload["when_busy"] = json!(policy);
        assert!(RpiCommandPayload::parse(RpiCommandKind::Submit, &payload.to_string()).is_err());
    }
    for fields in [
        json!({"model_provider":"local"}),
        json!({"model_id":"coder"}),
        json!({"model_provider":"","model_id":""}),
    ] {
        let mut payload = json!({"kind":"configure"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(RpiCommandPayload::parse(RpiCommandKind::Configure, &payload.to_string()).is_err());
    }
    assert!(
        RpiCommandPayload::parse(
            RpiCommandKind::Configure,
            r#"{"kind":"configure","title":"Only title"}"#
        )
        .is_ok()
    );
}

#[test]
fn semantic_fingerprint_ignores_transport_epoch_and_json_key_order() {
    let payload = submit();
    let first = command(&payload, 1);
    let mut replay = first.clone();
    replay.expected_worker_epoch = "replacement".into();
    let decoded: serde_json::Value = serde_json::from_str(&first.payload_json).unwrap();
    replay.payload_json = serde_json::to_string_pretty(&decoded).unwrap();
    let fingerprint = |command: &proto::DRpiCommand| {
        RpiCommandPayload::parse(command.kind.as_known().unwrap(), &command.payload_json)
            .unwrap()
            .semantic_fingerprint()
            .unwrap()
    };
    assert_eq!(fingerprint(&first), fingerprint(&replay));
    replay.payload_json = replay.payload_json.replace("inspect both", "different");
    assert_ne!(fingerprint(&first), fingerprint(&replay));
    let mut reordered = payload;
    if let RpiCommandPayload::Submit { attachments, .. } = &mut reordered {
        attachments.reverse();
    }
    assert_ne!(
        fingerprint(&first),
        reordered.semantic_fingerprint().unwrap()
    );
}

#[test]
fn settled_outcomes_have_typed_recovery_and_sanitized_errors() {
    for result in [
        RpiCommandResult::Open {
            actor_id: "actor".into(),
            interrupted_submission_ids: vec!["submit-2".into(), "submit-1".into()],
            needs_resume: true,
        },
        RpiCommandResult::Submitted {},
        RpiCommandResult::Aborted {},
        RpiCommandResult::Withdrawn { withdrawn: true },
        RpiCommandResult::Withdrawn { withdrawn: false },
        RpiCommandResult::Configured {},
        RpiCommandResult::Closed {},
        RpiCommandResult::Error {
            code: RpiErrorCode::AuthenticationRequired,
        },
    ] {
        let encoded = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<RpiCommandResult>(&encoded).unwrap(),
            result
        );
    }
    assert!(
        serde_json::from_value::<RpiCommandResult>(json!({
            "kind":"error", "code":"runtime_failure", "message":"provider secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<RpiCommandResult>(json!({
            "kind":"error", "code":"raw provider secret"
        }))
        .is_err()
    );
    assert_eq!(
        RpiErrorCode::ToolOutcomeUnknown.message(),
        "tool outcome unknown after worker restart"
    );
}
