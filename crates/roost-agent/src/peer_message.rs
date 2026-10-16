//! The `message` harness tool: sends text to a peer conversation (the parent,
//! a sibling subagent, or a child) or broadcasts to every visible peer. Visibility,
//! labels and each peer's run state live here; admission and delivery belong to
//! `AgentRuntime::deliver_peer_message`.

use roost_llm::ToolSpec;
use serde::Deserialize;
use serde_json::json;

use crate::error::AgentError;
use crate::prompts;
use crate::records::{ConversationRecord, Entry};
use crate::runtime::AgentRuntime;
use crate::subagents::TITLE_SEPARATOR;
use crate::tool_round::{CallResult, CallScope};
use crate::toolset::TOOL_MESSAGE;
use crate::traits::ToolOutcome;

/// The `to` value that addresses every visible peer.
const BROADCAST_TO: &str = "all";
/// How the main conversation is named to its subagents.
const MAIN_LABEL: &str = "main";

#[derive(Deserialize)]
struct MessageArgs {
    to: String,
    message: String,
}

/// Whether a peer can take a message now. `Running` takes it at its next step
/// boundary. `Idle` is a conversation with no run in progress: the main
/// conversation starts one for a direct message, while a subagent reads its
/// messages when its task starts. `Finished` is a subagent whose run ended, so it
/// will not run again.
enum PeerState {
    Running,
    Idle,
    Finished,
}

/// A conversation the sender may message, with its current run state.
struct Peer {
    record: ConversationRecord,
    label: String,
    state: PeerState,
}

pub(crate) fn message_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_MESSAGE.into(),
        description: prompts::MESSAGE_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "message": {"type": "string"}
            },
            "required": ["to", "message"],
            "additionalProperties": false
        }),
    }
}

pub(crate) async fn run(scope: &CallScope<'_>, args: &str) -> CallResult {
    let parsed = match serde_json::from_str::<MessageArgs>(args) {
        Ok(parsed) if !parsed.to.trim().is_empty() && !parsed.message.trim().is_empty() => parsed,
        Ok(_) => return CallResult::failure("message requires a non-empty `to` and `message`"),
        Err(error) => return CallResult::failure(format!("invalid message arguments: {error}")),
    };
    let records = match scope.runtime.inner.store.conversations().await {
        Ok(records) => records,
        Err(error) => return CallResult::failure(format!("message could not read peers: {error}")),
    };
    let peers = match visible_peers(scope.runtime, scope.record, &records).await {
        Ok(peers) => peers,
        Err(error) => return CallResult::failure(format!("message could not read peers: {error}")),
    };
    let sender_label = label_of(scope.record);
    if parsed.to == BROADCAST_TO {
        return broadcast(
            scope.runtime,
            scope.record,
            &sender_label,
            &peers,
            &parsed.message,
        )
        .await;
    }
    let peer = match resolve_peer(&peers, &parsed.to) {
        Ok(peer) => peer,
        Err(reason) => return CallResult::failure(reason),
    };
    if matches!(peer.state, PeerState::Finished) {
        return CallResult::failure(format!(
            "{} has finished and cannot receive messages.",
            peer.label
        ));
    }
    match deliver(
        scope.runtime,
        scope.record,
        &sender_label,
        peer,
        false,
        &parsed.message,
    )
    .await
    {
        Ok(()) => delivered_result(direct_summary(peer)),
        Err(error) => {
            CallResult::failure(format!("Message not delivered to {}: {error}", peer.label))
        }
    }
}

/// The conversations `me` may message. Its child subagents are always visible.
/// A subagent also sees its parent and the siblings that share that parent; a
/// grandchild is reached through its parent, not directly.
async fn visible_peers(
    runtime: &AgentRuntime,
    me: &ConversationRecord,
    records: &[ConversationRecord],
) -> Result<Vec<Peer>, AgentError> {
    let mut candidates: Vec<&ConversationRecord> = records
        .iter()
        .filter(|record| record.is_child_of(&me.id))
        .collect();
    if let Some(parent_id) = me.parent_id.as_deref() {
        candidates.extend(records.iter().filter(|record| record.id == parent_id));
        candidates.extend(
            records
                .iter()
                .filter(|record| record.id != me.id && record.is_child_of(parent_id)),
        );
    }
    let mut peers = Vec::with_capacity(candidates.len());
    for record in candidates {
        peers.push(Peer {
            label: label_of(record),
            state: peer_state(runtime, record).await?,
            record: record.clone(),
        });
    }
    Ok(peers)
}

