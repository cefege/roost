//! The tab fence on the CALLING side: what this client composed, and what it
//! must do when the coordinator answers.
//!
//! A layout apply is composed against a tab: a fingerprint, a tab id, and the
//! socket generation that tab held at the time. If the tab has moved, the
//! coordinator refuses -- it is refusing a stale arrangement, and the client
//! that retries the same apply against a fence is spinning. The recovery is
//! RE-FETCH AND RECOMPUTE, and this file has no arm that carries a document
//! forward, so "retry the same apply" is not a state this client can reach.
//!
//! The re-fetch and the broadcast then converge because they read ONE source:
//! both arrangements come from the document, through
//! `roost_client_core::store::layout::apply_layout_document`.

use roost_protocol::layout::LayoutDocumentV1;

use crate::store::layout::{LayoutDocumentError, PaneLayout, export_layout_document};

/// The reason a recovery re-reads the retained tab states.
pub const LAYOUT_APPLY_REFETCH_REASON: &str =
    "the target tab has moved; re-read its reported arrangement and recompute";

/// One browser tab's retained report, as `UiListStates` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedTab {
    /// The device key that reported it.
    pub fingerprint: String,
    /// Its tab id.
    pub tab_id: String,
    /// The arrangement it last reported.
    ///
    /// Absent is NOT a refusal and NOT a fence: a tab that has never tiled
    /// anything reports no document, and the first apply to it is exactly the
    /// first arrangement. A port that requires a prior document here refuses
    /// every new tab's first layout.
    pub layout_document: Option<LayoutDocumentV1>,
}

/// The exact tab and socket generation an apply names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutApplyTarget {
    /// The device key.
    pub fingerprint: String,
    /// The tab id.
    pub tab_id: String,
    /// The socket generation the target held when this was composed.
    pub socket_id: String,
}

/// One apply this client composed, and the folder it arranges.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingLayoutApply {
    /// The tab it was composed against.
    pub target: LayoutApplyTarget,
    /// The bucket the document belongs to.
    pub folder_key: String,
    /// The arrangement being sent, already checked against the shared parser.
    pub document: LayoutDocumentV1,
}

/// What the coordinator answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutApplyAnswer {
    /// The target applied the document.
    Applied { correlation_id: String },
    /// The target refused it, with a bounded reason.
    Rejected {
        /// The correlation the answer travelled on.
        correlation_id: String,
        /// What the target said, already bounded by the coordinator.
        reason: String,
    },
    /// No live socket could answer for that tab.
    TargetGone { correlation_id: String },
    /// Not an outcome this build knows, so not one to act on.
    Unrecognised { correlation_id: String },
}

impl LayoutApplyAnswer {
    /// The correlation this answer travelled on.
    pub fn correlation_id(&self) -> &str {
        match self {
            Self::Applied { correlation_id }
            | Self::Rejected { correlation_id, .. }
            | Self::TargetGone { correlation_id }
            | Self::Unrecognised { correlation_id } => correlation_id,
        }
    }
}

/// What this client owes after an answer.
///
/// THERE IS NO RETRY ARM, and that is the port. Every recovery that is not
/// `Settled` re-reads the retained tab states instead of re-sending, so the
/// arrangement that lands on the target is always composed against a tab that
/// still exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutApplyRecovery {
    /// The arrangement is on the target tab. Nothing is owed.
    Settled {
        /// The correlation that settled.
        correlation_id: String,
    },
    /// The tab has moved, or refused: re-read the retained tab states and
    /// recompose against whatever the tab reports now.
    Refetch {
        /// The correlation that did not settle.
        correlation_id: String,
        /// Why, for this client's log.
        reason: String,
    },
    /// The answer is not for an apply this client issued on this socket.
    Unrecognised {
        /// The correlation that arrived.
        correlation_id: String,
    },
}

/// The applies this client is holding open, one per target tab.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingLayoutApplies {
    entries: Vec<PendingLayoutApply>,
}

impl PendingLayoutApplies {
    /// No open applies.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an apply as open, replacing any apply already open on the same
    /// tab. Replacing rather than queueing is the fence: a second apply to a
    /// tab that has not answered the first is a compose against a tab whose
    /// state this client has not re-read, and its answer would be matched
    /// against the wrong arrangement.
    pub fn begin(&mut self, pending: PendingLayoutApply) {
        self.forget(&pending.target);
        self.entries.push(pending);
    }

