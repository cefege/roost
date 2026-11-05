//! Streaming, tool boundaries and turn settlement through the shared fold.
//! Projection is pure; fixtures use real ACP JSON update spellings.
//! These cases protect consumer-visible transcript ordering and busy state.

use super::*;
use serde_json::json;

mod todo;

fn apply(transcript: &mut Transcript, seq: u64, event: OmpWorkerEvent) -> Vec<ChatEvent> {
    let projected = project(transcript, "r1", seq, &event);
    for event in &projected.events {
        fold_chat_event(transcript, event);
    }
    projected.events
}
fn chunk(text: &str, thinking: bool) -> OmpWorkerEvent {
    OmpWorkerEvent::Update {
        update: json!({"sessionUpdate":if thinking {"agent_thought_chunk"} else {"agent_message_chunk"},"content":{"type":"text","text":text}}),
    }
}
fn ended(outstanding: u32) -> OmpWorkerEvent {
    OmpWorkerEvent::TurnEnded {
        request_id: "req".into(),
        stop_reason: "end_turn".into(),
        error: None,
        outstanding,
    }
}
#[test]
fn omp_chunks_append_and_thought_text_use_distinct_blocks() {
    let mut transcript = super::super::store::empty_transcript();
    let events = apply(&mut transcript, 1, chunk("think", true));
    assert!(matches!(
        &events[..],
        [
            ChatEvent::Item { .. },
            ChatEvent::ThinkingDelta { block: 0, .. }
        ]
    ));
    apply(&mut transcript, 2, chunk("Hel", false));
    let events = apply(&mut transcript, 3, chunk("lo", false));
    assert!(matches!(
        &events[..],
        [ChatEvent::TextDelta { block: 1, .. }]
    ));
    assert!(
        matches!(&transcript.items[..],[TranscriptItem::Assistant {blocks,streaming:true,..}] if blocks == &vec![TranscriptBlock::Thinking {text:"think".into()},TranscriptBlock::Text {text:"Hello".into()}])
    );
}
#[test]
fn omp_tool_call_closes_assistant_and_partial_completion_sets_output() {
    let mut transcript = super::super::store::empty_transcript();
    apply(&mut transcript, 1, chunk("checking", false));
    apply(
        &mut transcript,
        2,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call","toolCallId":"call","title":"Read file","status":"in_progress","rawInput":{"path":"a"}}),
        },
    );
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::Assistant {
            streaming: false,
            ..
        }
    ));
    apply(
        &mut transcript,
        3,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call_update","toolCallId":"call","status":"completed","rawOutput":"contents"}),
        },
    );
    assert!(
        matches!(&transcript.items[1],TranscriptItem::Tool {running:false,output,tool_name,..} if output == "contents" && tool_name == "Read file")
    );
}

#[test]
fn omp_tool_updates_preserve_identity_output_details_and_diff() {
    let mut transcript = super::super::store::empty_transcript();
    apply(
        &mut transcript,
        1,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call","toolCallId":"edit","title":"Editing file","name":"edit","status":"in_progress","rawInput":{"path":"src/lib.rs"}}),
        },
    );
    apply(
        &mut transcript,
        2,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call_update","toolCallId":"edit","title":"A prettier title"}),
        },
    );
    apply(
        &mut transcript,
        3,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call_update","toolCallId":"edit","name":"write","rawOutput":{"content":[{"type":"text","text":"partial result"}],"details":{"phase":"streaming"}}}),
        },
    );
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::Tool {tool_name,output,details_json,..}
            if tool_name == "write" && output == "partial result"
                && serde_json::from_str::<serde_json::Value>(details_json).ok()
                    == Some(json!({"phase":"streaming"}))
    ));
    apply(
        &mut transcript,
        4,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call_update","toolCallId":"edit","status":"completed","rawOutput":{"content":[{"type":"text","text":"final result"}],"details":{"exit_code":0}}}),
        },
    );
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::Tool {tool_name,output,details_json,running,..}
            if tool_name == "write" && output == "final result" && !running
                && serde_json::from_str::<serde_json::Value>(details_json).ok()
                    == Some(json!({"exit_code":0}))
    ));
    apply(
        &mut transcript,
        5,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call_update","toolCallId":"edit","rawOutput":{"content":[{"type":"text","text":"Edit completed"}],"details":{"exit_code":0}},"content":[{"type":"diff","path":"/tmp/a.rs","oldText":"first\nold line\nlast\n","newText":"first\nnew line\nlast\n"}]}),
        },
    );
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::Tool {output,details_json,..}
            if output == "Edit completed"
                && serde_json::from_str::<serde_json::Value>(details_json).ok()
                    .is_some_and(|details| details.get("exit_code") == Some(&json!(0))
                        && details.get("diff").and_then(serde_json::Value::as_str)
                            .is_some_and(|diff| diff.contains("/tmp/a.rs")
                                && diff.contains("-old line") && diff.contains("+new line")
                                && diff.contains(" first")))
    ));
}

