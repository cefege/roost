//! Execution of the one acknowledged browser layout command.
//!
//! Ported from `apps/web/src/client/ui-state/uiLayoutApplyCore.ts`. The browser
//! shell supplies the tab and socket identity, the active folder, the records,
//! the id source, navigation, and the Sync send; this file decides.
//!
//! THE ORDER IS THE CONTRACT. Every refusal happens BEFORE the single write, so
//! a refused apply leaves the stored bytes exactly as they were -- a partial
//! application is worse than none, because the panes on screen then match no
//! arrangement and the user cannot tell whether the document is wrong or their
//! view is. And once the write has happened there is no path back to a
//! rejection: the commit is the last fallible step, so a host that returns
//! normally always answers APPLIED. v2 needed a `finally` to get that
//! property; here it is the shape of the function.
//!
//! The document arrives already decoded into `roost_protocol::layout`'s shape,
//! because the proto-to-document adapter is ONE adapter shared with the
//! coordinator (`roost_coord::ui_state::layout_proto`), and a second copy here
//! would be a second answer to "what does this wire mean".

use crate::store::layout::{LayoutRecords, PaneIdSource, apply_layout_document};
use roost_protocol::layout::LayoutDocumentV1;

/// The event a settled apply is reported under, with the correlation alone.
pub const LAYOUT_APPLY_SETTLED_EVENT: &str = "ui_cc.layout_apply_settled";

/// How much of a correlation id reaches a diagnostic sink.
///
/// Bounded because the id is echoed straight to a log line, and a peer chooses
/// it. The result frame itself carries the whole id: truncation is for the
/// diagnostic, never for the answer.
pub const LAYOUT_APPLY_DIAGNOSTIC_CORRELATION_MAX_CODE_POINTS: usize = 128;

/// Why this tab refused an apply aimed at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutApplyRejection {
    /// The shell that would have applied the document could not be reached.
    BridgeUnavailable,
    /// This tab is not viewing a live folder, or its membership is not the
    /// fleet's yet.
    NoActiveFolder,
    /// The document is not a valid arrangement for this folder.
    InvalidDocument,
}

impl LayoutApplyRejection {
    /// The text the caller is given. A fixed sentence per arm, never the
    /// validation error: the reason travels to a browser's UI, and a parse
    /// error names a session id.
    pub fn message(self) -> &'static str {
        match self {
            Self::BridgeUnavailable => "The target tab UI bridge is unavailable.",
            Self::NoActiveFolder => "The target tab is not viewing a live folder.",
            Self::InvalidDocument => "The layout document is invalid for the current folder.",
        }
    }
}

impl std::fmt::Display for LayoutApplyRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// The folder an apply may be applied to, as this tab sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutApplyFolder {
    /// The pane-layout bucket the document belongs to.
    pub folder_key: String,
    /// The session this tab is showing.
    pub active_session_id: String,
    /// The folder's live membership, projected.
    pub live_session_ids: Vec<String>,
    /// Whether any of that membership exists only in this browser. An
    /// arrangement computed against a session the fleet has not admitted is not
    /// an arrangement anyone else can read.
    pub has_client_only_session: bool,
}

/// What the target tab answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutApplyOutcome {
    /// The document was applied.
    Applied,
    /// The document was refused, before any mutation.
    Rejected,
}

impl LayoutApplyOutcome {
    /// The wire spelling of this outcome.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }
}

/// One acknowledgement, ready for the Sync socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutApplyResult {
    /// The correlation this tab was addressed by. Echoed whole.
    pub correlation_id: String,
    /// What happened.
    pub outcome: LayoutApplyOutcome,
    /// Why, for a rejection. A fixed sentence, never a validation error.
    pub reason: Option<String>,
}

/// The settlement line for one apply: the correlation and the outcome, and
/// nothing else. A diagnostic sink that could see the document, the session ids
/// or the reason would be a second copy of the arrangement in a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutApplySettlementDiagnostic {
    /// The correlation, bounded.
    pub correlation_id: String,
    /// `"applied"` or `"rejected"`.
    pub outcome: &'static str,
}

/// One decoded apply command, as the host's Sync dispatcher hands it over.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutApplyCommand {
    /// The tab this command is addressed to. Empty is a broadcast, and an
    /// acknowledged apply is never one.
    pub target_tab_id: String,
    /// The exact Sync-v2 socket generation it was reserved on.
    pub target_socket_id: String,
    /// The correlation the answer travels on.
    pub correlation_id: String,
    /// The decoded document. Absent is the wire's "no document", which is a
    /// refusal rather than an empty arrangement.
    pub document: Option<LayoutDocumentV1>,
}

