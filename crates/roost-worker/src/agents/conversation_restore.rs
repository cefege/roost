//! The pinned OMP conversation-resume plan, its single acknowledged PTY write
//! after an ordinary replacement shell is durably admitted, and the line-discard
//! write that cancels a partly delivered resume command. Called by the boot
//! reconcile (`runtime::session_reconcile`) behind `WorkerBoot::agent_conversation_restore`;
//! it supplies only private recovery metadata and a live session. Ports
//! `apps/worker/src/agents/agent-conversation-restore.ts`.

use std::collections::HashSet;

use roost_platform::{HostPlatform, posix_shell_quote};
use roost_protocol::agent_conversation_reference::{
    AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use roost_protocol::wire::brand::SessionId;

use crate::session::input_write::WorkerInputResult;
use crate::session::lifecycle::SessionManager;

const CARRIAGE_RETURN: &str = "\r";
/// ETX discards the current input line in bash and zsh, in both emacs and vi
/// editing modes, so a partially delivered resume command cannot stay on the
/// prompt where one Enter would run a truncated `--resume=` id prefix.
const PARTIAL_INPUT_DISCARD_BYTES: [u8; 1] = [0x03];

/// The one resume plan Roost will execute for an agent. `omp` exposes resume
/// as `-r, --resume=<value>` and has no `--session` flag, so the reference
/// travels as one `--resume=` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationResumeDescriptor {
    pub schema_version: u32,
    pub agent_id: &'static str,
    pub executable: &'static str,
    pub fixed_option_prefix: &'static str,
    pub reference_kinds: [AgentConversationReferenceKind; 2],
    pub platforms: [HostPlatform; 2],
}

pub const OMP_CONVERSATION_RESUME_DESCRIPTOR_V1: ConversationResumeDescriptor =
    ConversationResumeDescriptor {
        schema_version: 1,
        agent_id: "omp",
        executable: "omp",
        fixed_option_prefix: "--resume=",
        reference_kinds: [
            AgentConversationReferenceKind::Id,
            AgentConversationReferenceKind::Path,
        ],
        platforms: [HostPlatform::MacOs, HostPlatform::Linux],
    };

/// Why no restore input was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreSkip {
    Disabled,
    MissingReference,
    Unsupported,
    Duplicate,
}

impl RestoreSkip {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::MissingReference => "missing_reference",
            Self::Unsupported => "unsupported",
            Self::Duplicate => "duplicate",
        }
    }
}

/// v2 `AgentConversationRestoreOutcome`: one attempted batch, or a skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConversationRestoreOutcome {
    Written(WorkerInputResult),
    Skipped(RestoreSkip),
}

/// What one restore reads. v2 `AgentConversationRestoreDeps`.
pub struct AgentConversationRestoreDeps<'a> {
    /// `WorkerBoot::agent_conversation_restore` (ROOST_AGENT_CONVERSATION_RESTORE).
    pub enabled: bool,
    pub manager: &'a SessionManager,
    pub platform: HostPlatform,
    /// Reference keys already claimed by an earlier session in this pass.
    pub resumed_reference_keys: Option<&'a mut HashSet<String>>,
}

impl std::fmt::Debug for AgentConversationRestoreDeps<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentConversationRestoreDeps")
            .field("enabled", &self.enabled)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

/// Two sessions holding the same reference cannot both resume it: the second
/// agent process would attach to a conversation another one already owns.
pub fn conversation_restore_dedupe_key(reference: &AgentConversationReferenceV1) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        reference.agent_id,
        reference.kind.as_str(),
        reference.value
    )
}

/// Materialize the pinned OMP argv plan as one shell command line. Integration
/// data contributes a single quoted argument and can never supply executable or
/// option text. `None` for a platform or reference the plan does not cover.
pub fn materialize_omp_conversation_restore_input(
    reference: &AgentConversationReferenceV1,
    platform: HostPlatform,
) -> Option<Vec<u8>> {
    let descriptor = OMP_CONVERSATION_RESUME_DESCRIPTOR_V1;
    if !descriptor.platforms.contains(&platform) {
        return None;
    }
    // The reference's own rules: the `omp` literal, the per-kind UTF-8 bound,
    // the control class, and an absolute path for a path reference.
    reference.check().ok()?;
    if reference.agent_id != descriptor.agent_id
        || !descriptor.reference_kinds.contains(&reference.kind)
    {
        return None;
    }
    let argv = [
        descriptor.executable.to_owned(),
        format!("{}{}", descriptor.fixed_option_prefix, reference.value),
    ];
    let command = argv
        .iter()
        .map(|argument| posix_shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("{command}{CARRIAGE_RETURN}").into_bytes())
}

