//! Submission inbox and boundary placement (port of
//! `packages/durable/src/harness/inbox.ts` @ `abe508e1b`).
//!
//! The `pi.inbox` document is one conversation's ordered queue of submissions
//! waiting for a boundary. A *boundary* is prepared (table reads) at the start
//! of a commit and applied at `postTools` or `final`: every queued write is
//! placed first, then the selected steers, and at `final` the selected
//! follow-ups. A queued reset (`head: "self"`) turns a `postTools` boundary into
//! a `final` one, so user items queued before it run in the new context.
//!
//! Rust shape note (registered V17-14 §7.3): upstream mutates the `Draft<T>`
//! inbox proxy in place; the Rust port reads the `DocDraft`, mutates an owned
//! `Vec`, and writes it back (`chord` still emits leaf-level ops).

use std::sync::{Arc, LazyLock};

use rpi_ai::types::{Message, UserContent, UserMessage, UserRole};
use serde::{Deserialize, Serialize};

use crate::chord::json::{JsonObject, JsonValue};
use crate::documents::{DocDefinition, DocScope, DocToken, define_doc};
use crate::entries::USER_ENTRY;
use crate::harness::types::{QueueMode, read_state, write_state};
use crate::ids::{ConversationId, Entry, EntryId, SubmissionId, id_from_number};
use crate::session::{
    DocDraft, DocOwner, EntryDraft, EntryHead, SessionError, SessionResult, SubmissionSettlement,
    Transaction,
};
use crate::types::{ContextEdit, DocumentFork, DocumentHistory};

/// One queued submission: user input for a run, or a passive entry write.
///
/// The queued write's `entry` is an [`EntryDraft`] stored as plain JSON
/// (`entry_to_json`/`entry_from_json` round-trip it).
// FORM: upstream is a discriminated union with a `mode` discriminant; Rust uses
// a serde-tagged enum preserving the same wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode")]
pub enum InboxItem {
    /// User input inserted at the next `postTools` boundary.
    #[serde(rename = "steer")]
    Steer {
        /// Queued submission.
        id: SubmissionId,
        /// User input content.
        content: UserContent,
    },
    /// User input queued for the next run's `final` boundary.
    #[serde(rename = "followUp")]
    FollowUp {
        /// Queued submission.
        id: SubmissionId,
        /// User input content.
        content: UserContent,
    },
    /// Passive entry write, placed at any boundary.
    #[serde(rename = "write")]
    Write {
        /// Queued submission.
        id: SubmissionId,
        /// The entry draft, stored as plain JSON.
        entry: JsonObject,
    },
}

impl InboxItem {
    /// Queued submission identity.
    pub fn id(&self) -> SubmissionId {
        match self {
            InboxItem::Steer { id, .. }
            | InboxItem::FollowUp { id, .. }
            | InboxItem::Write { id, .. } => *id,
        }
    }

    /// Wire mode (`steer`/`followUp`/`write`).
    pub fn mode(&self) -> &'static str {
        match self {
            InboxItem::Steer { .. } => "steer",
            InboxItem::FollowUp { .. } => "followUp",
            InboxItem::Write { .. } => "write",
        }
    }
}

/// Built-in queue of one conversation's submissions waiting for a boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InboxState {
    /// Queued items in ID order.
    pub items: Vec<InboxItem>,
}

fn initial() -> JsonObject {
    match serde_json::to_value(InboxState::default()) {
        Ok(JsonValue::Object(object)) => object,
        // Invariant: an `InboxState` always serializes to a JSON object.
        _ => JsonObject::new(),
    }
}

/// Built-in inbox document (`pi.inbox`), conversation scope, latest history,
/// initial fork, emptied when every item is placed.
pub static INBOX_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.inbox".to_string(),
        version: 1,
        scope: DocScope::Conversation,
        history: Some(DocumentHistory::Latest),
        fork: Some(DocumentFork::Initial),
        family: false,
        initial: Arc::new(|_seed| initial()),
        migrate: None,
        checkpoint_when: Some(Arc::new(|value, _ops, _revision| {
            value
                .get("items")
                .and_then(JsonValue::as_array)
                .map(|items| items.is_empty())
                .unwrap_or(true)
        })),
    })
});

