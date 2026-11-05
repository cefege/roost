//! Transient, never-persisted OMP host-service requests and native action
//! receipts.
//!
//! A host-service value is a clipboard payload or a URL. It is held in memory
//! only, dropped by `Store::forget_session`-style teardown, never logged, never
//! serialised and never written to a transcript. Native receipts are private
//! Sync controls: they are fenced by the canonical chat sequence the browser
//! has already loaded, so an action whose effect is not yet visible cannot be
//! released early.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

/// How many service prompts one browser tab may hold at once. Mirrors the
/// native adapter's `MAX_PENDING` (32).
pub const MAX_PENDING_HOST_SERVICES: usize = 32;
/// Deferred receipts are cheap, but a peer must not be able to grow the set.
pub const MAX_PENDING_ACTION_RECEIPTS: usize = 64;
/// Tombstones for cancelled ids stop a late re-delivery re-opening a prompt.
pub const MAX_CANCELLED_HOST_SERVICES: usize = 64;
/// A copy payload may be a large paste; URLs are bounded by the same limit.
/// One MiB, matching the coordinator's host-service frame cap.
pub const MAX_HOST_SERVICE_VALUE_BYTES: usize = 1024 * 1024;

/// The kinds the native host adapter can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmpHostServiceKind {
    /// Copy text to the browser's clipboard.
    CopyText,
    /// Open a URL, or resolve a worker file path into the in-app viewer.
    OpenUrl,
    /// Open the native OMP stats dashboard through its capability bridge.
    OpenStatsDashboard,
}

impl OmpHostServiceKind {
    /// The wire spelling.
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::CopyText => "copy_text",
            Self::OpenUrl => "open_url",
            Self::OpenStatsDashboard => "open_stats_dashboard",
        }
    }

    /// Parse the wire spelling; any other value is not a host service.
    pub fn from_wire(kind: &str) -> Option<Self> {
        match kind {
            "copy_text" => Some(Self::CopyText),
            "open_url" => Some(Self::OpenUrl),
            "open_stats_dashboard" => Some(Self::OpenStatsDashboard),
            _ => None,
        }
    }
}

/// One one-shot host-service frame.
///
/// A cancellation carries the original identity and kind with an empty value;
/// it never applies an effect. `value` is deliberately absent from `Debug`: an
/// incident log must not contain the text an operator copied.
#[derive(Clone, PartialEq, Eq)]
pub struct OmpHostService {
    /// The native request this answers.
    pub service_id: String,
    /// The AgentOmpUi request whose native handler raised it.
    pub origin_request_id: String,
    /// The conversation whose OMP view asked.
    pub conversation_id: String,
    /// The browser view that owns the action.
    pub view_id: String,
    /// The native UI transport the action ran on.
    pub transport_id: String,
    /// Clipboard write or URL open.
    pub kind: OmpHostServiceKind,
    /// The clipboard text or URL. Empty exactly when `cancelled`. Shared so a
    /// render that reads every pending prompt cannot clone a MiB payload per
    /// prompt.
    pub value: Arc<str>,
    /// Withdraw the prompt; never apply.
    pub cancelled: bool,
}

impl fmt::Debug for OmpHostService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OmpHostService")
            .field("service_id", &self.service_id)
            .field("origin_request_id", &self.origin_request_id)
            .field("conversation_id", &self.conversation_id)
            .field("view_id", &self.view_id)
            .field("transport_id", &self.transport_id)
            .field("kind", &self.kind)
            .field("cancelled", &self.cancelled)
            .field("value_bytes", &self.value.len())
            .finish()
    }
}

/// A private native receipt for one receipt-tracked action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmpUiActionReceipt {
    /// The AgentOmpUi request the native handler completed.
    pub origin_request_id: String,
    /// The conversation whose OMP view ran it.
    pub conversation_id: String,
    /// The browser view that sent it.
    pub view_id: String,
    /// The native transport that ran it.
    pub transport_id: String,
    /// The canonical chat sequence that contains its rendered effect.
    pub after_seq: u64,
    /// Empty on success (including a no-op); the native handler's failure text
    /// otherwise.
    pub error: String,
}

/// One-host-service-or-receipt inbox owned by the store.
///
/// Transient by construction: it is not serialised, not projected into a
/// transcript, and cleared at the credential boundary.
#[derive(Debug, Clone, Default)]
pub struct OmpHostInbox {
    services: VecDeque<OmpHostService>,
    cancelled_services: BTreeSet<String>,
    receipts: VecDeque<OmpUiActionReceipt>,
}

impl OmpHostInbox {
    /// Empty inbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a service is waiting for this exact view and transport.
    pub fn pending_services(
        &self,
        conversation_id: &str,
        view_id: &str,
        transport_id: &str,
    ) -> Vec<OmpHostService> {
        self.services
            .iter()
            .filter(|service| {
                !service.cancelled
                    && service.conversation_id == conversation_id
                    && service.view_id == view_id
                    && service.transport_id == transport_id
            })
            .cloned()
            .collect()
    }

