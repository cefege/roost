//! Code-mode completion must not wait for an unawaited nested child.
//! The real sandbox races a blocked worker call against a fast one, then the
//! native owner must settle and cancel the remaining owned work.

mod support;

use std::sync::atomic::Ordering;

use roost_llm::Message;
use support::{Harness, text_reply, tool_reply};

#[tokio::test]
async fn promise_race_delivers_fast_result_and_releases_blocked_owned_child() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.tools.answer("read", "slow-result");
    harness.tools.answer("bash", "fast-result");
    harness.tools.hold_next.store(true, Ordering::SeqCst);
    let args = serde_json::json!({"code": r#"
const winner = await Promise.race([
    tools.read({i: "Waiting on the slow fixture", path: "/repo/slow"}),
    tools.bash({i: "Completing the fast fixture", command: "printf fast"})
]);
text(winner);
"#})
    .to_string();
    harness
        .llm
        .script(&id, tool_reply(&[("race-script", "codemode", &args)]));
    harness.llm.script(&id, text_reply("The fast call won."));
    harness
        .submit(&id, "Complete when the first nested call answers.")
        .await;
    let settled = harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    let result = requests
        .iter()
        .find_map(|request| {
            request.messages.iter().find_map(|message| match message {
                Message::ToolResult {
                    call_id,
                    text,
                    is_error: false,
                    ..
                } if call_id == "race-script" => Some(text),
                _ => None,
            })
        })
        .expect("the completed script result reaches the follow-up model request");
    assert!(
        result.contains("fast-result"),
        "unexpected race result: {result}"
    );
    assert!(
        !result.contains("slow-result"),
        "the blocked result must not win: {result}"
    );
    assert_eq!(
        settled.run_state,
        roost_protocol::wire::agent_chat::AgentRunState::Idle
    );
}
