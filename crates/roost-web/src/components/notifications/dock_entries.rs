//! Every card the notification dock stacks, as one ordered list: the newer-build
//! notice, transfers, toasts and pending-close undos, oldest first so the front
//! card is the last entry. Called by `toast_stack`; reads the store and
//! `inline_transfer` claims, and depends on `version_banner` for the stale build.

use roost_client_core::store::Store;
use roost_client_core::store::pending_close::PendingClose;
use roost_client_core::store::toasts::{ToastId, ToastKind};
use roost_client_core::store::transfers::{Transfer, TransferState};

use super::inline_transfer::InlineTransferPresentation;
use super::version_banner::StaleBuild;

/// One card in the dock stack.
#[derive(Debug, Clone)]
pub enum DockEntry {
    /// The newer-build notice.
    Version(StaleBuild),
    /// One upload or download.
    Transfer(Transfer),
    /// A toast, by identity and raise order.
    Toast {
        id: ToastId,
        order: u64,
        kind: ToastKind,
    },
    /// A close still inside its undo window.
    Undo(PendingClose),
}

impl DockEntry {
    /// The render key: unique across kinds, stable while the card lives.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Version(_) => "version".to_owned(),
            Self::Transfer(transfer) => format!("transfer-{}", transfer.id),
            Self::Toast { order, .. } => format!("toast-{order}"),
            Self::Undo(close) => format!("undo-{}", close.session_id),
        }
    }

    /// Whether folding this card behind the overflow row would hide a failure.
    #[must_use]
    pub fn is_error(&self) -> bool {
        match self {
            Self::Toast { kind, .. } => *kind == ToastKind::Err,
            Self::Transfer(transfer) => transfer.state == TransferState::Failed,
            Self::Version(_) | Self::Undo(_) => false,
        }
    }
}

/// The dock's cards, oldest to front. A transfer an inline composer already
/// presents stays out, unless it failed.
#[must_use]
pub fn dock_entries(
    store: &Store,
    inline: Option<InlineTransferPresentation>,
    stale: Option<StaleBuild>,
) -> Vec<DockEntry> {
    let transfers = store
        .transfers
        .transfers()
        .filter(|transfer| {
            !inline
                .is_some_and(|presentation| presentation.suppresses(&transfer.id, transfer.state))
        })
        .cloned()
        .collect();
    let toasts = store
        .toasts
        .toasts()
        .map(|toast| (toast.id.clone(), toast.raised_order, toast.kind))
        .collect();
    let closes = store.pending_closes.entries().cloned().collect();
    order_entries(stale, transfers, toasts, closes)
}

/// Version, then transfers, then toasts by raise order, then undos.
fn order_entries(
    stale: Option<StaleBuild>,
    transfers: Vec<Transfer>,
    toasts: Vec<(ToastId, u64, ToastKind)>,
    closes: Vec<PendingClose>,
) -> Vec<DockEntry> {
    let mut entries = Vec::with_capacity(
        usize::from(stale.is_some()) + transfers.len() + toasts.len() + closes.len(),
    );
    entries.extend(stale.map(DockEntry::Version));
    entries.extend(transfers.into_iter().map(DockEntry::Transfer));
    entries.extend(
        toasts
            .into_iter()
            .map(|(id, order, kind)| DockEntry::Toast { id, order, kind }),
    );
    entries.extend(closes.into_iter().map(DockEntry::Undo));
    entries
}

#[cfg(test)]
mod tests {
    use roost_client_core::store::pending_close::{CloseLabels, PendingClose};
    use roost_client_core::store::toasts::{ToastId, ToastKind, ToastSource};
    use roost_client_core::store::transfers::{Transfer, TransferDirection, TransferState};

    use super::{DockEntry, order_entries};

    fn transfer(id: &str, state: TransferState) -> Transfer {
        Transfer {
            id: id.to_owned(),
            name: "shot.png".to_owned(),
            direction: TransferDirection::Up,
            bytes_done: 0,
            bytes_total: 10,
            speed_bps: None,
            eta_s: None,
            state,
            err: None,
            route: None,
            dismiss_at_ms: None,
            last_progress_ms: None,
        }
    }

    fn toast(subject: &str, order: u64, kind: ToastKind) -> (ToastId, u64, ToastKind) {
        (
            ToastId::new(ToastSource::Host { name: "test" }, subject),
            order,
            kind,
        )
    }

    fn close(session_id: &str) -> PendingClose {
        PendingClose {
            session_id: session_id.to_owned(),
            labels: CloseLabels::default(),
            expires_at_ms: 0,
        }
    }

    #[test]
    fn entries_run_transfers_toasts_and_undo_and_keys_are_unique() {
        let entries = order_entries(
            None,
            vec![
                transfer("a", TransferState::Running),
                transfer("b", TransferState::Failed),
            ],
            vec![
                toast("s1", 4, ToastKind::Ok),
                toast("s1", 7, ToastKind::Err),
            ],
            vec![close("s1")],
        );
        let keys: Vec<String> = entries.iter().map(DockEntry::key).collect();
        assert_eq!(
            keys,
            ["transfer-a", "transfer-b", "toast-4", "toast-7", "undo-s1"]
        );
        let errors: Vec<bool> = entries.iter().map(DockEntry::is_error).collect();
        assert_eq!(errors, [false, true, false, true, false]);
    }
}
