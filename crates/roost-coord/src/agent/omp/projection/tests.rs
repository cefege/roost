//! Streaming, tool boundaries and turn settlement through the shared fold.
//! Projection is pure; fixtures use real ACP JSON update spellings.
//! These cases protect consumer-visible transcript ordering and busy state.

use super::*;
use serde_json::json;

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
