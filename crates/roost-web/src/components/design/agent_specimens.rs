//! The agent-chat timeline specimen, rendered from its shipped component so the
//! design gallery shows the same markup as live chats.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::TranscriptItem;

use crate::components::agent_chat::timeline_rail::StepGroup;

/// One tool call in a sample run, including output for expanded code and diff previews.
fn sample_step(
    id: &str,
    tool_name: &str,
    args_json: &str,
    running: bool,
    queued: bool,
    is_error: bool,
) -> TranscriptItem {
    let output = match id {
        "read" => "pub fn greeting(name: &str) -> String {\n    format!(\"Hello, {name}\")\n}\n",
        "clippy" => "error: unused variable `result`\n  --> src/lib.rs:12:9",
        _ => "",
    };
    let details_json = if id == "edit" {
        serde_json::json!({
            "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n pub fn greeting(name: &str) -> String {\n-    format!(\"Hi, {name}\")\n+    format!(\"Hello, {name}\")\n }\n"
        })
        .to_string()
    } else {
        String::new()
    };
    TranscriptItem::Tool {
        id: id.to_owned(),
        call_id: id.to_owned(),
        tool_name: tool_name.to_owned(),
        args_json: args_json.to_owned(),
        output: output.to_owned(),
        is_error,
        running,
        queued,
        details_json,
        children: Vec::new(),
    }
}

/// One run's tool calls as a step group, with a step in every mark state.
#[component]
pub fn TimelineSpecimen() -> Element {
    let patch_tag = "A1B2";
    let edit_args = serde_json::json!({
        "input": format!("[src/lib.rs#{patch_tag}]\nPUT 2.=2:\n+    format!(\"Hello, {{name}}\")")
    })
    .to_string();
    let steps = vec![
        sample_step(
            "read",
            "read",
            r#"{"path":"src/lib.rs"}"#,
            false,
            false,
            false,
        ),
        sample_step(
            "test",
            "bash",
            r#"{"command":"cargo test -p roost-web"}"#,
            true,
            false,
            false,
        ),
        sample_step("edit", "edit", &edit_args, false, false, false),
        sample_step(
            "clippy",
            "bash",
            r#"{"command":"cargo clippy"}"#,
            false,
            false,
            true,
        ),
    ];
    rsx! {
        div { class: "max-w-reading",
            StepGroup { steps, expanded: false }
        }
    }
}
