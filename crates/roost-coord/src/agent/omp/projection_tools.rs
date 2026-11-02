//! ACP tool calls projected into replaceable transcript rows.
//! The pure projector delegates content and partial update handling here.
//! Tool ids remain stable across status changes and output replacement.

use agent_client_protocol::v1::schema::{
    ContentBlock, ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate,
};
use roost_protocol::wire::agent_chat::{Transcript, TranscriptItem};

pub(super) fn new_tool(call: ToolCall) -> TranscriptItem {
    TranscriptItem::Tool {
        id: format!("t-{}", call.tool_call_id),
        call_id: call.tool_call_id.to_string(),
        tool_name: call.title,
        args_json: call
            .raw_input
            .map(|value| value.to_string())
            .unwrap_or_else(|| "{}".into()),
        output: tool_content_text(&call.content),
        is_error: call.status == ToolCallStatus::Failed,
        running: !matches!(
            call.status,
            ToolCallStatus::Completed | ToolCallStatus::Failed
        ),
        queued: false,
        details_json: String::new(),
        children: vec![],
    }
}

pub(super) fn update_tool(
    transcript: &Transcript,
    update: ToolCallUpdate,
) -> Option<TranscriptItem> {
    let target = format!("t-{}", update.tool_call_id);
    let mut item = transcript
        .items
        .iter()
        .find(|item| matches!(item, TranscriptItem::Tool { id, .. } if id == &target))?
        .clone();
    if let TranscriptItem::Tool {
        tool_name,
        args_json,
        output,
        is_error,
        running,
        ..
    } = &mut item
    {
        let fields = update.fields;
        if let Some(title) = fields.title {
            *tool_name = title;
        }
        if let Some(input) = fields.raw_input {
            *args_json = input.to_string();
        }
        if let Some(content) = fields.content {
            *output = tool_content_text(&content);
        } else if matches!(
            fields.status,
            Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
        ) && let Some(raw) = fields.raw_output
        {
            *output = raw
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| serde_json::to_string_pretty(&raw).unwrap_or_default());
        }
        if let Some(status) = fields.status {
            *running = !matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed);
            if status == ToolCallStatus::Failed {
                *is_error = true;
            }
        }
    }
    Some(item)
}

fn tool_content_text(content: &[ToolCallContent]) -> String {
    content
        .iter()
        .filter_map(|content| match content {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            ToolCallContent::Diff(diff) => Some(format!("Edited {}", diff.path.display())),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
