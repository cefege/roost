//! The private OMP browser bridge: decoding, bounded transient state, receipt
//! fencing, and the rule that a service value never reaches a transcript or a
//! log line.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::client::agent_chat::{
    MAX_CANCELLED_HOST_SERVICES, MAX_HOST_SERVICE_VALUE_BYTES, MAX_PENDING_ACTION_RECEIPTS,
    MAX_PENDING_HOST_SERVICES, OmpHostInbox, OmpHostService, OmpHostServiceKind,
    OmpUiActionReceipt,
};
use roost_client_core::sync::decode::DecodeRefusal;
use roost_client_core::{ClientEvent, SyncFrame};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{OmpHostServiceFrame, OmpUiActionReceiptFrame};
use std::sync::Arc;

use sync_decode_support::{control, decoded, deliver, frame_of, ready_core, refused};

const CONVERSATION: &str = "conversation-a";
const VIEW: &str = "view-a";
const TRANSPORT: &str = "transport-a";

fn service_frame(value: &str) -> Frame {
    Frame::OmpHostService(Box::new(OmpHostServiceFrame {
        service_id: "service-1".to_owned(),
        origin_request_id: "request-1".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        view_id: VIEW.to_owned(),
        transport_id: TRANSPORT.to_owned(),
        kind: "copy_text".to_owned(),
        value: value.to_owned(),
        cancelled: false,
        __buffa_unknown_fields: Default::default(),
    }))
}

fn receipt_frame(after_seq: u64) -> Frame {
    Frame::OmpUiActionReceipt(Box::new(OmpUiActionReceiptFrame {
        origin_request_id: "request-1".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        view_id: VIEW.to_owned(),
        transport_id: TRANSPORT.to_owned(),
        after_seq,
        error: String::new(),
        __buffa_unknown_fields: Default::default(),
    }))
}

#[test]
fn a_host_service_frame_decodes_as_a_control_with_its_kind_and_value() {
    let event = decoded(&control(service_frame("hello")), 4);
    let ClientEvent::SyncFrameReceived {
        delivery_seq,
        frame,
        ..
    } = event
    else {
        panic!("expected a received frame");
    };
    assert_eq!(
        delivery_seq, 0,
        "host services never consume the ACK window"
    );
    let SyncFrame::OmpHostService { service } = frame else {
        panic!("expected a host service");
    };
    assert_eq!(service.kind, OmpHostServiceKind::CopyText);
    assert_eq!(&*service.value, "hello");
    assert!(!service.cancelled);
}

#[test]
fn a_cancellation_keeps_the_identity_and_carries_no_value() {
    let mut frame = match service_frame("") {
        Frame::OmpHostService(frame) => *frame,
        _ => unreachable!(),
    };
    frame.cancelled = true;
    let event = decoded(&control(Frame::OmpHostService(Box::new(frame))), 4);
    let SyncFrame::OmpHostService { service } = frame_of(&event) else {
        panic!("expected a host service");
    };
    assert!(service.cancelled);
    assert!(service.value.is_empty());
    assert_eq!(service.transport_id, TRANSPORT);
}

#[test]
fn malformed_host_service_frames_are_refused_without_panicking() {
    let mut frame = match service_frame("") {
        Frame::OmpHostService(frame) => *frame,
        _ => unreachable!(),
    };
    frame.kind = "run_script".to_owned();
    assert!(matches!(
        refused(&control(Frame::OmpHostService(Box::new(frame)))),
        DecodeRefusal::MalformedArm {
            arm: "omp_host_service",
            ..
        }
    ));

    let mut frame = match service_frame("") {
        Frame::OmpHostService(frame) => *frame,
        _ => unreachable!(),
    };
    frame.cancelled = true;
    frame.value = "still here".to_owned();
    assert!(matches!(
        refused(&control(Frame::OmpHostService(Box::new(frame)))),
        DecodeRefusal::MalformedArm { .. }
    ));

    let oversized = "x".repeat(MAX_HOST_SERVICE_VALUE_BYTES + 1);
    assert!(matches!(
        refused(&control(service_frame(&oversized))),
        DecodeRefusal::MalformedArm { .. }
    ));

    let mut frame = match service_frame("hello") {
        Frame::OmpHostService(frame) => *frame,
        _ => unreachable!(),
    };
    frame.view_id.clear();
    assert!(matches!(
        refused(&control(Frame::OmpHostService(Box::new(frame)))),
        DecodeRefusal::MalformedArm { .. }
    ));
}

