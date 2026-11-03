//! ACP tool calls projected into replaceable transcript rows.
//! The pure projector delegates content and partial update handling here.
//! Tool ids remain stable across status changes and output replacement.

use agent_client_protocol::v1::schema::{
    ContentBlock, ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate,
};
use roost_protocol::wire::agent_chat::{Transcript, TranscriptItem};

pub(super) fn new_tool(call: ToolCall) -> TranscriptItem {
    let tool_name = call.name.unwrap_or(call.title);
    let raw_output = call.raw_output;
    let details_json =
        merged_details(raw_output.as_ref(), Some(&call.content), None).unwrap_or_default();
    TranscriptItem::Tool {
        id: format!("t-{}", call.tool_call_id),
        call_id: call.tool_call_id.to_string(),
        tool_name,
        args_json: call
            .raw_input
            .map(|value| value.to_string())
            .unwrap_or_else(|| "{}".into()),
        output: raw_output
            .as_ref()
            .map(raw_output_text)
            .unwrap_or_else(|| tool_content_text(&call.content)),
        is_error: call.status == ToolCallStatus::Failed,
        running: !matches!(
            call.status,
            ToolCallStatus::Completed | ToolCallStatus::Failed
        ),
        queued: false,
        details_json,
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
        details_json,
        ..
    } = &mut item
    {
        let fields = update.fields;
        if let Some(name) = fields.name {
            *tool_name = name;
        }
        if let Some(input) = fields.raw_input {
            *args_json = input.to_string();
        }
        let raw_output = fields.raw_output;
        let content = fields.content;
        if let Some(raw) = raw_output.as_ref() {
            *output = raw_output_text(raw);
        } else if let Some(content) = content.as_ref() {
            *output = tool_content_text(content);
        }
        if let Some(details) = merged_details(
            raw_output.as_ref(),
            content.as_deref(),
            Some(details_json.as_str()),
        ) {
            *details_json = details;
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

fn raw_output_text(raw: &serde_json::Value) -> String {
    raw.get("content")
        .and_then(|content| {
            content.as_str().map(str::to_owned).or_else(|| {
                content.as_array().map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| {
                            (block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                                .then(|| block.get("text").and_then(serde_json::Value::as_str))
                                .flatten()
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            })
        })
        .or_else(|| raw.as_str().map(str::to_owned))
        .unwrap_or_else(|| serde_json::to_string_pretty(raw).unwrap_or_default())
}

fn merged_details(
    raw: Option<&serde_json::Value>,
    content: Option<&[ToolCallContent]>,
    previous: Option<&str>,
) -> Option<String> {
    let raw_details = raw.and_then(|raw| raw.get("details")).cloned();
    let content_diff = raw_details
        .as_ref()
        .and_then(|details| {
            Some(unified_diff(
                details.get("path")?.as_str()?,
                details.get("oldText")?.as_str()?,
                details.get("newText")?.as_str()?,
            ))
        })
        .or_else(|| tool_content_diff(content.unwrap_or_default()));
    if raw_details.is_none() && content_diff.is_none() {
        return None;
    }
    let mut details = raw_details
        .or_else(|| previous.and_then(|value| serde_json::from_str(value).ok()))
        .unwrap_or_else(|| serde_json::json!({}));
    let has_diff = details
        .get("diff")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|diff| diff.starts_with("--- ") && diff.contains("\n@@ "));
    if !has_diff
        && let Some(diff) = content_diff
        && let Some(object) = details.as_object_mut()
    {
        object.insert("diff".into(), serde_json::Value::String(diff));
    }
    Some(serde_json::to_string(&details).unwrap_or_default())
}

fn tool_content_diff(content: &[ToolCallContent]) -> Option<String> {
    let diffs: Vec<_> = content
        .iter()
        .filter_map(|content| match content {
            ToolCallContent::Diff(diff) => Some(unified_diff(
                &diff.path.display().to_string(),
                diff.old_text.as_deref().unwrap_or_default(),
                &diff.new_text,
            )),
            _ => None,
        })
        .collect();
    (!diffs.is_empty()).then(|| diffs.join("\n"))
}

fn unified_diff(path: &str, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
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
