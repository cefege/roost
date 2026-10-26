//! Committed transcript derivation for model context (port of
//! `packages/durable/src/harness/context.ts` @ `abe508e1b`).
//!
//! The active context is fixed by a *head marker*: the newest visible entry that
//! carries a `head`, or the transcript start. Within that range, per target the
//! newest edit wins, and assistant messages that ended `aborted`/`error`/
//! `deferred` contribute no messages. The derived view also orders each
//! assistant's tool results directly after it, synthesizing a missing result.
//!
//! Rust shape note (registered V17-14 §7.3): upstream `freezeJson()` deep-freezes
//! retained records; the Rust port returns owned clones, so retained ranges are
//! frozen by construction and need no explicit freezing.

use std::collections::BTreeMap;
use std::sync::Arc;

use rpi_ai::types::{
    AssistantContent, Message, StopReason, TextContent, ToolCall, ToolResultContent,
    ToolResultMessage, ToolResultRole,
};

use crate::chord::context::Context;
use crate::ids::{ConversationId, EntryId, id_from_number};
use crate::session::{SessionError, SessionImpl, SessionResult};
use crate::types::{ContextEdit, ContextEditAction, EntryQuery, EntryRecord, Storage};

use crate::harness::types::ContextView;

const SCAN_PAGE_SIZE: usize = 256;

/// Synthesized tool result text when history ends before a call completed.
pub const MISSING_RESULT_TEXT: &str =
    "Tool result unavailable: history ends before this call completed.";

/// Stop reasons whose assistant messages are excluded from model context.
pub fn is_excluded_stop_reason(reason: StopReason) -> bool {
    matches!(
        reason,
        StopReason::Aborted | StopReason::Error | StopReason::Deferred
    )
}

/// Head marker and newest visible entry that fix one committed context range.
#[derive(Debug, Clone)]
pub struct ContextBounds {
    /// Newest visible head marker.
    pub head: Option<EntryRecord>,
    /// Newest visible entry of the range.
    pub tail: EntryId,
}

/// One context read: the visible entries it scanned (oldest first) and the view
/// derived from them.
#[derive(Debug, Clone)]
pub struct ContextRange {
    /// Range bounds.
    pub bounds: ContextBounds,
    /// Visible entries from the head marker's head (or transcript start) through
    /// the tail.
    pub entries: Vec<EntryRecord>,
    /// Derived view.
    pub view: ContextView,
    /// Targets of the edits in `entries`.
    pub edited: std::collections::BTreeSet<EntryId>,
    /// `view.messages` from the contributed messages before the last assistant
    /// message (tool results ordered).
    pub settled: Vec<Message>,
    /// Contributed messages from the last assistant message on, before tool
    /// result ordering.
    pub open: Vec<Message>,
}

/// Capture the bounds of the current context, or of the context cut off at the
/// visible entry `at`. Run this on the session line; entries at or below the tail
/// are immutable, so [`derive_context`] can scan them off the line.
pub async fn capture_context_bounds(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    context: &Context,
    at: Option<EntryId>,
) -> SessionResult<Option<ContextBounds>> {
    let tail = match at {
        None => {
            let page = storage
                .scan_entries(
                    EntryQuery {
                        conversation_id,
                        min_entry_id: None,
                        max_entry_id: None,
                        order: None,
                    },
                    1,
                    None,
                    context,
                )
                .await?;
            match page.items.first() {
                Some(entry) => entry.id,
                None => return Ok(None),
            }
        }
        Some(at) => {
            if storage
                .entry_in_conversation(conversation_id, at, context)
                .await?
                .is_none()
            {
                return Err(SessionError::message(format!(
                    "Entry {at} is not visible from conversation {conversation_id}"
                )));
            }
            at
        }
    };
    let head = storage
        .find_latest_head_marker(conversation_id, Some(tail), context)
        .await?;
    Ok(Some(ContextBounds { head, tail }))
}