/// The settings a boundary reads, on the Session line.
#[derive(Debug, Clone, Copy)]
pub struct QueueModes {
    /// Steering queue mode.
    pub steering_mode: QueueMode,
    /// Follow-up queue mode.
    pub follow_up_mode: QueueMode,
}

/// What a boundary reads before the commit's first table write, and the newest
/// head it has seen so far.
pub struct Boundary {
    /// Conversation the boundary applies to.
    pub conversation_id: ConversationId,
    /// Inbox draft, mutated by [`apply_boundary`].
    pub inbox: DocDraft,
    /// Steering queue mode read on the line.
    pub steering_mode: QueueMode,
    /// Follow-up queue mode read on the line.
    pub follow_up_mode: QueueMode,
    /// Start of the active range; advanced by heads written in this commit.
    pub head: Option<EntryId>,
}

/// Which boundary is being applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryAt {
    /// The boundary after a tool round: steers only.
    PostTools,
    /// The boundary at the end of a run: steers and follow-ups.
    Final,
}

/// Selected user items, in ID order, and whether a `head: "self"` write was
/// placed.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryResult {
    /// Placed user submission IDs.
    pub users: Vec<SubmissionId>,
    /// Whether a reset was placed.
    pub reset: bool,
}

/// Read what a boundary needs. Table reads must precede the commit's first
/// table write, so callers prepare the boundary at the start of their commit.
pub async fn prepare_boundary(
    tx: &mut Transaction,
    conversation_id: ConversationId,
    modes: QueueModes,
) -> SessionResult<Boundary> {
    let head = tx
        .latest_head_marker(conversation_id)
        .await?
        .and_then(|entry| entry.head);
    let inbox = tx
        .doc(&INBOX_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    Ok(Boundary {
        conversation_id,
        inbox,
        steering_mode: modes.steering_mode,
        follow_up_mode: modes.follow_up_mode,
        head,
    })
}

/// Place the queued items a boundary selects: every write, the first or all
/// steers, and at `final` the first or all follow-ups. A selected reset turns a
/// `postTools` boundary into `final`. Writes are placed first and user items
/// after them, each in ID order, so user items queued before a reset run in the
/// new context. A write whose head targets an entry before the active range,
/// including a range started earlier in this commit, is stale. Selected and
/// stale items are removed positionally.
pub async fn apply_boundary(
    tx: &mut Transaction,
    mut boundary: Boundary,
    at: BoundaryAt,
    now: f64,
) -> SessionResult<BoundaryResult> {
    let state: InboxState = read_state(&boundary.inbox)?;
    let mut items = state.items;
    let reset = items.iter().any(|item| match item {
        InboxItem::Write { entry, .. } => {
            entry.get("head").and_then(JsonValue::as_str) == Some("self")
        }
        _ => false,
    });
    let final_at = at == BoundaryAt::Final || reset;
    let pick = |mode: &str, queue_mode: QueueMode| -> Vec<usize> {
        let indexes: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.mode() == mode)
            .map(|(index, _)| index)
            .collect();
        if queue_mode == QueueMode::All {
            indexes
        } else {
            indexes.into_iter().take(1).collect()
        }
    };
    let writes: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item, InboxItem::Write { .. }))
        .map(|(index, _)| index)
        .collect();
    let mut users = pick("steer", boundary.steering_mode);
    if final_at {
        users.extend(pick("followUp", boundary.follow_up_mode));
    }
    users.sort_unstable();

    // `append_entry()` copies the drafts' values; the items are removed only afterwards.
    for index in &writes {
        let (id, entry_json) = match &items[*index] {
            InboxItem::Write { id, entry } => (*id, entry.clone()),
            // Invariant: `writes` only collects write-mode indexes.
            _ => continue,
        };
        let draft = entry_from_json(&entry_json)?;
        if is_stale(&boundary, &draft) {
            tx.settle_submission(
                id,
                SubmissionSettlement::Unanswered {
                    reason: "stale".to_string(),
                    detail: None,
                },
            )?;
            continue;
        }
        let record = tx
            .append_entry(boundary.conversation_id, draft.clone())
            .await?;
        if let Some(next_head) = draft.head {
            boundary.head = Some(match next_head {
                EntryHead::This => record.id,
                EntryHead::At(entry) => entry,
            });
        }
        tx.place_submission(id, record.id)?;
    }
    let mut placed: Vec<SubmissionId> = Vec::new();
    for index in &users {
        let (id, content) = match &items[*index] {
            InboxItem::Steer { id, content } | InboxItem::FollowUp { id, content } => {
                (*id, content.clone())
            }
            // Invariant: `users` only collects steer/followUp indexes.
            _ => continue,
        };
        let message = Message::User(UserMessage {
            role: UserRole::User,
            content,
            timestamp: now as i64,
        });
        let mut draft = EntryDraft::new(USER_ENTRY);
        draft.model = Some(vec![message]);
        let record = tx.append_entry(boundary.conversation_id, draft).await?;
        tx.place_submission(id, record.id)?;
        placed.push(id);
    }
    let mut removed: Vec<usize> = writes;
    removed.extend(users);
    removed.sort_unstable_by(|a, b| b.cmp(a));
    for index in removed {
        items.remove(index);
    }
    write_state(&boundary.inbox, &InboxState { items })?;
    Ok(BoundaryResult {
        users: placed,
        reset,
    })
}