    /// The open apply for a target, if there is one.
    pub fn pending(&self, target: &LayoutApplyTarget) -> Option<&PendingLayoutApply> {
        self.entries
            .iter()
            .find(|entry| same_target(&entry.target, target))
    }

    /// How many applies are open.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is open.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Settle one open apply against an answer that arrived on `socket_id`.
    ///
    /// The socket is part of the argument because it is the fence this side can
    /// still check: an answer that arrives on a generation the apply was not
    /// composed against settles nothing, and must not be read as though the
    /// target had applied an arrangement it never saw.
    pub fn settle(
        &mut self,
        target: &LayoutApplyTarget,
        socket_id: &str,
        answer: &LayoutApplyAnswer,
    ) -> LayoutApplyRecovery {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| same_target(&entry.target, target))
        else {
            return LayoutApplyRecovery::Unrecognised {
                correlation_id: answer.correlation_id().to_owned(),
            };
        };
        if self.entries[index].target.socket_id != socket_id {
            tracing::info!(
                target: "layout",
                tab_id = %target.tab_id,
                composed_against = %self.entries[index].target.socket_id,
                answered_on = socket_id,
                "layout apply answered on a socket generation it was not composed against"
            );
            self.entries.remove(index);
            return LayoutApplyRecovery::Refetch {
                correlation_id: answer.correlation_id().to_owned(),
                reason: LAYOUT_APPLY_REFETCH_REASON.to_owned(),
            };
        }
        let pending = self.entries.remove(index);
        settle_layout_apply(&pending, answer)
    }

    fn forget(&mut self, target: &LayoutApplyTarget) {
        self.entries
            .retain(|entry| !same_target(&entry.target, target));
    }
}

/// Read an answer against the apply it settles.
pub fn settle_layout_apply(
    pending: &PendingLayoutApply,
    answer: &LayoutApplyAnswer,
) -> LayoutApplyRecovery {
    match answer {
        LayoutApplyAnswer::Applied { correlation_id } => {
            tracing::info!(
                target: "layout",
                folder_key = %pending.folder_key,
                tab_id = %pending.target.tab_id,
                "layout apply settled applied"
            );
            LayoutApplyRecovery::Settled {
                correlation_id: correlation_id.clone(),
            }
        }
        LayoutApplyAnswer::Rejected {
            correlation_id,
            reason,
        } => {
            tracing::warn!(
                target: "layout",
                folder_key = %pending.folder_key,
                tab_id = %pending.target.tab_id,
                reason = %reason,
                "layout apply refused; re-reading the tab's arrangement"
            );
            LayoutApplyRecovery::Refetch {
                correlation_id: correlation_id.clone(),
                reason: LAYOUT_APPLY_REFETCH_REASON.to_owned(),
            }
        }
        LayoutApplyAnswer::TargetGone { correlation_id } => {
            tracing::warn!(
                target: "layout",
                folder_key = %pending.folder_key,
                tab_id = %pending.target.tab_id,
                "layout apply target has moved; re-reading the tab's arrangement"
            );
            LayoutApplyRecovery::Refetch {
                correlation_id: correlation_id.clone(),
                reason: LAYOUT_APPLY_REFETCH_REASON.to_owned(),
            }
        }
        LayoutApplyAnswer::Unrecognised { correlation_id } => LayoutApplyRecovery::Unrecognised {
            correlation_id: correlation_id.clone(),
        },
    }
}

/// Compose an apply for a reported tab, against the arrangement this client
/// holds for the same folder.
///
/// The document is exported and re-parsed here rather than at the send site, so
/// an apply that cannot be admitted by the coordinator's parser is never
/// composed at all.
pub fn compose_layout_apply(
    folder_key: &str,
    live_session_ids: &[String],
    layout: &PaneLayout,
    target: &ReportedTab,
    socket_id: &str,
) -> Result<PendingLayoutApply, LayoutDocumentError> {
    let document = export_layout_document(folder_key, live_session_ids, layout)?;
    Ok(PendingLayoutApply {
        target: LayoutApplyTarget {
            fingerprint: target.fingerprint.clone(),
            tab_id: target.tab_id.clone(),
            socket_id: socket_id.to_owned(),
        },
        folder_key: folder_key.to_owned(),
        document,
    })
}

fn same_target(left: &LayoutApplyTarget, right: &LayoutApplyTarget) -> bool {
    left.fingerprint == right.fingerprint && left.tab_id == right.tab_id
}
