//! Disposable application smoke for the public native AgentRuntime.
//! Scripted model transport makes the actual kernel, adapters, tools and restart
//! deterministic without operator credentials or production data.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../tests/support/mod.rs"]
mod support;

use roost_agent::AgentStore;
use roost_llm::Message;
use roost_protocol::wire::agent_chat::AgentRunState;
use support::{Harness, text_reply, tool_reply};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.tools.answer("read", "fixture bytes");
    harness.llm.script(
        &id,
        tool_reply(&[("read-1", "read", r#"{"path":"fixture.txt"}"#)]),
    );
    harness.llm.script(&id, text_reply("Read verified."));
    harness.submit(&id, "Read the fixture.").await;
    let first =
        match tokio::time::timeout(std::time::Duration::from_secs(3), harness.settle(&id)).await {
            Ok(transcript) => transcript,
            Err(_) => {
                let batches = harness.store.durable_batches(&id).await?;
                let mut tasks = std::collections::BTreeMap::new();
                for batch in &batches {
                    for write in &batch.writes {
                        if let roost_agent::durable::StorageWrite::Task { value } = write {
                            tasks.insert(value.id.get(), value);
                        }
                    }
                }
                serde_json::to_writer(
                    std::io::stderr(),
                    &serde_json::json!({
                        "tasks": tasks, "batches": batches.len(),
                        "requests": harness.llm.requests_for(&id).len(),
                        "entries": harness.store.entries(&id).await?,
                        "events": harness.sink.events_for(&id),
                    }),
                )?;
                return Err("native tool round remained active".into());
            }
        };
    if first.run_state != AgentRunState::Idle
        || !support::texts(&first)
            .iter()
            .any(|text| text == "Read verified.")
    {
        return Err(format!("native tool round did not finish: {first:?}").into());
    }
    let requests = harness.llm.requests_for(&id);
    let result_seen = requests.get(1).is_some_and(|request| {
        request.messages.iter().any(|message| {
            matches!(message, Message::ToolResult { call_id, text, is_error: false, .. }
            if call_id == "read-1" && text == "fixture bytes")
        })
    });
    if !result_seen || harness.tools.calls_to("read").len() != 1 {
        serde_json::to_writer(
            std::io::stderr(),
            &serde_json::json!({
                "requests": requests.iter().map(|request| &request.messages).collect::<Vec<_>>(),
                "readEffects": harness.tools.calls_to("read").len(),
                "entries": harness.store.entries(&id).await?,
            }),
        )?;
        return Err("actual native tool result did not reach the next request exactly once".into());
    }

    let restarted = harness.restart_runtime();
    harness.llm.script(&id, text_reply("Restart verified."));
    restarted
        .submit(&id, "Continue after restart.".into())
        .await?;
    let resumed = harness.settle_runtime(&restarted, &id).await;
    if !support::texts(&resumed)
        .iter()
        .any(|text| text == "Restart verified.")
    {
        return Err(format!("native restart did not finish: {resumed:?}").into());
    }
    if harness.tools.calls_to("read").len() != 1 {
        return Err("restart repeated an already settled tool effect".into());
    }

    let code_id = harness.conversation().await;
    harness.llm.script(&code_id, tool_reply(&[("script-1", "codemode",
        r#"{"code":"const value = await tools.read({i: 'Reading fixture', path: 'fixture.txt'}); text(value); return value;"}"#)]));
    harness
        .llm
        .script(&code_id, text_reply("Code mode verified."));
    harness
        .submit(&code_id, "Read through a code-mode nested tool.")
        .await;
    let code = harness.settle(&code_id).await;
    if !support::texts(&code)
        .iter()
        .any(|text| text == "Code mode verified.")
    {
        return Err(format!("native code mode did not finish: {code:?}").into());
    }
    let code_requests = harness.llm.requests_for(&code_id);
    let code_result_seen = code_requests.get(1).is_some_and(|request| request.messages.iter().any(|message| {
        matches!(message, Message::ToolResult { call_id, is_error: false, .. } if call_id == "script-1")
    }));
    if !code_result_seen || harness.tools.calls_to("read").len() != 2 {
        return Err(format!("code mode did not settle one nested effect: {code:?}").into());
    }
    let batches = harness.store.durable_batches(&id).await?;
    if !batches
        .windows(2)
        .all(|pair| pair[1].sequence == pair[0].sequence + 1)
    {
        return Err("native kernel log is discontinuous".into());
    }
    serde_json::to_writer(
        std::io::stdout(),
        &serde_json::json!({
            "nativeToolRound": "passed",
            "nativeRestart": "passed",
            "nativeCodeModeNestedTool": "passed",
            "mainRequests": harness.llm.requests_for(&id).len(),
            "codeModeRequests": code_requests.len(),
            "readEffects": harness.tools.calls_to("read").len(),
            "durableBatches": batches.len(),
        }),
    )?;
    Ok(())
}