/// The exact tab, socket and correlation an acknowledgement goes back on.
///
/// NOT `fence::LayoutApplyTarget`. That one is what a CALLER composed against
/// and it names a device fingerprint; this one is what this TAB answers on, and
/// it carries the correlation the answer must travel. One name for the two
/// sides of the same wire is how a port ends up with an acknowledgement that
/// names a fingerprint the browser does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactApplyTarget {
    /// The tab the command named, which is this tab.
    pub tab_id: String,
    /// The socket generation it named, which is the one this tab holds.
    pub socket_id: String,
    /// The correlation the answer must travel on.
    pub correlation_id: String,
}

/// The host surface this core calls into, all of it synchronous.
///
/// One method for the two pieces of state the apply writes, because a host
/// cannot hand out two overlapping mutable borrows of itself and the write
/// needs both at once.
pub trait LayoutApplyContext {
    /// The tab this client presents.
    fn current_tab_id(&self) -> &str;
    /// The socket generation this client holds, if it holds one.
    fn current_socket_id(&self) -> Option<&str>;
    /// The folder this tab is viewing, or `None` when it is not on a live one.
    fn active_folder(&mut self) -> Option<LayoutApplyFolder>;
    /// The records an apply commits into, and the id source it mints from.
    fn layout_state(&mut self) -> (&mut LayoutRecords, &mut dyn PaneIdSource);
    /// Dismiss a transient overlay that would otherwise sit over the new pane.
    fn clear_spotlight(&mut self);
    /// Navigate to the session the applied arrangement selects.
    fn navigate_to_session(&mut self, session_id: &str);
    /// Publish the acknowledgement. `false` means the socket would not take it.
    fn send_result(&mut self, result: LayoutApplyResult) -> bool;
    /// Record a settlement line.
    fn record_diagnostic(&mut self, event: &str, diagnostic: LayoutApplySettlementDiagnostic);
}

/// What the execution did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutApplyExecution {
    /// Not an apply frame. The caller dispatches it to the legacy command path,
    /// which is deliberately outside this acknowledgement contract.
    NotMine,
    /// An apply this client is not the exact target for: another tab, a socket
    /// generation that has moved, or a frame with no correlation to answer on.
    /// Consumed, with nothing mutated and nothing sent.
    Ignored,
    /// An apply addressed to exactly this tab and socket.
    Settled(LayoutApplyConsumption),
}

/// How an exact-target apply ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutApplyConsumption {
    /// Committed locally, then acknowledged applied.
    Applied {
        /// What the arrangement leaves selected, or `None` when it selects
        /// nothing to navigate to.
        selected_session_id: Option<String>,
    },
    /// Committed locally, and this tab's identity was replaced before the
    /// acknowledgement could go out. The commit stands; the caller will hear
    /// the reservation expire, and the coordinator's ledger refuses the answer
    /// rather than settling against a tab that is no longer this one.
    AppliedUnacknowledged {
        /// What the arrangement leaves selected.
        selected_session_id: Option<String>,
    },
    /// Refused before any mutation.
    Rejected(LayoutApplyRejection),
}

/// Execute an apply addressed to this tab.
///
/// `command` is `None` for every frame that is not an apply, which is what keeps
/// the eight legacy UI commands outside this acknowledgement path.
pub fn execute_targeted_layout_apply(
    command: Option<&LayoutApplyCommand>,
    context: &mut impl LayoutApplyContext,
) -> LayoutApplyExecution {
    let Some(command) = command else {
        return LayoutApplyExecution::NotMine;
    };
    let Some(target) = exact_current_target(command, context) else {
        return LayoutApplyExecution::Ignored;
    };
    let Some(folder) = context.active_folder() else {
        return reject_current(&target, context, LayoutApplyRejection::NoActiveFolder);
    };
    if !is_current_live_folder(&folder) {
        return reject_current(&target, context, LayoutApplyRejection::NoActiveFolder);
    }
    let Some(document) = command.document.as_ref() else {
        return reject_current(&target, context, LayoutApplyRejection::InvalidDocument);
    };
    let applied = {
        let (records, ids) = context.layout_state();
        apply_layout_document(
            records,
            &folder.folder_key,
            document,
            &folder.live_session_ids,
            ids,
        )
    };
    let applied = match applied {
        Ok(applied) => applied,
        // The reason is deliberately dropped here. This tab answers with a
        // fixed sentence, and the refusal itself goes to this client's own log,
        // where a session id is not somebody's UI.
        Err(error) => {
            tracing::warn!(
                target: "layout",
                folder_key = %folder.folder_key,
                reason = %error,
                "refused a targeted layout apply"
            );
            return reject_current(&target, context, LayoutApplyRejection::InvalidDocument);
        }
    };
    let selected = applied.selected_session_id;
    context.clear_spotlight();
    if let Some(session_id) = selected.as_deref() {
        context.navigate_to_session(session_id);
    }
    if acknowledge_current(&target, context, LayoutApplyOutcome::Applied, None) {
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Applied {
            selected_session_id: selected,
        })
    } else {
        LayoutApplyExecution::Settled(LayoutApplyConsumption::AppliedUnacknowledged {
            selected_session_id: selected,
        })
    }
}

