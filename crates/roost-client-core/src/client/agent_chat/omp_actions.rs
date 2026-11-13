//! The one-in-flight queue for receipt-tracked OMP input actions.
//!
//! Open, close, lease renewals, host replies, modal commands and typed
//! edit/focus never enter this queue: a native command that stays pending
//! until a picker closes must not hold the close path behind it.

use std::collections::VecDeque;

use serde_json::Value;

use super::host_inbox::OmpUiActionReceipt;

/// One receipt-tracked action is in flight; the rest wait here.
pub const MAX_PENDING_OMP_ACTIONS: usize = 32;

/// Where one OMP action was raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmpActionIdentity {
    /// The conversation whose view raised it.
    pub conversation_id: String,
    /// The per-mount browser view.
    pub view_id: String,
    /// The native transport the transcript currently names.
    pub transport_id: String,
}

/// One AgentOmpUi action waiting for its turn.
#[derive(Clone, PartialEq)]
pub struct OmpQueuedAction {
    /// The browser-minted request id, which the receipt carries back.
    pub request_id: String,
    /// The exact view and transport it belongs to.
    pub identity: OmpActionIdentity,
    /// The action JSON sent unchanged to the native controller.
    pub action: Value,
}

impl std::fmt::Debug for OmpQueuedAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OmpQueuedAction")
            .field("request_id", &self.request_id)
            .field("identity", &self.identity)
            .field("method", &self.action.get("method").and_then(Value::as_str))
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
struct InFlight {
    request_id: String,
    identity: OmpActionIdentity,
}

impl From<&OmpQueuedAction> for InFlight {
    fn from(action: &OmpQueuedAction) -> Self {
        Self {
            request_id: action.request_id.clone(),
            identity: action.identity.clone(),
        }
    }
}

/// Whether an action must wait for a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmpActionClass {
    /// An input or event the native controller renders; serialise it and wait
    /// for its private receipt.
    ReceiptTracked,
    /// Open, close, lease, host reply, a command, or a typed edit/focus that
    /// carries its own text acknowledgement. Never queued behind input.
    Bypass,
}

/// Classify one action by its method and event verb.
pub fn classify_action(action: &Value) -> OmpActionClass {
    let method = action.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "input" => OmpActionClass::ReceiptTracked,
        "event" => match action.pointer("/params/event/ev").and_then(Value::as_str) {
            // Typed text and caret movement have their own frame ACK; theme,
            // motion and resize describe the view itself and must not wait
            // behind a text edit that may still be reconciling.
            Some("edit" | "focus" | "theme" | "motion" | "resize") => OmpActionClass::Bypass,
            _ => OmpActionClass::ReceiptTracked,
        },
        _ => OmpActionClass::Bypass,
    }
}

/// What the caller should do with a submitted action.
#[derive(Debug, Clone, PartialEq)]
pub enum OmpSubmit {
    /// Send now with `receipt=false`.
    Bypass(OmpQueuedAction),
    /// Send now with `receipt=true`; it is now the one in flight.
    Send(OmpQueuedAction),
    /// Held behind the in-flight action.
    Queued,
    /// The bounded queue is full; the action was not accepted.
    Full,
}

/// The outcome of applying a receipt.
#[derive(Debug, Clone, PartialEq)]
pub enum OmpReceiptOutcome {
    /// Not for the in-flight action (stale, other view, or replayed).
    Ignored,
    /// Held until the transcript reaches `after_seq`.
    Deferred,
    /// The action completed; `next` is the action now ready to send.
    Completed {
        /// The request the receipt answered.
        request_id: String,
        /// Empty on success.
        error: String,
        /// The next queued action, already marked in flight.
        next: Box<Option<OmpQueuedAction>>,
    },
}

/// One-in-flight serialisation of receipt-tracked actions.
///
/// Typed edits, focus changes, modal commands, lease renewals and host replies
/// never enter this queue, so a pending command cannot hold the close path
/// behind it.
#[derive(Debug, Clone, Default)]
pub struct OmpActionQueue {
    in_flight: Option<InFlight>,
    deferred: Option<OmpUiActionReceipt>,
    queued: VecDeque<OmpQueuedAction>,
}