/// Committed context of one conversation: bounds captured on the session line,
/// entries derived off it.
pub async fn read_context(
    session: &Arc<SessionImpl>,
    storage: &Arc<dyn Storage>,
    conversation_id: ConversationId,
    context: &Context,
    at: Option<EntryId>,
) -> SessionResult<ContextView> {
    let bounds = read_bounds_on_line(session, storage, conversation_id, context, at).await?;
    derive_context(storage.as_ref(), conversation_id, bounds, context).await
}

async fn read_bounds_on_line(
    session: &Arc<SessionImpl>,
    storage: &Arc<dyn Storage>,
    conversation_id: ConversationId,
    context: &Context,
    at: Option<EntryId>,
) -> SessionResult<Option<ContextBounds>> {
    let storage = Arc::clone(storage);
    let read_context = context.clone();
    session
        .read_on_line(move || {
            let storage = Arc::clone(&storage);
            let read_context = read_context.clone();
            Box::pin(async move {
                capture_context_bounds(storage.as_ref(), conversation_id, &read_context, at).await
            })
        })
        .await
}

/// `read_context()` that reuses `previous`, an earlier range of the same
/// conversation: with the same head marker, only entries after its tail are
/// scanned. Returns the view and the range to pass to the next read.
pub async fn read_context_from(
    session: &Arc<SessionImpl>,
    storage: &Arc<dyn Storage>,
    conversation_id: ConversationId,
    context: &Context,
    at: Option<EntryId>,
    previous: Option<ContextRange>,
) -> SessionResult<(ContextView, Option<ContextRange>)> {
    let bounds = read_bounds_on_line(session, storage, conversation_id, context, at).await?;
    let Some(bounds) = bounds else {
        return Ok((empty_view(), None));
    };
    let range = match previous {
        None => {
            let entries = scan_range(storage.as_ref(), conversation_id, &bounds, context).await?;
            derive_range(bounds, entries)
        }
        Some(previous)
            if previous.bounds.head.as_ref().map(|h| h.id)
                != bounds.head.as_ref().map(|h| h.id) =>
        {
            let entries = scan_range(storage.as_ref(), conversation_id, &bounds, context).await?;
            derive_range(bounds, entries)
        }
        Some(previous) if bounds.tail == previous.bounds.tail => previous,
        Some(previous) if bounds.tail < previous.bounds.tail => {
            let entries: Vec<EntryRecord> = previous
                .entries
                .iter()
                .filter(|entry| entry.id <= bounds.tail)
                .cloned()
                .collect();
            derive_range(bounds, entries)
        }
        Some(previous) => {
            let min_entry_id = id_from_number::<crate::ids::Entry>(previous.bounds.tail.get() + 1);
            let query = EntryQuery {
                conversation_id,
                min_entry_id: Some(min_entry_id),
                max_entry_id: Some(bounds.tail),
                order: None,
            };
            let added = scan_range_query(storage.as_ref(), query, context).await?;
            extend_range(previous, bounds, added)
        }
    };
    let view = ContextView {
        head: range.view.head.clone(),
        entries: range.view.entries.clone(),
        contributions: range.view.contributions.clone(),
        messages: range.view.messages.clone(),
    };
    Ok((view, Some(range)))
}

/// Derive the active transcript and model context of one conversation within
/// captured bounds.
pub async fn derive_context(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: Option<ContextBounds>,
    context: &Context,
) -> SessionResult<ContextView> {
    let Some(bounds) = bounds else {
        return Ok(empty_view());
    };
    let entries = scan_range(storage, conversation_id, &bounds, context).await?;
    Ok(derive_range(bounds, entries).view)
}

/// The raw active entries within captured bounds, without deriving model context.
pub async fn active_entries(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: Option<ContextBounds>,
    context: &Context,
) -> SessionResult<Vec<EntryRecord>> {
    let Some(bounds) = bounds else {
        return Ok(Vec::new());
    };
    let entries = scan_range(storage, conversation_id, &bounds, context).await?;
    Ok(select_active(bounds.head.as_ref(), &entries))
}

fn empty_view() -> ContextView {
    ContextView {
        head: None,
        entries: Vec::new(),
        contributions: Vec::new(),
        messages: Vec::new(),
    }
}

