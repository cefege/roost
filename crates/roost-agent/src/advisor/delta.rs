//! Renders incremental primary entries for advisor reviews and excludes
//! delivered advisories from the model-visible shadow transcript.
//! Results are truncated at the transcript boundary before becoming context.

use crate::records::Entry;

const TOOL_RESULT_LIMIT: usize = 4 * 1024;

pub(crate) fn render_delta(entries: &[(u64, Entry)], cursor: u64) -> String {
    entries
        .iter()
        .filter(|(seq, _)| *seq > cursor)
        .filter_map(|(_, entry)| render_entry(entry))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_entry(entry: &Entry) -> Option<String> {
    match entry {
        Entry::User { text } => Some(format!("## User\n{text}")),
        Entry::Assistant { blocks, .. } => {
            let blocks = blocks
                .iter()
                .map(|block| match block {
                    roost_llm::AssistantBlock::Text { text } => text.clone(),
                    roost_llm::AssistantBlock::Thinking { text, .. } => text.clone(),
                    roost_llm::AssistantBlock::ToolCall {
                        name, args_json, ..
                    } => format!("{name}({args_json})"),
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!blocks.is_empty()).then(|| format!("## Assistant\n{blocks}"))
        }
        Entry::ToolResult {
            tool_name,
            text,
            is_error,
            ..
        } => {
            let text = truncate(text, TOOL_RESULT_LIMIT);
            Some(format!(
                "## Tool result: {tool_name}{}\n{text}",
                if *is_error { " (error)" } else { "" }
            ))
        }
        Entry::Compaction { summary, .. } => Some(format!("## Compaction\n{summary}")),
        Entry::ModeChange { mode, plan_title } => Some(format!(
            "## Mode change: {}{}",
            mode.as_str(),
            plan_title
                .as_ref()
                .map(|title| format!(" ({title})"))
                .unwrap_or_default()
        )),
        Entry::PlanProposal {
            title,
            content,
            state,
            ..
        } => Some(format!(
            "## Plan proposal ({})\n{title}\n{content}",
            state.as_str()
        )),
        Entry::Notice { level, title, body } => {
            Some(format!("## Notice ({level})\n{title}: {body}"))
        }
        Entry::Advisory {
            severity,
            note,
            delivered,
            ..
        } if !delivered => Some(format!("## Advisory card ({})\n{note}", severity.as_str())),
        Entry::Advisory { .. } => None,
    }
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