/// The latest run intent says whether a conversation is running. Only a subagent
/// can finish: the main conversation is always there to take a message.
async fn peer_state(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
) -> Result<PeerState, AgentError> {
    let entries = runtime.inner.store.entries(&record.id).await?;
    let latest_run_active = entries.iter().rev().find_map(|(_, entry)| match entry {
        Entry::RunIntent { active, .. } => Some(*active),
        _ => None,
    });
    Ok(match latest_run_active {
        Some(true) => PeerState::Running,
        Some(false) if record.parent_id.is_some() => PeerState::Finished,
        _ => PeerState::Idle,
    })
}

fn label_of(record: &ConversationRecord) -> String {
    match record.parent_id {
        None => MAIN_LABEL.to_owned(),
        Some(_) => record
            .title
            .split(TITLE_SEPARATOR)
            .next()
            .unwrap_or(&record.title)
            .trim()
            .to_owned(),
    }
}

/// Matches `to` by conversation id first, then by unique label. An ambiguous
/// label is refused so a message never lands on the wrong sibling.
fn resolve_peer<'a>(peers: &'a [Peer], to: &str) -> Result<&'a Peer, String> {
    if let Some(peer) = peers.iter().find(|peer| peer.record.id == to) {
        return Ok(peer);
    }
    let matches: Vec<&Peer> = peers.iter().filter(|peer| peer.label == to).collect();
    match matches.as_slice() {
        [peer] => Ok(peer),
        [] => Err(format!(
            "No visible peer named `{to}`. Visible peers: {}.",
            describe_peers(peers)
        )),
        _ => Err(format!(
            "`{to}` matches more than one peer; address one by id: {}.",
            describe_peers(peers)
        )),
    }
}

fn describe_peers(peers: &[Peer]) -> String {
    if peers.is_empty() {
        return "none".to_owned();
    }
    peers
        .iter()
        .map(|peer| format!("{} ({})", peer.label, peer.record.id))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A subagent with no run in progress reads a direct message only when its task
/// starts, so the sender is told the message is queued rather than delivered.
fn direct_summary(peer: &Peer) -> String {
    match peer.state {
        PeerState::Idle if peer.record.parent_id.is_some() => format!(
            "Message queued for {}: it is not running and reads it when its task starts.",
            peer.label
        ),
        _ => format!("Message delivered to {}.", peer.label),
    }
}

/// Finished peers are skipped and named in the result: they cannot take a
/// message. A broadcast never wakes an idle peer, so idle peers only buffer it.
async fn broadcast(
    runtime: &AgentRuntime,
    sender: &ConversationRecord,
    sender_label: &str,
    peers: &[Peer],
    text: &str,
) -> CallResult {
    if peers.is_empty() {
        return CallResult::failure(
            "No peers to broadcast to: this conversation has no visible peers.",
        );
    }
    let mut delivered = 0usize;
    let mut undelivered = Vec::new();
    for peer in peers {
        if matches!(peer.state, PeerState::Finished) {
            undelivered.push(format!("{} (finished)", peer.label));
            continue;
        }
        match deliver(runtime, sender, sender_label, peer, true, text).await {
            Ok(()) => delivered += 1,
            Err(error) => undelivered.push(format!("{} ({error})", peer.label)),
        }
    }
    let summary = format!(
        "Broadcast delivered to {delivered} of {} peer(s).",
        peers.len()
    );
    let summary = if undelivered.is_empty() {
        summary
    } else {
        format!("{summary} Not delivered to: {}.", undelivered.join(", "))
    };
    if delivered == 0 {
        return CallResult::failure(summary);
    }
    delivered_result(summary)
}

async fn deliver(
    runtime: &AgentRuntime,
    sender: &ConversationRecord,
    sender_label: &str,
    peer: &Peer,
    broadcast: bool,
    text: &str,
) -> Result<(), AgentError> {
    let entry = Entry::PeerMessage {
        from_id: sender.id.clone(),
        from: sender_label.to_owned(),
        broadcast,
        text: text.to_owned(),
    };
    // A subagent runs only under its task, so a direct message to one waits in its
    // journal; only the main conversation is started for a direct message.
    let wakes_idle = !broadcast && peer.record.parent_id.is_none();
    runtime
        .deliver_peer_message(&peer.record.id, entry, wakes_idle)
        .await?;
    tracing::info!(
        from_id = %sender.id,
        to_id = %peer.record.id,
        broadcast,
        wakes_idle,
        "peer message delivered"
    );
    Ok(())
}

fn delivered_result(content: String) -> CallResult {
    CallResult::plain(ToolOutcome {
        is_error: false,
        content,
        details_json: String::new(),
    })
}