fn derive_range(bounds: ContextBounds, entries: Vec<EntryRecord>) -> ContextRange {
    // FORM: upstream `freezeJson()` deep-freezes retained records; owned clones
    // make the Rust range frozen by construction.
    let mut edits: BTreeMap<EntryId, ContextEdit> = BTreeMap::new();
    // Edits of every entry in the range count, including older head markers that
    // `select_active()` drops.
    for entry in &entries {
        for edit in entry.edits.iter().flatten() {
            edits.insert(edit.target, edit.clone());
        }
    }
    let active = select_active(bounds.head.as_ref(), &entries);
    let contributions: Vec<Vec<Message>> = active
        .iter()
        .map(|entry| contribute(entry, edits.get(&entry.id)))
        .collect();
    let flat: Vec<Message> = contributions.iter().flatten().cloned().collect();
    let (settled, open) = settle(Vec::new(), flat);
    let mut ordered = settled.clone();
    ordered.extend(order_tool_results(&open));
    let messages = lead_with_system(ordered);
    let view = ContextView {
        head: bounds.head.clone(),
        entries: active,
        contributions,
        messages,
    };
    ContextRange {
        bounds,
        entries,
        view,
        edited: edits.keys().copied().collect(),
        settled,
        open,
    }
}

fn extend_range(
    previous: ContextRange,
    bounds: ContextBounds,
    added: Vec<EntryRecord>,
) -> ContextRange {
    let mut entries = previous.entries.clone();
    entries.extend(added.iter().cloned());
    if added.iter().any(|entry| {
        entry.edits.is_some() || entry.head.is_some() || previous.edited.contains(&entry.id)
    }) {
        return derive_range(bounds, entries);
    }
    let contributions: Vec<Vec<Message>> =
        added.iter().map(|entry| contribute(entry, None)).collect();
    let mut flat = previous.open.clone();
    for contribution in &contributions {
        flat.extend(contribution.iter().cloned());
    }
    let (settled, open) = settle(previous.settled.clone(), flat);
    let mut view_entries = previous.view.entries.clone();
    view_entries.extend(added.iter().cloned());
    let mut view_contributions = previous.view.contributions.clone();
    view_contributions.extend(contributions.clone());
    let mut ordered = settled.clone();
    ordered.extend(order_tool_results(&open));
    let view = ContextView {
        head: bounds.head.clone(),
        entries: view_entries,
        contributions: view_contributions,
        messages: lead_with_system(ordered),
    };
    ContextRange {
        bounds,
        entries,
        view,
        edited: previous.edited,
        settled,
        open,
    }
}

/// Move a system message that only user messages precede to the front.
fn lead_with_system(messages: Vec<Message>) -> Vec<Message> {
    let index = messages
        .iter()
        .position(|message| message.role() != rpi_ai::types::Role::User);
    let Some(index) = index else {
        return messages;
    };
    if index == 0 || messages[index].role() != rpi_ai::types::Role::System {
        return messages;
    }
    let mut ordered = Vec::with_capacity(messages.len());
    ordered.push(messages[index].clone());
    ordered.extend(messages[..index].iter().cloned());
    ordered.extend(messages[index + 1..].iter().cloned());
    ordered
}

/// One active entry's model messages after its edit and excluded stop reasons,
/// before tool result ordering.
fn contribute(entry: &EntryRecord, edit: Option<&ContextEdit>) -> Vec<Message> {
    if let Some(edit) = edit
        && matches!(edit.action, ContextEditAction::Omit)
    {
        return Vec::new();
    }
    let contributed: Vec<Message> = match edit {
        Some(ContextEdit {
            action: ContextEditAction::Replace { messages },
            ..
        }) => messages.clone(),
        _ => entry.model.clone().unwrap_or_default(),
    };
    contributed
        .into_iter()
        .filter(|message| match message {
            Message::Assistant(assistant) => !is_excluded_stop_reason(assistant.stop_reason),
            _ => true,
        })
        .collect()
}