/// Attempt one worker-owned input batch. Every result is terminal for automatic
/// restoration: nothing here retries, respawns, or closes the session.
pub async fn restore_agent_conversation_after_respawn(
    deps: AgentConversationRestoreDeps<'_>,
    session_id: &SessionId,
    reference: Option<&AgentConversationReferenceV1>,
) -> AgentConversationRestoreOutcome {
    let AgentConversationRestoreDeps {
        enabled,
        manager,
        platform,
        mut resumed_reference_keys,
    } = deps;
    if !enabled {
        return recorded(
            session_id,
            AgentConversationRestoreOutcome::Skipped(RestoreSkip::Disabled),
        );
    }
    let Some(reference) = reference else {
        return recorded(
            session_id,
            AgentConversationRestoreOutcome::Skipped(RestoreSkip::MissingReference),
        );
    };
    let dedupe_key = conversation_restore_dedupe_key(reference);
    if resumed_reference_keys
        .as_ref()
        .is_some_and(|keys| keys.contains(&dedupe_key))
    {
        return recorded(
            session_id,
            AgentConversationRestoreOutcome::Skipped(RestoreSkip::Duplicate),
        );
    }
    let Some(payload) = materialize_omp_conversation_restore_input(reference, platform) else {
        return recorded(
            session_id,
            AgentConversationRestoreOutcome::Skipped(RestoreSkip::Unsupported),
        );
    };
    // The claim precedes the write: an ambiguous outcome may still have reached
    // the PTY, so no second session may resume the same conversation.
    if let Some(keys) = resumed_reference_keys.as_deref_mut() {
        keys.insert(dedupe_key.clone());
    }
    let outcome = manager.write_worker_owned_input(session_id, payload).await;
    if let WorkerInputResult::Rejected { .. } = outcome {
        // A rejection is proven pre-write with zero keeper bytes, so no agent
        // process can have attached: another session holding the same reference
        // must still be allowed to resume it in this pass.
        if let Some(keys) = resumed_reference_keys {
            keys.remove(&dedupe_key);
        }
    }
    let partial = match &outcome {
        WorkerInputResult::Ambiguous { written_bytes, .. } => *written_bytes > 0,
        WorkerInputResult::Accepted { .. } | WorkerInputResult::Rejected { .. } => false,
    };
    let result = recorded(
        session_id,
        AgentConversationRestoreOutcome::Written(outcome.clone()),
    );
    if partial {
        discard_partial_restore_input(manager, session_id, &outcome).await;
    }
    result
}

/// Cancel — never re-send — a resume command the keeper only partly delivered.
/// `omp --resume=` matches an id PREFIX, so a truncated command left on the
/// prompt could attach one Enter to a different conversation. Discarding the
/// input line is the only remedy available here: the replacement shell has
/// already emitted a durable `respawned` event, so ending it would tombstone
/// the session and delete its coordinator row over a stray prompt line.
async fn discard_partial_restore_input(
    manager: &SessionManager,
    session_id: &SessionId,
    partial: &WorkerInputResult,
) {
    let discard = manager
        .write_worker_owned_input(session_id, PARTIAL_INPUT_DISCARD_BYTES.to_vec())
        .await;
    let (partial_outcome, partial_bytes) = input_summary(partial);
    let (outcome, _) = input_summary(&discard);
    if let WorkerInputResult::Accepted { .. } = discard {
        tracing::info!(session = %session_id, outcome, partial_outcome, partial_bytes, "agent_conversation_restore_discard_transition");
    } else {
        tracing::warn!(session = %session_id, outcome, partial_outcome, partial_bytes, "agent_conversation_restore_discard_transition");
    }
}

/// One transition line per restore, carrying the outcome and never the
/// reference: its value is a private path or id.
fn recorded(
    session_id: &SessionId,
    outcome: AgentConversationRestoreOutcome,
) -> AgentConversationRestoreOutcome {
    match &outcome {
        AgentConversationRestoreOutcome::Skipped(skip) => {
            tracing::info!(session = %session_id, outcome = "skipped", skip_reason = skip.as_str(), "agent_conversation_restore_transition");
        }
        AgentConversationRestoreOutcome::Written(written @ WorkerInputResult::Accepted { .. }) => {
            tracing::info!(session = %session_id, outcome = input_summary(written).0, "agent_conversation_restore_transition");
        }
        AgentConversationRestoreOutcome::Written(written) => {
            tracing::warn!(session = %session_id, outcome = input_summary(written).0, "agent_conversation_restore_transition");
        }
    }
    outcome
}

fn input_summary(result: &WorkerInputResult) -> (&'static str, u32) {
    match result {
        WorkerInputResult::Accepted { written_bytes } => ("accepted", *written_bytes),
        WorkerInputResult::Rejected { .. } => ("rejected", 0),
        WorkerInputResult::Ambiguous { written_bytes, .. } => ("ambiguous", *written_bytes),
    }
}
