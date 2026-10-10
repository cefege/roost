//! Agent chat events form one canonical transcript through the shared fold.

use roost_protocol::wire::agent_chat::{
    AgentRunState, ChatEvent, Transcript, TranscriptBlock, TranscriptItem, UsageTotals,
    fold_chat_event,
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
                children: Vec::new(),
            },
        ],
        run_state: AgentRunState::Running,
        error: None,
        model: None,
        thinking_level: None,
        usage: UsageTotals::default(),
        mode: None,
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
            transcript: reset.clone(),
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
                transcript: Transcript {
                    items: vec![],
                    run_state: AgentRunState::Idle,
                    error: None,
                    model: None,
                    thinking_level: None,
                    mode: None,
                    usage: UsageTotals::default(),
                },
            },
        ),
        (
            r#"{"type":"item","item":{"id":"u","kind":"user","text":"hi"}}"#,
            ChatEvent::Item {
                item: TranscriptItem::User {
                    id: "u".into(),
                    text: "hi".into(),
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
            },
        ),
        (
            r#"{"type":"usage","usage":{"input_tokens":1,"output_tokens":2,"cost_usd":0.5}}"#,
            ChatEvent::Usage {
                usage: UsageTotals {
                    input_tokens: 1,
                    output_tokens: 2,
                    cost_usd: 0.5,
                },
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
fn notice_plan_and_advisory_items_fold_and_plan_updates_replace() {
    let mut value = transcript();
    for item in [
        TranscriptItem::Notice {
            id: "n".into(),
            level: "warn".into(),
            title: "Warning".into(),
            body: "Details".into(),
        },
        TranscriptItem::Plan {
            id: "p".into(),
            title: "Plan".into(),
            content: "Steps".into(),
            state: "proposed".into(),
        },
        TranscriptItem::Advisory {
            id: "adv".into(),
            severity: "concern".into(),
            note: "Check this".into(),
            delivered: false,
        },
        TranscriptItem::Plan {
            id: "p".into(),
            title: "Plan".into(),
            content: "Updated".into(),
            state: "approved".into(),
        },
    ] {
        fold_chat_event(&mut value, &ChatEvent::Item { item });
    }
    assert!(matches!(&value.items[2], TranscriptItem::Notice { body, .. } if body == "Details"));
    assert!(
        matches!(&value.items[3], TranscriptItem::Plan { state, content, .. } if state == "approved" && content == "Updated")
    );
    assert!(matches!(
        &value.items[4],
        TranscriptItem::Advisory {
            delivered: false,
            ..
        }
    ));
}

#[test]
fn tool_children_default_when_omitted_from_json() {
    let item: TranscriptItem = serde_json::from_str(
        r#"{"kind":"tool","id":"t","call_id":"c","tool_name":"task","args_json":"{}","output":"","is_error":false,"running":false}"#,
    ).expect("legacy tool item");
    assert!(matches!(item, TranscriptItem::Tool { children, .. } if children.is_empty()));
}