#[test]
fn omp_tool_title_fallback_does_not_replace_identity() {
    let mut transcript = super::super::store::empty_transcript();
    apply(
        &mut transcript,
        1,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call","toolCallId":"read","title":"Read file","status":"in_progress"}),
        },
    );
    apply(
        &mut transcript,
        2,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call_update","toolCallId":"read","title":"Changed presentation"}),
        },
    );
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::Tool {tool_name,..} if tool_name == "Read file"
    ));
}
#[test]
fn omp_turn_end_preserves_running_until_last_outstanding_prompt() {
    let mut transcript = super::super::store::empty_transcript();
    apply(
        &mut transcript,
        1,
        OmpWorkerEvent::TurnStarted {
            request_id: "req".into(),
        },
    );
    apply(&mut transcript, 2, chunk("hi", false));
    apply(&mut transcript, 3, ended(1));
    assert_eq!(transcript.run_state, AgentRunState::Running);
    apply(&mut transcript, 4, ended(0));
    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::Assistant {
            streaming: false,
            ..
        }
    ));
}
#[test]
fn omp_process_exit_fails_a_running_turn() {
    let mut transcript = super::super::store::empty_transcript();
    transcript.run_state = AgentRunState::Running;
    apply(&mut transcript, 1, chunk("partial", false));
    apply(
        &mut transcript,
        2,
        OmpWorkerEvent::ProcessExited {
            error: Some("gone".into()),
        },
    );
    assert_eq!(transcript.run_state, AgentRunState::Failed);
    assert_eq!(
        transcript.error.as_deref(),
        Some("OMP exited during a turn")
    );
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::Assistant {
            streaming: false,
            ..
        }
    ));
}

#[test]
fn omp_session_failure_does_not_settle_pending_turn_and_ready_clears_failure() {
    let mut transcript = super::super::store::empty_transcript();
    transcript.run_state = AgentRunState::Running;
    transcript.model = Some(ModelRef {
        provider: "provider".into(),
        model_id: "model".into(),
    });
    transcript.thinking_level = Some("low".into());
    transcript.mode = Some("normal".into());
    let failed = apply(
        &mut transcript,
        1,
        OmpWorkerEvent::SessionFailed {
            error: "OMP is not signed in".into(),
        },
    );
    assert!(matches!(
        failed.as_slice(),
        [ChatEvent::Agent {
            model: Some(_),
            thinking_level: Some(thinking),
            mode: Some(mode),
            omp_session: Some(OmpSessionState::Failed { error }),
        }] if thinking == "low" && mode == "normal" && error == "OMP is not signed in"
    ));
    assert_eq!(transcript.run_state, AgentRunState::Running);
    assert_eq!(transcript.items, Vec::<TranscriptItem>::new());

    let ready = apply(
        &mut transcript,
        2,
        OmpWorkerEvent::SessionReady {
            acp_session_id: "s1".into(),
            config_options: json!([]),
            mode: None,
        },
    );
    assert!(matches!(
        ready.as_slice(),
        [ChatEvent::Agent {
            omp_session: Some(OmpSessionState::Ready),
            ..
        }]
    ));
    assert_eq!(transcript.run_state, AgentRunState::Running);
    assert_eq!(transcript.items, Vec::<TranscriptItem>::new());
}