#[test]
fn an_action_receipt_decodes_with_its_sequence_fence() {
    let event = decoded(&control(receipt_frame(12)), 9);
    let SyncFrame::OmpUiActionReceipt { receipt } = frame_of(&event) else {
        panic!("expected a receipt");
    };
    assert_eq!(receipt.after_seq, 12);
    assert_eq!(receipt.view_id, VIEW);

    let mut frame = match receipt_frame(12) {
        Frame::OmpUiActionReceipt(frame) => *frame,
        _ => unreachable!(),
    };
    frame.transport_id.clear();
    assert!(matches!(
        refused(&control(Frame::OmpUiActionReceipt(Box::new(frame)))),
        DecodeRefusal::MalformedArm {
            arm: "omp_ui_action_receipt",
            ..
        }
    ));
}

fn request(
    service_id: &str,
    transport: &str,
    kind: OmpHostServiceKind,
    value: &str,
) -> OmpHostService {
    OmpHostService {
        service_id: service_id.to_owned(),
        origin_request_id: "request-1".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        view_id: VIEW.to_owned(),
        transport_id: transport.to_owned(),
        kind,
        value: value.into(),
        cancelled: false,
    }
}

fn cancellation(service_id: &str) -> OmpHostService {
    OmpHostService {
        cancelled: true,
        value: Arc::from(""),
        ..request(service_id, TRANSPORT, OmpHostServiceKind::CopyText, "")
    }
}

#[test]
fn services_are_bounded_deduplicated_and_scoped_to_the_exact_transport() {
    let mut inbox = OmpHostInbox::new();
    for index in 0..MAX_PENDING_HOST_SERVICES {
        assert!(inbox.apply_service(request(
            &format!("service-{index}"),
            TRANSPORT,
            OmpHostServiceKind::CopyText,
            "text"
        )));
    }
    assert!(
        !inbox.apply_service(request(
            "overflow",
            TRANSPORT,
            OmpHostServiceKind::OpenUrl,
            "x"
        )),
        "the 33rd prompt is dropped"
    );
    assert!(
        !inbox.apply_service(request(
            "service-0",
            TRANSPORT,
            OmpHostServiceKind::CopyText,
            "again"
        )),
        "a replayed id does not reopen a prompt"
    );
    assert!(
        inbox
            .pending_services(CONVERSATION, VIEW, "other-transport")
            .is_empty(),
        "a service for another native transport is not actionable"
    );
    assert_eq!(
        inbox.pending_services(CONVERSATION, VIEW, TRANSPORT).len(),
        MAX_PENDING_HOST_SERVICES
    );
}

#[test]
fn cancellation_removes_the_prompt_and_a_late_replay_cannot_resurrect_it() {
    let mut inbox = OmpHostInbox::new();
    assert!(inbox.apply_service(request(
        "service-1",
        TRANSPORT,
        OmpHostServiceKind::CopyText,
        "secret"
    )));
    assert!(inbox.apply_service(cancellation("service-1")));
    assert!(
        inbox
            .pending_services(CONVERSATION, VIEW, TRANSPORT)
            .is_empty()
    );
    assert!(
        !inbox.apply_service(request(
            "service-1",
            TRANSPORT,
            OmpHostServiceKind::CopyText,
            "secret"
        )),
        "a cancelled service id is tombstoned"
    );
    assert!(
        !inbox.apply_service(cancellation("service-1")),
        "a duplicate cancellation changes nothing"
    );
    assert_eq!(MAX_CANCELLED_HOST_SERVICES, 64);
}

