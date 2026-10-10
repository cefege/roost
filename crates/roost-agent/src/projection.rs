//! Entry journal → transcript items. One function per direction of use: the
//! full transcript a client resets to, and the single item a freshly appended
//! entry becomes, both with the same stable item ids.

use std::collections::HashMap;

use roost_llm::{AssistantBlock, Catalog, Usage};
use roost_protocol::wire::agent_chat::{Transcript, TranscriptBlock, TranscriptItem, UsageTotals};

use crate::records::{ConversationRecord, Entry};

/// The transcript item id of a tool call's result card.
pub fn tool_item_id(call_id: &str) -> String {
    format!("tool-{call_id}")
}

/// Tool name and arguments by call id, gathered from assistant entries.
pub type ToolCallIndex = HashMap<String, (String, String)>;

pub fn index_tool_calls(entries: &[(u64, Entry)]) -> ToolCallIndex {
    let mut index = HashMap::new();
    for (_, entry) in entries {
        if let Entry::Assistant { blocks, .. } = entry {
            for block in blocks {
                if let AssistantBlock::ToolCall {
                    call_id,
                    name,
                    args_json,
                } = block
                {
                    index.insert(call_id.clone(), (name.clone(), args_json.clone()));
                }
            }
        }
    }
    index
}

pub fn transcript_blocks(blocks: &[AssistantBlock]) -> Vec<TranscriptBlock> {
    blocks
        .iter()
        .map(|block| match block {
            AssistantBlock::Text { text } => TranscriptBlock::Text { text: text.clone() },
            AssistantBlock::Thinking { text, .. } => {
                TranscriptBlock::Thinking { text: text.clone() }
            }
            AssistantBlock::ToolCall {
                call_id,
                name,
                args_json,
            } => TranscriptBlock::ToolCall {
                call_id: call_id.clone(),
                tool_name: name.clone(),
                args_json: args_json.clone(),
            },
        })
        .collect()
}

/// Child conversation ids a `task` tool result recorded in its details.
pub fn children_from_details(details_json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(details_json)
        .ok()
        .and_then(|details| {
            details.get("children").and_then(|children| {
                children.as_array().map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_owned))
                        .collect()
                })
            })
        })
        .unwrap_or_default()
}

/// The item one entry projects to, if it is user-visible.
pub fn entry_item(seq: u64, entry: &Entry, tools: &ToolCallIndex) -> Option<TranscriptItem> {
    match entry {
        Entry::User { text } => Some(TranscriptItem::User {
            id: format!("u{seq}"),
            text: text.clone(),
        }),
        Entry::Assistant {
            item_id, blocks, ..
        } => Some(TranscriptItem::Assistant {
            id: item_id.clone(),
            blocks: transcript_blocks(blocks),
            streaming: false,
            error: None,
        }),
        Entry::ToolResult {
            call_id,
            tool_name,
            text,
            is_error,
            details_json,
        } => {
            let args_json = tools
                .get(call_id)
                .map_or_else(|| "{}".to_owned(), |(_, args)| args.clone());
            Some(TranscriptItem::Tool {
                id: tool_item_id(call_id),
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                args_json,
                output: text.clone(),
                is_error: *is_error,
                running: false,
                children: children_from_details(details_json),
            })
        }
        Entry::Compaction { summary, .. } => Some(TranscriptItem::Notice {
            id: format!("c{seq}"),
            level: "info".into(),
            title: "Context compacted".into(),
            body: summary.clone(),
        }),
        Entry::ModeChange { mode, plan_title } => Some(TranscriptItem::Notice {
            id: format!("m{seq}"),
            level: "info".into(),
            title: match mode {
                crate::records::Mode::Plan => "Plan mode on".into(),
                crate::records::Mode::Normal => "Plan mode off".into(),
            },
            body: plan_title.clone().unwrap_or_default(),
        }),
        Entry::PlanProposal {
            item_id,
            title,
            content,
            state,
        } => Some(TranscriptItem::Plan {
            id: item_id.clone(),
            title: title.clone(),
            content: content.clone(),
            state: state.as_str().to_owned(),
        }),
        Entry::Notice { level, title, body } => Some(TranscriptItem::Notice {
            id: format!("n{seq}"),
            level: level.clone(),
            title: title.clone(),
            body: body.clone(),
        }),
        Entry::Advisory {
            item_id,
            severity,
            note,
            delivered,
        } => Some(TranscriptItem::Advisory {
            id: item_id.clone(),
            severity: severity.as_str().to_owned(),
            note: note.clone(),
            delivered: *delivered,
        }),
    }
}

/// Every visible item in journal order. A re-appended plan or advisory
/// replaces its earlier copy in place, so a card keeps its position.
pub fn project_items(entries: &[(u64, Entry)]) -> Vec<TranscriptItem> {
    let tools = index_tool_calls(entries);
    let mut items: Vec<TranscriptItem> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for (seq, entry) in entries {
        let Some(item) = entry_item(*seq, entry, &tools) else {
            continue;
        };
        let id = item_id(&item).to_owned();
        if let Some(&position) = positions.get(&id) {
            items[position] = item;
        } else {
            positions.insert(id, items.len());
            items.push(item);
        }
    }
    items
}

pub fn item_id(item: &TranscriptItem) -> &str {
    match item {
        TranscriptItem::User { id, .. }
        | TranscriptItem::Assistant { id, .. }
        | TranscriptItem::Tool { id, .. }
        | TranscriptItem::Notice { id, .. }
        | TranscriptItem::Plan { id, .. }
        | TranscriptItem::Advisory { id, .. } => id,
    }
}

/// Token and cost totals of every assistant entry plus out-of-band usage
/// (advisor reviews, compaction summaries).
pub fn usage_totals(
    catalog: &Catalog,
    entries: &[(u64, Entry)],
    extra: &UsageTotals,
) -> UsageTotals {
    let mut totals = extra.clone();
    for (_, entry) in entries {
        if let Entry::Assistant {
            usage,
            model: Some(model),
            ..
        } = entry
        {
            add_usage(
                &mut totals,
                catalog,
                &model.provider,
                &model.model_id,
                usage,
            );
        }
    }
    totals
}

pub fn add_usage(
    totals: &mut UsageTotals,
    catalog: &Catalog,
    provider: &str,
    model_id: &str,
    usage: &Usage,
) {
    totals.input_tokens = totals
        .input_tokens
        .saturating_add(usage.input + usage.cache_read + usage.cache_write);
    totals.output_tokens = totals.output_tokens.saturating_add(usage.output);
    if let Some(model) = catalog.get(provider, model_id) {
        totals.cost_usd += model.cost_usd(usage);
    }
}

pub fn transcript(
    record: &ConversationRecord,
    entries: &[(u64, Entry)],
    usage: UsageTotals,
) -> Transcript {
    Transcript {
        items: project_items(entries),
        run_state: record.run_state,
        error: record.error.clone(),
        model: record.model.clone(),
        thinking_level: Some(record.thinking_level.clone()),
        usage,
        mode: Some(record.mode.as_str().to_owned()),
    }
}