impl OmpActionQueue {
    /// Empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify and either release the action now or hold it.
    pub fn submit(
        &mut self,
        request_id: String,
        identity: OmpActionIdentity,
        action: Value,
    ) -> OmpSubmit {
        let pending = OmpQueuedAction {
            request_id,
            identity,
            action,
        };
        if classify_action(&pending.action) == OmpActionClass::Bypass {
            return OmpSubmit::Bypass(pending);
        }
        if self
            .queued
            .back_mut()
            .is_some_and(|previous| coalesce_text_input(previous, &pending))
        {
            return OmpSubmit::Queued;
        }
        if self.pending_count() >= MAX_PENDING_OMP_ACTIONS {
            return OmpSubmit::Full;
        }
        if self.in_flight.is_none() {
            self.in_flight = Some(InFlight::from(&pending));
            OmpSubmit::Send(pending)
        } else {
            self.queued.push_back(pending);
            OmpSubmit::Queued
        }
    }

    /// Apply a receipt for the in-flight action, honouring the transcript
    /// fence. A receipt that is not the in-flight one is discarded.
    pub fn receive(&mut self, receipt: &OmpUiActionReceipt, loaded_seq: u64) -> OmpReceiptOutcome {
        let Some(pending) = self.in_flight.as_ref() else {
            return OmpReceiptOutcome::Ignored;
        };
        if pending.request_id != receipt.origin_request_id
            || pending.identity.conversation_id != receipt.conversation_id
            || pending.identity.view_id != receipt.view_id
            || pending.identity.transport_id != receipt.transport_id
        {
            return OmpReceiptOutcome::Ignored;
        }
        if receipt.after_seq > loaded_seq {
            self.deferred = Some(receipt.clone());
            return OmpReceiptOutcome::Deferred;
        }
        self.finish_inner(receipt.error.clone())
    }

    /// Release a deferred receipt once the transcript has caught up.
    pub fn settle(&mut self, loaded_seq: u64) -> Option<OmpReceiptOutcome> {
        if self.deferred.as_ref()?.after_seq > loaded_seq {
            return None;
        }
        let deferred = self.deferred.take()?;
        Some(self.finish_inner(deferred.error))
    }

    fn finish_inner(&mut self, error: String) -> OmpReceiptOutcome {
        let Some(done) = self.in_flight.take() else {
            return OmpReceiptOutcome::Ignored;
        };
        self.deferred = None;
        let next = self.queued.pop_front();
        self.in_flight = next.as_ref().map(InFlight::from);
        OmpReceiptOutcome::Completed {
            request_id: done.request_id,
            error,
            next: Box::new(next),
        }
    }

    /// Drop every queued action and any deferred receipt: the view closed or
    /// the native transport was replaced.
    pub fn cancel_all(&mut self) {
        self.in_flight = None;
        self.deferred = None;
        self.queued.clear();
    }

    /// A replacement transport cannot execute input queued for its predecessor.
    pub fn retain_identity(&mut self, identity: &OmpActionIdentity) {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|pending| &pending.identity != identity)
        {
            self.cancel_all();
        }
    }

    /// Abort an ambiguous admission failure without retrying later actions.
    pub fn reject_admission(&mut self, request_id: &str, identity: &OmpActionIdentity) -> bool {
        if !self.in_flight.as_ref().is_some_and(|pending| {
            pending.request_id == request_id && &pending.identity == identity
        }) {
            return false;
        }
        self.cancel_all();
        true
    }

    /// How many receipt-tracked actions are waiting or in flight.
    pub fn pending_count(&self) -> usize {
        self.queued.len() + usize::from(self.in_flight.is_some())
    }
}

fn coalesce_text_input(previous: &mut OmpQueuedAction, next: &OmpQueuedAction) -> bool {
    if previous.identity != next.identity
        || previous.action.get("method").and_then(Value::as_str) != Some("input")
        || next.action.get("method").and_then(Value::as_str) != Some("input")
        || next.action.pointer("/params/before").is_some()
    {
        return false;
    }
    let Some(next_data) = next.action.pointer("/params/data").and_then(Value::as_str) else {
        return false;
    };
    let Some(Value::String(previous_data)) = previous.action.pointer_mut("/params/data") else {
        return false;
    };
    // Native key handlers receive one gesture, not a byte-stream parser.
    // Only plain insertion text can share a call; escape chords and bracketed
    // pastes must retain their original boundaries.
    if !next_data.chars().all(|character| !character.is_control())
        || !previous_data
            .chars()
            .all(|character| !character.is_control())
        || previous_data.len().saturating_add(next_data.len())
            > roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_INPUT_BYTES.min(
                roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_MESSAGE_BYTES / 2 - 1024,
            )
    {
        return false;
    }
    previous_data.push_str(next_data);
    true
}