    /// The service with `service_id`, when it is still pending.
    pub fn service(&self, service_id: &str) -> Option<&OmpHostService> {
        self.services
            .iter()
            .find(|service| service.service_id == service_id && !service.cancelled)
    }

    /// Count every pending prompt, for the bounded-state assertions.
    pub fn pending_service_count(&self) -> usize {
        self.services
            .iter()
            .filter(|service| !service.cancelled)
            .count()
    }

    /// Insert a request, or withdraw the one a cancellation names.
    ///
    /// Returns whether the visible state changed. A duplicate id is a replay
    /// and does not re-open a prompt; a cancellation tombstones the id so a
    /// late re-delivery cannot resurrect it.
    pub fn apply_service(&mut self, service: OmpHostService) -> bool {
        if service.cancelled {
            let was_spent = self.cancelled_services.contains(&service.service_id);
            self.remember_spent(&service.service_id);
            if was_spent {
                return false;
            }
            let before = self.services.len();
            self.services
                .retain(|pending| pending.service_id != service.service_id);
            return before != self.services.len();
        }
        if self.cancelled_services.contains(&service.service_id)
            || self
                .services
                .iter()
                .any(|pending| pending.service_id == service.service_id)
        {
            return false;
        }
        if self.services.len() >= MAX_PENDING_HOST_SERVICES {
            tracing::warn!(
                target: "agent_chat",
                service_id = %service.service_id,
                limit = MAX_PENDING_HOST_SERVICES,
                "OMP host service dropped: too many prompts pending"
            );
            return false;
        }
        self.services.push_back(service);
        true
    }

    /// Remove a service once its reply has been sent.
    ///
    /// The id is tombstoned exactly like a cancellation: a replayed frame must
    /// not re-open a prompt whose reply already consumed the native request.
    pub fn complete_service(&mut self, service_id: &str) -> bool {
        let before = self.services.len();
        self.services
            .retain(|service| service.service_id != service_id);
        if before != self.services.len() {
            self.remember_spent(service_id);
            return true;
        }
        false
    }

    fn remember_spent(&mut self, service_id: &str) {
        if !self.cancelled_services.insert(service_id.to_owned()) {
            return;
        }
        if self.cancelled_services.len() > MAX_CANCELLED_HOST_SERVICES
            && let Some(oldest) = self.cancelled_services.iter().next().cloned()
        {
            self.cancelled_services.remove(&oldest);
        }
    }

    /// Drop prompts and receipts that belong to a transport this view no
    /// longer owns, so a reconnect cannot leave a stale Open button behind.
    pub fn discard_other_transports(
        &mut self,
        conversation_id: &str,
        view_id: &str,
        transport_id: &str,
    ) -> bool {
        let before = self.services.len();
        self.services.retain(|service| {
            service.conversation_id != conversation_id
                || service.view_id != view_id
                || service.transport_id == transport_id
        });
        self.receipts.retain(|receipt| {
            receipt.conversation_id != conversation_id
                || receipt.view_id != view_id
                || receipt.transport_id == transport_id
        });
        before != self.services.len()
    }

    /// Store a receipt until its transcript fence is satisfied.
    pub fn apply_receipt(&mut self, receipt: OmpUiActionReceipt) -> bool {
        if self
            .receipts
            .iter()
            .any(|pending| pending.origin_request_id == receipt.origin_request_id)
        {
            return false;
        }
        if self.receipts.len() >= MAX_PENDING_ACTION_RECEIPTS {
            self.receipts.pop_front();
        }
        self.receipts.push_back(receipt);
        true
    }

    /// Take the first receipt for this exact identity whose `after_seq` the
    /// transcript has already loaded.
    pub fn take_ready_receipt(
        &mut self,
        conversation_id: &str,
        view_id: &str,
        transport_id: &str,
        loaded_seq: u64,
    ) -> Option<OmpUiActionReceipt> {
        let position = self.receipts.iter().position(|receipt| {
            receipt.conversation_id == conversation_id
                && receipt.view_id == view_id
                && receipt.transport_id == transport_id
                && receipt.after_seq <= loaded_seq
        })?;
        self.receipts.remove(position)
    }

    /// Number of receipts waiting on the transcript fence.
    pub fn pending_receipt_count(&self) -> usize {
        self.receipts.len()
    }

    /// Forget one conversation; the OMP view can no longer answer.
    pub fn forget_conversation(&mut self, conversation_id: &str) -> bool {
        let before = self.services.len() + self.receipts.len();
        let forgotten: Vec<String> = self
            .services
            .iter()
            .filter(|service| service.conversation_id == conversation_id)
            .map(|service| service.service_id.clone())
            .collect();
        for service_id in forgotten {
            self.remember_spent(&service_id);
        }
        self.services
            .retain(|service| service.conversation_id != conversation_id);
        self.receipts
            .retain(|receipt| receipt.conversation_id != conversation_id);
        before != self.services.len() + self.receipts.len()
    }

    /// Every prompt and receipt goes at a credential boundary.
    pub fn clear(&mut self) -> bool {
        let had_any = !self.services.is_empty() || !self.receipts.is_empty();
        self.services.clear();
        self.receipts.clear();
        self.cancelled_services.clear();
        had_any
    }
}