#[test]
fn omp_edit_snapshot_normalizes_numbered_diff_without_losing_context() {
    let mut transcript = super::super::store::empty_transcript();
    apply(
        &mut transcript,
        1,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call","toolCallId":"edit","name":"edit","title":"Fixing reducer","status":"in_progress"}),
        },
    );
    apply(
        &mut transcript,
        2,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"tool_call_update","toolCallId":"edit","status":"completed",
            "rawOutput":{"content":[{"type":"text","text":"Updated file"}],"details":{
                "path":"/work/sum.ts","diff":"-1|old\n+1|new\n 2|unchanged",
                "oldText":"old\nunchanged\n","newText":"new\nunchanged\n","firstChangedLine":1
            }}}),
        },
    );
    let TranscriptItem::Tool { details_json, .. } = &transcript.items[0] else {
        panic!("expected projected edit");
    };
    let details: serde_json::Value = serde_json::from_str(details_json).unwrap();
    let diff = details["diff"].as_str().unwrap();
    assert!(diff.starts_with("--- a/"));
    assert!(diff.contains("\n@@ "));
    assert!(diff.contains("\n-old\n+new\n unchanged\n"));
    assert_eq!(details["firstChangedLine"], 1);
}

#[test]
fn semantic_ui_frames_and_errors_fold_into_durable_transcript_state() {
    let mut transcript = super::super::store::empty_transcript();
    let message =
        json!({"verb":"o","body":{"id":"command-surface","mode":"inline","title":"Model"}});
    let events = apply(
        &mut transcript,
        1,
        OmpWorkerEvent::Ui {
            message: message.clone(),
        },
    );
    assert_eq!(events, vec![ChatEvent::OmpUi { message }]);
    assert!(
        transcript
            .omp_ui
            .as_ref()
            .is_some_and(|document| document.surfaces.contains_key("command-surface"))
    );
    let failed = apply(
        &mut transcript,
        2,
        OmpWorkerEvent::UiFailed {
            error: "semantic UI transport failed".into(),
        },
    );
    assert_eq!(
        failed,
        vec![ChatEvent::OmpUiError {
            error: "semantic UI transport failed".into(),
        }]
    );
    assert_eq!(
        transcript.omp_ui_error.as_deref(),
        Some("semantic UI transport failed")
    );
    let events = apply(
        &mut transcript,
        3,
        OmpWorkerEvent::UiReset {
            transport_id: Some("nonce-a".into()),
        },
    );
    assert_eq!(
        events,
        vec![ChatEvent::OmpUiReset {
            transport_id: Some("nonce-a".into()),
        }]
    );
    assert_eq!(
        transcript.omp_ui_transport_id.as_deref(),
        Some("nonce-a"),
        "the live token must survive the fold for later admission"
    );
    assert!(transcript.omp_ui.is_none());
    assert!(transcript.omp_ui_error.is_none());

    let events = apply(
        &mut transcript,
        4,
        OmpWorkerEvent::UiReset { transport_id: None },
    );
    assert_eq!(events, vec![ChatEvent::OmpUiReset { transport_id: None }]);
    assert_eq!(
        transcript.omp_ui_transport_id, None,
        "a closed view must leave no token a stale action could name"
    );
}

#[test]
fn process_exit_closes_the_live_ui_transport_before_the_transcript_is_reloaded() {
    let mut transcript = super::super::store::empty_transcript();
    transcript.omp_ui_transport_id = Some("nonce-a".into());
    transcript.omp_ui = Some(Default::default());
    let projected = project(
        &transcript,
        "r1",
        7,
        &OmpWorkerEvent::ProcessExited { error: None },
    );
    assert!(
        projected
            .events
            .iter()
            .any(|event| matches!(event, ChatEvent::OmpUiReset { transport_id: None }))
    );
    assert!(
        projected
            .effects
            .iter()
            .any(|effect| matches!(effect, OmpEffect::UiTransport(None)))
    );
}
