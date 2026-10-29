//! Inline transfer claims for uploads with a visible local presentation.
//! Terminal composers claim their image uploads; the notification dock reads the
//! same claims to avoid duplicating active and successful transfer cards.
//! Failed and ambiguous ledger rows stay visible because they need global notice.

use std::collections::{BTreeMap, BTreeSet};

use dioxus::prelude::*;
use roost_client_core::store::transfers::TransferState;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct InlineTransferClaims {
    by_scope: BTreeMap<String, BTreeSet<String>>,
}

/// Claims the transfer ids a local composer already presents inline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InlineTransferPresentation(Signal<InlineTransferClaims>);

impl InlineTransferPresentation {
    /// Install one registry above the shell and its notification dock.
    pub fn provide() -> Self {
        use_context_provider(|| Self(Signal::new(InlineTransferClaims::default())))
    }

    /// Replace one composer's current image-upload claims.
    pub fn replace(&self, scope: String, upload_ids: BTreeSet<String>) {
        let mut registry = self.0;
        registry.with_mut(|claims| {
            if upload_ids.is_empty() {
                claims.by_scope.remove(&scope);
            } else {
                claims.by_scope.insert(scope, upload_ids);
            }
        });
    }

    /// Remove every claim made by a composer that unmounted.
    pub fn clear(&self, scope: &str) {
        let mut registry = self.0;
        registry.with_mut(|claims| {
            claims.by_scope.remove(scope);
        });
    }

    /// Whether a transfer is already represented by an inline image thumbnail.
    #[must_use]
    pub fn suppresses(&self, upload_id: &str, state: TransferState) -> bool {
        !matches!(state, TransferState::Failed | TransferState::Ambiguous)
            && self
                .0
                .read()
                .by_scope
                .values()
                .any(|upload_ids| upload_ids.contains(upload_id))
    }
}

/// The registry where it is available, or no inline suppression in isolated renders.
#[must_use]
pub fn use_inline_transfer_presentation() -> Option<InlineTransferPresentation> {
    try_use_context::<InlineTransferPresentation>()
}
