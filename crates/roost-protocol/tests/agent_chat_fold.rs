//! Agent chat events form one canonical transcript through the shared fold.

use roost_protocol::wire::agent_chat::{
    AgentRunState, ChatEvent, QueuedMessage, Transcript, TranscriptBlock, TranscriptItem,
    UsageTotals, fold_chat_event,
};

fn transcript() -> Transcript {
    Transcript {
        items: vec![
            TranscriptItem::Assistant {
                id: "a".into(),
                blocks: vec![],
                streaming: true,
                error: None,
            },
            TranscriptItem::Tool {
                id: "t".into(),
                call_id: "c".into(),
                tool_name: "bash".into(),
                args_json: "{}".into(),
                output: "old".into(),
                is_error: false,
                running: true,
                queued: true,
                details_json: String::new(),
                children: Vec::new(),
            },
        ],
        run_state: AgentRunState::Running,
        error: None,
        model: None,
        thinking_level: None,
        usage: UsageTotals::default(),
        mode: None,
        queued: Vec::new(),
        omp_commands: Vec::new(),
        omp_interactions: Vec::new(),
        omp_ui: None,
        omp_ui_transport_id: None,
        omp_ui_error: None,
        schedules: Vec::new(),
        omp_session: None,
    }
}

#[test]
fn reset_and_deltas_build_text() {
    let mut value = transcript();
    fold_chat_event(
        &mut value,
        &ChatEvent::TextDelta {
            item_id: "a".into(),
            block: 0,
            delta: "hello".into(),
        },
    );
    fold_chat_event(
        &mut value,
        &ChatEvent::TextDelta {
            item_id: "a".into(),
            block: 0,
            delta: " world".into(),
        },
    );
    let reset = value.clone();
    fold_chat_event(
        &mut value,
        &ChatEvent::Reset {
            transcript: Box::new(reset.clone()),
        },
    );
    assert_eq!(value, reset);
    assert!(
        matches!(&value.items[0], TranscriptItem::Assistant { blocks, .. } if blocks == &vec![TranscriptBlock::Text { text: "hello world".into() }])
    );
}

#[test]
fn block_set_replaces_block() {
    let mut value = transcript();
    fold_chat_event(
        &mut value,
        &ChatEvent::TextDelta {
            item_id: "a".into(),
            block: 0,
            delta: "x".into(),
        },
    );
    fold_chat_event(
        &mut value,
        &ChatEvent::BlockSet {
            item_id: "a".into(),
            block: 0,
            value: TranscriptBlock::Thinking {
                text: "thought".into(),
            },
        },
    );
    assert!(
        matches!(&value.items[0], TranscriptItem::Assistant { blocks, .. } if blocks == &vec![TranscriptBlock::Thinking { text: "thought".into() }])
    );
}

#[test]
fn tool_output_sets_and_trims_then_appends() {
    let mut value = transcript();
    fold_chat_event(
        &mut value,
        &ChatEvent::ToolOutput {
            item_id: "t".into(),
            trim_start: None,
            append: None,
            set: Some("abcdef".into()),
        },
    );
    fold_chat_event(
        &mut value,
        &ChatEvent::ToolOutput {
            item_id: "t".into(),
            trim_start: Some(2),
            append: Some("!".into()),
            set: None,
        },
    );
    assert!(matches!(&value.items[1], TranscriptItem::Tool { output, .. } if output == "cdef!"));
}

#[test]
fn unknown_item_is_ignored() {
    let mut value = transcript();
    let before = value.clone();
    fold_chat_event(
        &mut value,
        &ChatEvent::TextDelta {
            item_id: "missing".into(),
            block: 0,
            delta: "x".into(),
        },
    );
    assert_eq!(value, before);
}