/// Answer an exact-target apply with the no-bridge refusal.
///
/// Separate from the executor because a shell whose router is not mounted must
/// still answer: the caller is holding a request open until this tab says what
/// happened, and silence is the one answer it cannot use. A frame that is not
/// this tab's exact target still gets nothing, because the reservation is on
/// the socket it named and a moved tab must not answer for its predecessor.
pub fn reject_layout_apply_without_bridge(
    command: Option<&LayoutApplyCommand>,
    context: &mut impl LayoutApplyContext,
) -> LayoutApplyExecution {
    let Some(command) = command else {
        return LayoutApplyExecution::NotMine;
    };
    let Some(target) = exact_current_target(command, context) else {
        return LayoutApplyExecution::Ignored;
    };
    reject_current(&target, context, LayoutApplyRejection::BridgeUnavailable)
}

fn exact_current_target(
    command: &LayoutApplyCommand,
    context: &impl LayoutApplyContext,
) -> Option<ExactApplyTarget> {
    if command.target_tab_id.is_empty()
        || command.target_socket_id.is_empty()
        || command.correlation_id.is_empty()
    {
        return None;
    }
    if command.target_tab_id != context.current_tab_id() {
        return None;
    }
    if context.current_socket_id() != Some(command.target_socket_id.as_str()) {
        return None;
    }
    Some(ExactApplyTarget {
        tab_id: command.target_tab_id.clone(),
        socket_id: command.target_socket_id.clone(),
        correlation_id: command.correlation_id.clone(),
    })
}

fn is_current_live_folder(folder: &LayoutApplyFolder) -> bool {
    !folder.folder_key.is_empty()
        && !folder.active_session_id.is_empty()
        && folder
            .live_session_ids
            .iter()
            .any(|session| *session == folder.active_session_id)
        && !folder.has_client_only_session
}

fn reject_current(
    target: &ExactApplyTarget,
    context: &mut impl LayoutApplyContext,
    reason: LayoutApplyRejection,
) -> LayoutApplyExecution {
    acknowledge_current(
        target,
        context,
        LayoutApplyOutcome::Rejected,
        Some(reason.message().to_owned()),
    );
    LayoutApplyExecution::Settled(LayoutApplyConsumption::Rejected(reason))
}

/// Report the settlement, then answer if this tab is still the same tab.
///
/// The identity is re-read HERE, after the commit, because the whole reason the
/// coordinator reserves an exact socket is that the answer has to come from the
/// socket the apply was composed against. A tab that redialled mid-apply must
/// not answer on its successor's behalf, and must not answer at all rather than
/// answer wrongly.
fn acknowledge_current(
    target: &ExactApplyTarget,
    context: &mut impl LayoutApplyContext,
    outcome: LayoutApplyOutcome,
    reason: Option<String>,
) -> bool {
    context.record_diagnostic(
        LAYOUT_APPLY_SETTLED_EVENT,
        LayoutApplySettlementDiagnostic {
            correlation_id: bounded_diagnostic_correlation(&target.correlation_id),
            outcome: outcome.as_str(),
        },
    );
    if context.current_tab_id() != target.tab_id
        || context.current_socket_id() != Some(target.socket_id.as_str())
    {
        tracing::info!(
            target: "layout",
            tab_id = %target.tab_id,
            "committed a layout apply this tab can no longer acknowledge"
        );
        return false;
    }
    context.send_result(LayoutApplyResult {
        correlation_id: target.correlation_id.clone(),
        outcome,
        reason,
    })
}

fn bounded_diagnostic_correlation(correlation_id: &str) -> String {
    correlation_id
        .chars()
        .take(LAYOUT_APPLY_DIAGNOSTIC_CORRELATION_MAX_CODE_POINTS)
        .collect()
}