/// Move the ordered messages before the last assistant message of `open` to
/// `settled`.
fn settle(settled: Vec<Message>, open: Vec<Message>) -> (Vec<Message>, Vec<Message>) {
    let last = open
        .iter()
        .rposition(|message| message.role() == rpi_ai::types::Role::Assistant);
    let Some(last) = last else {
        return (settled, open);
    };
    if last == 0 {
        return (settled, open);
    }
    let mut next_settled = settled;
    next_settled.extend(order_tool_results(&open[..last]));
    let next_open = open[last..].to_vec();
    (next_settled, next_open)
}

/// The visible range of `bounds`: from the head marker's head, or transcript
/// start, through the tail.
fn range_query(bounds: &ContextBounds, conversation_id: ConversationId) -> EntryQuery {
    EntryQuery {
        conversation_id,
        min_entry_id: bounds.head.as_ref().and_then(|head| head.head),
        max_entry_id: Some(bounds.tail),
        order: None,
    }
}

/// Visible entries within `bounds`, oldest first.
async fn scan_range(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: &ContextBounds,
    context: &Context,
) -> SessionResult<Vec<EntryRecord>> {
    scan_range_query(storage, range_query(bounds, conversation_id), context).await
}

async fn scan_range_query(
    storage: &dyn Storage,
    query: EntryQuery,
    context: &Context,
) -> SessionResult<Vec<EntryRecord>> {
    let mut entries: Vec<EntryRecord> = Vec::new();
    let mut cursor = None;
    loop {
        let page = storage
            .scan_entries(query.clone(), SCAN_PAGE_SIZE, cursor, context)
            .await?;
        entries.extend(page.items);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    entries.reverse();
    Ok(entries)
}

/// The head marker followed by the range's non-head entries, or the whole range
/// without a marker.
fn select_active<'a>(head: Option<&'a EntryRecord>, range: &'a [EntryRecord]) -> Vec<EntryRecord> {
    match head {
        None => range.to_vec(),
        Some(head) => {
            let mut active = Vec::with_capacity(range.len() + 1);
            active.push(head.clone());
            active.extend(range.iter().filter(|entry| entry.head.is_none()).cloned());
            active
        }
    }
}

/// Place each assistant's tool results directly after it in call order. Results
/// are taken from the messages before the next assistant; a missing result is
/// synthesized and unmatched results are dropped.
pub fn order_tool_results(messages: &[Message]) -> Vec<Message> {
    let mut ordered: Vec<Message> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if message.role() == rpi_ai::types::Role::ToolResult {
            continue;
        }
        ordered.push(message.clone());
        let Message::Assistant(assistant) = message else {
            continue;
        };
        let calls: Vec<ToolCall> = assistant
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            continue;
        }
        let mut results: BTreeMap<String, usize> = BTreeMap::new();
        let mut next = index + 1;
        while next < messages.len() && messages[next].role() != rpi_ai::types::Role::Assistant {
            let candidate = &messages[next];
            if let Message::ToolResult(result) = candidate {
                results.entry(result.tool_call_id.clone()).or_insert(next);
            }
            next += 1;
        }
        for call in calls {
            match results.get(&call.id) {
                Some(result_index) => ordered.push(messages[*result_index].clone()),
                None => ordered.push(missing_result(&call, assistant.timestamp)),
            }
        }
    }
    ordered
}