/// Whether a head write targets an entry before the active range, so placing it
/// would bring back cut history.
pub fn is_stale(boundary: &Boundary, entry: &EntryDraft) -> bool {
    match entry.head {
        Some(EntryHead::At(entry)) => boundary.head.is_some_and(|head| entry < head),
        _ => false,
    }
}

/// Remove a withdrawn submission's item; the caller settles the submission.
pub async fn remove_inbox_item(
    tx: &mut Transaction,
    conversation_id: ConversationId,
    id: SubmissionId,
) -> SessionResult<()> {
    let draft = tx
        .doc(&INBOX_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    let mut state: InboxState = read_state(&draft)?;
    state.items.retain(|item| item.id() != id);
    write_state(&draft, &state)
}

/// Withdraw every queued input of a conversation, as `Conversation.abort()` and
/// abort cascades do: each settles `unanswered` with `aborted` and leaves the
/// inbox; queued writes stay for later placement.
pub async fn withdraw_queued_inputs(
    tx: &mut Transaction,
    conversation_id: ConversationId,
) -> SessionResult<()> {
    let draft = tx
        .doc(&INBOX_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    let state: InboxState = read_state(&draft)?;
    let mut kept: Vec<InboxItem> = Vec::new();
    for item in state.items {
        match item {
            InboxItem::Write { .. } => kept.push(item),
            InboxItem::Steer { id, .. } | InboxItem::FollowUp { id, .. } => {
                tx.settle_submission(
                    id,
                    SubmissionSettlement::Unanswered {
                        reason: "aborted".to_string(),
                        detail: None,
                    },
                )?;
            }
        }
    }
    write_state(&draft, &InboxState { items: kept })
}

// ---------------------------------------------------------------------------
// EntryDraft <-> JSON (inbox write entries)
// ---------------------------------------------------------------------------

/// Serialize an [`EntryDraft`] into the plain JSON stored by a queued write
/// (upstream `copyJson(draft.entry, { omitUndefinedProperties: true })`).
pub fn entry_to_json(draft: &EntryDraft) -> JsonObject {
    let mut object = JsonObject::new();
    object.insert("kind".to_string(), JsonValue::String(draft.kind.clone()));
    if let Some(model) = &draft.model {
        // Invariant: `Vec<Message>` always serializes.
        object.insert(
            "model".to_string(),
            serde_json::to_value(model).unwrap_or(JsonValue::Null),
        );
    }
    if let Some(data) = &draft.data {
        object.insert("data".to_string(), data.clone());
    }
    if let Some(head) = draft.head {
        object.insert(
            "head".to_string(),
            match head {
                EntryHead::This => JsonValue::String("self".to_string()),
                EntryHead::At(entry) => JsonValue::from(entry.get()),
            },
        );
    }
    if let Some(edits) = &draft.edits {
        // Invariant: `Vec<ContextEdit>` always serializes.
        object.insert(
            "edits".to_string(),
            serde_json::to_value(edits).unwrap_or(JsonValue::Null),
        );
    }
    object
}

/// Deserialize the plain JSON of a queued write back into an [`EntryDraft`].
pub fn entry_from_json(object: &JsonObject) -> SessionResult<EntryDraft> {
    let kind = object
        .get("kind")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| SessionError::message("Inbox write is missing its entry kind"))?
        .to_string();
    let model = match object.get("model") {
        Some(value) => Some(
            serde_json::from_value(value.clone())
                .map_err(|error| SessionError::message(error.to_string()))?,
        ),
        None => None,
    };
    let edits = match object.get("edits") {
        Some(value) => Some(
            serde_json::from_value::<Vec<ContextEdit>>(value.clone())
                .map_err(|error| SessionError::message(error.to_string()))?,
        ),
        None => None,
    };
    let head = match object.get("head") {
        Some(JsonValue::String(value)) if value == "self" => Some(EntryHead::This),
        Some(JsonValue::Number(number)) => number
            .as_u64()
            .map(|value| EntryHead::At(id_from_number::<Entry>(value))),
        _ => None,
    };
    Ok(EntryDraft {
        kind,
        model,
        data: object.get("data").cloned(),
        head,
        edits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Submission;

    #[test]
    fn inbox_state_round_trips_its_items() {
        let state = InboxState {
            items: vec![
                InboxItem::Write {
                    id: id_from_number::<Submission>(1),
                    entry: entry_to_json(&EntryDraft::new("pi.reset")),
                },
                InboxItem::Steer {
                    id: id_from_number::<Submission>(2),
                    content: UserContent::Text("go".to_string()),
                },
                InboxItem::FollowUp {
                    id: id_from_number::<Submission>(3),
                    content: UserContent::Text("next".to_string()),
                },
            ],
        };
        let value = serde_json::to_value(&state).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "items": [
                    { "id": 1, "mode": "write", "entry": { "kind": "pi.reset" } },
                    { "id": 2, "mode": "steer", "content": "go" },
                    { "id": 3, "mode": "followUp", "content": "next" },
                ]
            })
        );
        let parsed: InboxState = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, state);
    }

    #[test]
    fn entry_json_round_trips_head_and_model() {
        let mut draft = EntryDraft::new("pi.reset");
        draft.head = Some(EntryHead::This);
        draft.model = Some(vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("handoff".to_string()),
            timestamp: 5,
        })]);
        let object = entry_to_json(&draft);
        assert_eq!(
            object.get("head"),
            Some(&JsonValue::String("self".to_string()))
        );
        let parsed = entry_from_json(&object).expect("parse");
        assert_eq!(parsed.kind, "pi.reset");
        assert!(matches!(parsed.head, Some(EntryHead::This)));
        assert_eq!(parsed.model.as_ref().map(Vec::len), Some(1));

        let mut at = EntryDraft::new("pi.user");
        at.head = Some(EntryHead::At(id_from_number::<Entry>(9)));
        let parsed = entry_from_json(&entry_to_json(&at)).expect("parse");
        assert!(matches!(parsed.head, Some(EntryHead::At(id)) if id == id_from_number::<Entry>(9)));
    }
}