#[test]
fn a_completed_service_cannot_be_replayed_into_a_second_prompt() {
    let mut inbox = OmpHostInbox::new();
    assert!(inbox.apply_service(request(
        "service-1",
        TRANSPORT,
        OmpHostServiceKind::CopyText,
        "text"
    )));
    assert!(inbox.complete_service("service-1"));
    assert!(
        !inbox.apply_service(request(
            "service-1",
            TRANSPORT,
            OmpHostServiceKind::CopyText,
            "text"
        )),
        "a spent service id is tombstoned"
    );
}

#[test]
fn receipts_wait_for_the_transcript_fence_and_drop_other_identities() {
    let mut inbox = OmpHostInbox::new();
    let receipt = OmpUiActionReceipt {
        origin_request_id: "request-1".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        view_id: VIEW.to_owned(),
        transport_id: TRANSPORT.to_owned(),
        after_seq: 20,
        error: String::new(),
    };
    assert!(inbox.apply_receipt(receipt.clone()));
    assert!(
        !inbox.apply_receipt(receipt.clone()),
        "a duplicate receipt is dropped"
    );
    assert!(
        inbox
            .take_ready_receipt(CONVERSATION, VIEW, TRANSPORT, 19)
            .is_none(),
        "loaded_seq below after_seq leaves the receipt fenced"
    );
    assert!(
        inbox
            .take_ready_receipt(CONVERSATION, VIEW, "other-transport", 20)
            .is_none(),
        "a receipt for another transport never applies here"
    );
    assert_eq!(
        inbox.take_ready_receipt(CONVERSATION, VIEW, TRANSPORT, 20),
        Some(receipt)
    );
    assert!(
        inbox
            .take_ready_receipt(CONVERSATION, VIEW, TRANSPORT, 20)
            .is_none()
    );
}

#[test]
fn receipts_are_bounded_and_a_removed_conversation_forgets_its_prompts() {
    let mut inbox = OmpHostInbox::new();
    for index in 0..(MAX_PENDING_ACTION_RECEIPTS + 8) {
        assert!(inbox.apply_receipt(OmpUiActionReceipt {
            origin_request_id: format!("request-{index}"),
            conversation_id: CONVERSATION.to_owned(),
            view_id: VIEW.to_owned(),
            transport_id: TRANSPORT.to_owned(),
            after_seq: 1,
            error: String::new(),
        }));
    }
    assert_eq!(inbox.pending_receipt_count(), MAX_PENDING_ACTION_RECEIPTS);
    assert!(inbox.apply_service(request(
        "service-1",
        TRANSPORT,
        OmpHostServiceKind::OpenUrl,
        "https://example.com"
    )));
    assert!(inbox.forget_conversation(CONVERSATION));
    assert_eq!(inbox.pending_service_count(), 0);
    assert_eq!(inbox.pending_receipt_count(), 0);
}

#[test]
fn a_delivered_service_stays_in_the_transient_inbox_and_never_reaches_a_transcript() {
    let (mut core, generation) = ready_core();
    let effects = deliver(
        &mut core,
        generation,
        &control(service_frame("private-copy")),
    );
    assert!(
        effects.is_empty(),
        "a control host service is never acknowledged"
    );
    assert!(core.store().agent_chat.transcripts.is_empty());
    let debug = format!("{:?}", core.store().omp_host);
    assert!(!debug.contains("private-copy"), "{debug}");
    assert_eq!(core.store().omp_host.pending_service_count(), 1);
}

#[test]
fn a_host_service_value_never_appears_in_debug_output() {
    let debug = format!(
        "{:?}",
        request(
            "service-1",
            TRANSPORT,
            OmpHostServiceKind::CopyText,
            "top-secret-value"
        )
    );
    assert!(!debug.contains("top-secret-value"), "{debug}");
    assert!(debug.contains("value_bytes"));
}