fn missing_result(call: &ToolCall, timestamp: i64) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: vec![ToolResultContent::Text(TextContent {
            text: MISSING_RESULT_TEXT.to_string(),
            text_signature: None,
        })],
        details: Some(serde_json::json!({ "reason": "missing_result" })),
        usage: None,
        nested_calls: None,
        is_error: true,
        timestamp,
        duration_ms: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::context::BACKGROUND_CONTEXT;
    use crate::session::{ConversationOwnership, EntryDraft, commit_fn, create_session};
    use crate::storage::MemoryStorage;

    fn user(text: &str, timestamp: i64) -> Message {
        Message::User(rpi_ai::types::UserMessage {
            role: rpi_ai::types::UserRole::User,
            content: rpi_ai::types::UserContent::Text(text.to_string()),
            timestamp,
        })
    }

    fn assistant(
        content: Vec<AssistantContent>,
        stop_reason: StopReason,
        timestamp: i64,
    ) -> Message {
        let value = serde_json::json!({
            "role": "assistant",
            "content": serde_json::to_value(&content).expect("content"),
            "api": "openai",
            "provider": "p",
            "model": "m",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 }
            },
            "stopReason": stop_reason,
            "timestamp": timestamp,
        });
        Message::Assistant(serde_json::from_value(value).expect("assistant"))
    }

    fn call(id: &str) -> AssistantContent {
        AssistantContent::ToolCall(rpi_ai::types::ToolCall {
            id: id.to_string(),
            name: "read".to_string(),
            arguments: serde_json::Map::new(),
            thought_signature: None,
            namespace: None,
        })
    }

    fn result(id: &str, timestamp: i64) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: id.to_string(),
            tool_name: "read".to_string(),
            content: vec![],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp,
            duration_ms: None,
        })
    }

    #[test]
    fn order_tool_results_places_results_in_call_order() {
        let messages = vec![
            user("go", 1),
            assistant(vec![call("b"), call("a")], StopReason::ToolUse, 2),
            result("a", 3),
            result("b", 4),
        ];
        let ordered = order_tool_results(&messages);
        assert_eq!(ordered.len(), 4);
        assert_eq!(ordered[0].role(), rpi_ai::types::Role::User);
        assert_eq!(ordered[1].role(), rpi_ai::types::Role::Assistant);
        match (&ordered[2], &ordered[3]) {
            (Message::ToolResult(first), Message::ToolResult(second)) => {
                assert_eq!(first.tool_call_id, "b");
                assert_eq!(second.tool_call_id, "a");
            }
            _ => panic!("expected tool results"),
        }
    }

    #[test]
    fn order_tool_results_synthesizes_a_missing_result() {
        let ordered = order_tool_results(&[assistant(vec![call("call")], StopReason::ToolUse, 9)]);
        assert_eq!(ordered.len(), 2);
        match &ordered[1] {
            Message::ToolResult(result) => {
                assert!(result.is_error);
                assert_eq!(
                    result.details,
                    Some(serde_json::json!({ "reason": "missing_result" }))
                );
                assert_eq!(result.tool_call_id, "call");
            }
            _ => panic!("expected synthesized result"),
        }
    }

    #[test]
    fn lead_with_system_moves_a_later_system_message_to_the_front() {
        let system = Message::System(
            serde_json::from_value(
                serde_json::json!({ "role": "system", "content": "base", "timestamp": 3 }),
            )
            .expect("system"),
        );
        let messages = vec![user("a", 1), user("b", 2), system.clone()];
        let ordered = lead_with_system(messages);
        assert_eq!(ordered[0].role(), rpi_ai::types::Role::System);
        assert_eq!(ordered.len(), 3);
    }

    #[tokio::test]
    async fn derive_context_excludes_aborted_assistant_messages() {
        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let context = BACKGROUND_CONTEXT.clone();
        let conversation = session
            .commit(
                commit_fn(move |tx| {
                    Box::pin(async move {
                        let record = tx
                            .create_conversation(ConversationOwnership::Ownerless)
                            .await?;
                        let mut user_entry = EntryDraft::new("pi.user");
                        user_entry.model = Some(vec![user("hi", 1)]);
                        tx.append_entry(record.id, user_entry).await?;
                        let mut aborted = EntryDraft::new("pi.assistant");
                        aborted.model = Some(vec![assistant(vec![], StopReason::Aborted, 2)]);
                        tx.append_entry(record.id, aborted).await?;
                        let mut answer = EntryDraft::new("pi.assistant");
                        answer.model = Some(vec![assistant(vec![], StopReason::Stop, 3)]);
                        tx.append_entry(record.id, answer).await?;
                        Ok(record.id)
                    })
                }),
                &context,
            )
            .await
            .expect("commit");
        let storage = Arc::clone(session.storage());
        let view = read_context(&session, &storage, conversation, &context, None)
            .await
            .expect("context");
        // The aborted assistant message is dropped; the stopped one is kept.
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[0].role(), rpi_ai::types::Role::User);
        assert_eq!(view.messages[1].role(), rpi_ai::types::Role::Assistant);
        assert_eq!(view.entries.len(), 3);
    }
}