#[test]
fn every_chat_event_round_trips_spec_json() {
    let pairs = [
        (
            r#"{"type":"reset","transcript":{"items":[],"run_state":"idle","error":null,"model":null,"thinking_level":null,"mode":null,"usage":{"input_tokens":0,"output_tokens":0,"cost_usd":0.0}}}"#,
            ChatEvent::Reset {
                transcript: Box::new(Transcript {
                    items: vec![],
                    run_state: AgentRunState::Idle,
                    error: None,
                    model: None,
                    thinking_level: None,
                    mode: None,
                    usage: UsageTotals::default(),
                    queued: Vec::new(),
                    omp_commands: Vec::new(),
                    omp_interactions: Vec::new(),
                    omp_ui: None,
                    omp_ui_transport_id: None,
                    omp_ui_error: None,
                    schedules: Vec::new(),
                    omp_session: None,
                }),
            },
        ),
        (
            r#"{"type":"item","item":{"id":"u","kind":"user","text":"hi"}}"#,
            ChatEvent::Item {
                item: TranscriptItem::User {
                    id: "u".into(),
                    text: "hi".into(),
                    attachment_names: Vec::new(),
                },
            },
        ),
        (
            r#"{"type":"text_delta","item_id":"a","block":0,"delta":"x"}"#,
            ChatEvent::TextDelta {
                item_id: "a".into(),
                block: 0,
                delta: "x".into(),
            },
        ),
        (
            r#"{"type":"thinking_delta","item_id":"a","block":1,"delta":"x"}"#,
            ChatEvent::ThinkingDelta {
                item_id: "a".into(),
                block: 1,
                delta: "x".into(),
            },
        ),
        (
            r#"{"type":"block_set","item_id":"a","block":0,"value":{"type":"text","text":"x"}}"#,
            ChatEvent::BlockSet {
                item_id: "a".into(),
                block: 0,
                value: TranscriptBlock::Text { text: "x".into() },
            },
        ),
        (
            r#"{"type":"tool_output","item_id":"t","trim_start":2,"append":"x","set":null}"#,
            ChatEvent::ToolOutput {
                item_id: "t".into(),
                trim_start: Some(2),
                append: Some("x".into()),
                set: None,
            },
        ),
        (
            r#"{"type":"run_state","run_state":"failed","error":"bad"}"#,
            ChatEvent::RunState {
                run_state: AgentRunState::Failed,
                error: Some("bad".into()),
            },
        ),
        (
            r#"{"type":"agent","model":{"provider":"p","model_id":"m"},"thinking_level":"high","mode":null}"#,
            ChatEvent::Agent {
                model: Some(roost_protocol::wire::agent_chat::ModelRef {
                    provider: "p".into(),
                    model_id: "m".into(),
                }),
                thinking_level: Some("high".into()),
                mode: None,
                omp_session: None,
            },
        ),
        (
            r#"{"type":"usage","usage":{"input_tokens":1,"output_tokens":2,"cost_usd":0.5}}"#,
            ChatEvent::Usage {
                usage: UsageTotals {
                    input_tokens: 1,
                    output_tokens: 2,
                    cost_usd: 0.5,
                    context_tokens: 0,
                },
            },
        ),
        (
            r#"{"type":"queued","message":{"id":"u7","text":"also check tests"}}"#,
            ChatEvent::Queued {
                message: QueuedMessage {
                    id: "u7".into(),
                    text: "also check tests".into(),
                    attachment_names: Vec::new(),
                    mode: String::new(),
                },
            },
        ),
        (
            r#"{"type":"queue_removed","submission_id":"u7"}"#,
            ChatEvent::QueueRemoved {
                submission_id: "u7".into(),
            },
        ),
        (
            r#"{"type":"queue_delivered","submission_ids":["u7"]}"#,
            ChatEvent::QueueDelivered {
                submission_ids: vec!["u7".into()],
            },
        ),
        (
            r#"{"type":"omp_history_reset"}"#,
            ChatEvent::OmpHistoryReset,
        ),
        (
            r#"{"type":"omp_ui_reset","transport_id":"nonce-a"}"#,
            ChatEvent::OmpUiReset {
                transport_id: Some("nonce-a".into()),
            },
        ),
        (
            r#"{"type":"omp_ui_reset","transport_id":null}"#,
            ChatEvent::OmpUiReset { transport_id: None },
        ),
        (
            r#"{"type":"omp_ui","message":{"verb":"o","body":{"id":"surface-a"}}}"#,
            ChatEvent::OmpUi {
                message: serde_json::json!({"verb":"o","body":{"id":"surface-a"}}),
            },
        ),
        (
            r#"{"type":"omp_ui_error","error":"bad frame"}"#,
            ChatEvent::OmpUiError {
                error: "bad frame".into(),
            },
        ),
    ];
    for (json, event) in pairs {
        assert_eq!(
            serde_json::from_str::<ChatEvent>(json).expect("valid event"),
            event
        );
        let encoded = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            encoded,
            serde_json::from_str::<serde_json::Value>(json).expect("literal JSON")
        );
    }
}

#[test]
fn a_queued_message_waits_outside_the_items_and_lands_after_them_when_read() {
    let mut value = transcript();
    let queued = QueuedMessage {
        id: "u9".into(),
        text: "steer".into(),
        attachment_names: vec!["queued.png".into()],
        mode: String::new(),
    };
    fold_chat_event(
        &mut value,
        &ChatEvent::Queued {
            message: queued.clone(),
        },
    );
    fold_chat_event(&mut value, &ChatEvent::Queued { message: queued });
    assert_eq!(value.queued.len(), 1);
    assert_eq!(value.items.len(), 2);
    fold_chat_event(
        &mut value,
        &ChatEvent::QueueDelivered {
            submission_ids: vec!["u9".into()],
        },
    );
    assert!(value.queued.is_empty());
    assert!(matches!(
        value.items.last(),
        Some(TranscriptItem::User {
            id,
            attachment_names,
            ..
        }) if id == "u9"
            && attachment_names.len() == 1
            && attachment_names[0] == "queued.png"
    ));
}

#[test]
fn queue_removal_removes_the_submission_from_live_transcript_state() {
    let mut value = transcript();
    fold_chat_event(
        &mut value,
        &ChatEvent::Queued {
            message: QueuedMessage {
                id: "request-1".into(),
                text: "queued text".into(),
                attachment_names: Vec::new(),
                mode: String::new(),
            },
        },
    );
    assert_eq!(value.queued.len(), 1);
    fold_chat_event(
        &mut value,
        &ChatEvent::QueueRemoved {
            submission_id: "request-1".into(),
        },
    );
    assert!(value.queued.is_empty());
    assert!(
        !value
            .items
            .iter()
            .any(|item| matches!(item, TranscriptItem::User { id, .. } if id == "request-1"))
    );
}

#[path = "agent_chat_fold/items.rs"]
mod items;
