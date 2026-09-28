//! The per-view input outcome feed: for each batch a pane admitted from its
//! view, the immediate admission answer and, later, the settled outcome, so the
//! pane's predictive echo can place, acknowledge or wipe its guesses. Fed by
//! `handle_terminal_input` (admission) and `InputRouter` settlement; drained by
//! roost-web's terminal pane. Ports the admission watch of
//! `apps/web/src/components/terminal/cell-terminal-input.ts`.

use std::collections::{BTreeMap, VecDeque};

use crate::terminal::input::{InputOutcome, InputRefusal, PendingInput};

/// Most settled outcomes one view may leave undrained. A pane drains on every
/// revision, so only a view whose pane vanished without forgetting reaches it.
const SETTLED_PER_VIEW_CAPACITY: usize = 256;

/// How the most recent admission from one view went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewAdmission {
    /// Queued under this sequence; its outcome arrives through the feed.
    Admitted {
        /// The batch's own sequence.
        input_seq: u64,
    },
    /// Refused before it was queued: nothing was sent.
    Refused {
        /// The router's reason.
        reason: String,
    },
}

/// Admission answers and settled outcomes, per view.
#[derive(Debug, Default)]
pub struct InputOutcomeFeed {
    watched: BTreeMap<u64, String>,
    admissions: BTreeMap<String, ViewAdmission>,
    settled: BTreeMap<String, VecDeque<InputOutcome>>,
}

impl InputOutcomeFeed {
    /// An empty feed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one admission answer. A batch with no view has no pane to tell.
    pub fn note_admission(
        &mut self,
        view_id: Option<&str>,
        admission: &Result<PendingInput, InputRefusal>,
    ) {
        let Some(view_id) = view_id else {
            return;
        };
        let answer = match admission {
            Ok(pending) => {
                self.watched.insert(pending.input_seq, view_id.to_owned());
                ViewAdmission::Admitted {
                    input_seq: pending.input_seq,
                }
            }
            Err(refusal) => ViewAdmission::Refused {
                reason: refusal.reason.clone(),
            },
        };
        self.admissions.insert(view_id.to_owned(), answer);
    }

    /// Record one settled outcome, if its batch came from a watched view.
    pub fn observe_outcome(&mut self, outcome: &InputOutcome) {
        let Some(view_id) = self.watched.remove(&outcome.input_seq()) else {
            return;
        };
        let queue = self.settled.entry(view_id).or_default();
        if queue.len() >= SETTLED_PER_VIEW_CAPACITY {
            queue.pop_front();
        }
        queue.push_back(outcome.clone());
    }

    /// The answer to this view's most recent admission, consumed.
    pub fn take_admission(&mut self, view_id: &str) -> Option<ViewAdmission> {
        self.admissions.remove(view_id)
    }

    /// Every outcome settled for this view since the last drain, oldest first.
    pub fn take_outcomes(&mut self, view_id: &str) -> Vec<InputOutcome> {
        self.settled
            .remove(view_id)
            .map(Vec::from)
            .unwrap_or_default()
    }

    /// Drop everything held for a view whose pane is gone, including the
    /// batches still in flight, whose outcomes then have nobody to reach.
    pub fn forget_view(&mut self, view_id: &str) {
        self.watched.retain(|_, watched| watched != view_id);
        self.admissions.remove(view_id);
        self.settled.remove(view_id);
        tracing::debug!(target: "input", view_id, "input outcome feed forgot view");
    }
}
