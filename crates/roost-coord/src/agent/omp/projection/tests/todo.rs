//! Todo snapshots retain native phase boundaries and richer task states.
//! The shared projection fixture applies actual ACP update shapes through the fold.
//! Flat plan notifications must update progress without erasing known blockers.

use super::*;

fn flat_plan(compile_status: &str) -> OmpWorkerEvent {
    OmpWorkerEvent::Update {
        update: json!({"sessionUpdate":"plan","entries":[
            {"content":"Draft design","priority":"medium","status":"completed"},
            {"content":"Obtain credentials","priority":"medium","status":"pending"},
            {"content":"Compile","priority":"medium","status":compile_status},
            {"content":"Retired path","priority":"medium","status":"completed"}
        ]}),
    }
}

fn phases(transcript: &Transcript) -> &[TodoPhase] {
    transcript
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            TranscriptItem::Todo { phases, .. } => Some(phases.as_slice()),
            _ => None,
        })
        .expect("expected a canonical task list")
}

#[test]
fn omp_todo_keeps_native_phases_and_blockers_across_flat_plan_updates() {
    let mut transcript = super::super::super::store::empty_transcript();
    apply(&mut transcript, 1, flat_plan("pending"));
    apply(
        &mut transcript,
        2,
        OmpWorkerEvent::Update {
            update: json!({
                "sessionUpdate":"tool_call","toolCallId":"todo-1","title":"Updating tasks",
                "name":"todo","kind":"other","status":"completed",
                "rawOutput":{"details":{"op":"init","phases":[
                    {"name":"Design","tasks":[
                        {"content":"Draft design","status":"completed"},
                        {"content":"Obtain credentials","status":"blocked","blocker":"Needs a credential"}
                    ]},
                    {"name":"Build","tasks":[
                        {"content":"Compile","status":"pending"},
                        {"content":"Retired path","status":"abandoned"}
                    ]}
                ]}}
            }),
        },
    );
    assert_eq!(phases(&transcript).len(), 2);
    assert_eq!(phases(&transcript)[1].name, "Build");

    apply(&mut transcript, 3, flat_plan("pending"));
    apply(&mut transcript, 4, flat_plan("in_progress"));
    let current = phases(&transcript);
    assert_eq!(current.len(), 2);
    assert_eq!(current[0].tasks[1].status, TodoStatus::Blocked);
    assert_eq!(
        current[0].tasks[1].blocker.as_deref(),
        Some("Needs a credential")
    );
    assert_eq!(current[1].tasks[0].status, TodoStatus::InProgress);
    assert_eq!(current[1].tasks[1].status, TodoStatus::Abandoned);

    apply(
        &mut transcript,
        5,
        OmpWorkerEvent::Update {
            update: json!({"sessionUpdate":"plan","entries":[]}),
        },
    );
    assert_eq!(
        phases(&transcript)
            .iter()
            .map(|phase| phase.tasks.len())
            .sum::<usize>(),
        0
    );
}
