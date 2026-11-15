//! One upload or download as a dock toast card; progress stays in the shared
//! transfer ledger. The dock stack owns the order and supplies the ledger
//! record; the route chip rides in the card's actions slot and the upload
//! outcome wording follows the current transport and ledger rules.

use dioxus::prelude::*;
use roost_client_core::store::transfers::{
    Transfer, TransferDirection, TransferState, remove_transfer,
};

use super::store_write::write_store;
use super::transfer_outcome::{TransferOutcome, transfer_outcome};
use crate::components::ui::{Chip, ToastPresentation, ToastType};
use crate::display_format::{format_bytes, format_eta, format_speed};
use crate::pump::use_store;

/// One per-file transfer card.
#[component]
pub fn TransferToast(transfer: Transfer) -> Element {
    let pump = use_store();
    let id = transfer.id.clone();
    let name = transfer.name.clone();
    let outcome = transfer_outcome(&transfer);
    let settled = transfer.state.is_terminal();
    let failed = matches!(
        outcome,
        Some(TransferOutcome::Rejected | TransferOutcome::Ambiguous)
    );
    let meta = meta_line(&transfer, outcome);
    let route = transfer.route.map(|route| route.label());
    let route_verb = match transfer.direction {
        TransferDirection::Up => "Sent",
        TransferDirection::Down => "Received",
    };
    let role = if failed {
        Some("alert".to_owned())
    } else if settled {
        Some("status".to_owned())
    } else {
        None
    };

    let dismiss = move |_event: MouseEvent| {
        write_store(&pump, |store| {
            remove_transfer(store, &id);
        });
    };

    rsx! {
        div {
            "data-testid": "transfer-row",
            "data-outcome": outcome.map(TransferOutcome::as_str),
            "data-state": transfer.state.as_str(),
            ToastPresentation {
                variant: transfer_variant(&transfer),
                class: "[word-break:break-word]",
                title: name.clone(),
                description: meta,
                role,
                close_label: format!("Dismiss {name}"),
                on_close: dismiss,
                progress: if settled { None } else { progress_fraction(&transfer) },
                progress_class: "transition-transform duration-300 ease-out motion-reduce:transition-none",
                actions: route.map(|route| rsx! {
                    Chip {
                        label: route.to_owned(),
                        small: true,
                        title: format!("{route_verb} {}", route.to_lowercase()),
                        test_id: "transfer-route".to_owned(),
                    }
                }),
            }
        }
    }
}

/// Loading while bytes move or wait, Warning for a stalled card, and the
/// settled outcome's own variant after that.
fn transfer_variant(transfer: &Transfer) -> ToastType {
    transfer_outcome(transfer).map_or(
        if transfer.state == TransferState::Stalled {
            ToastType::Warning
        } else {
            ToastType::Loading
        },
        TransferOutcome::toast_variant,
    )
}

/// The progress track's value, or `None` for a card with no meaningful fraction
/// yet: a queued or hashing card has sent nothing, and a running card with no
/// declared total has nothing to be a fraction OF. Both show the Loading
/// spinner alone rather than a track that jumps to full.
fn progress_fraction(transfer: &Transfer) -> Option<f64> {
    if matches!(
        transfer.state,
        TransferState::Queued | TransferState::Hashing
    ) {
        return None;
    }
    if transfer.bytes_total == 0 {
        return None;
    }
    transfer.fraction()
}

/// The card's description line, including the settled outcomes.
fn meta_line(transfer: &Transfer, outcome: Option<TransferOutcome>) -> String {
    let line = match transfer.state {
        TransferState::Queued => "Queued…".to_owned(),
        TransferState::Hashing => "Checking…".to_owned(),
        TransferState::Dedup => "Already uploaded · reused".to_owned(),
        TransferState::Failed => {
            format!("Failed · {}", transfer.err.clone().unwrap_or_default())
        }
        TransferState::Ambiguous => transfer.err.clone().unwrap_or_default(),
        TransferState::Done => {
            format!(
                "{} · {}",
                verb(transfer),
                format_bytes(transfer.bytes_total as f64)
            )
        }
        TransferState::Running => {
            format!("{} · {}", active_verb(transfer), progress_line(transfer))
        }
        TransferState::Stalled => format!(
            "{} · stalled · {}",
            active_verb(transfer),
            progress_line(transfer)
        ),
    };
    match outcome {
        // The card must say what decision an ambiguous result asks the user to
        // make: bytes may already be on the worker, so another send could double them.
        Some(TransferOutcome::Ambiguous) => {
            format!("{line} · not retried — check the worker before sending again")
        }
        _ => line,
    }
}

fn verb(transfer: &Transfer) -> &'static str {
    match transfer.direction {
        roost_client_core::store::transfers::TransferDirection::Up => "Sent",
        roost_client_core::store::transfers::TransferDirection::Down => "Downloaded",
    }
}

fn active_verb(transfer: &Transfer) -> &'static str {
    match transfer.direction {
        TransferDirection::Up => "Sending",
        TransferDirection::Down => "Receiving",
    }
}

/// `12.0 MB / 20.0 MB · 60% · 1.2 MB/s · 8s left`, without the trailing
/// `left` when the rate or the total is still unknown.
fn progress_line(transfer: &Transfer) -> String {
    let total = if transfer.bytes_total > 0 {
        format!(" / {}", format_bytes(transfer.bytes_total as f64))
    } else {
        String::new()
    };
    let percent = if transfer.bytes_total > 0 {
        transfer
            .fraction()
            .map(|fraction| format!(" · {:.0}%", fraction * 100.0))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let base = format!(
        "{}{total}{percent} · {}",
        format_bytes(transfer.bytes_done as f64),
        format_speed(transfer.speed_bps.unwrap_or_default())
    );
    match transfer.eta_s {
        Some(seconds) => {
            let eta = format_eta(seconds as f64);
            if eta.is_empty() {
                base
            } else {
                format!("{base} · {eta} left")
            }
        }
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use roost_client_core::store::transfers::{Transfer, TransferDirection, TransferState};

    use super::transfer_variant;
    use crate::components::ui::ToastType;

    fn transfer(state: TransferState) -> Transfer {
        Transfer {
            id: "t1".to_owned(),
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

    #[test]
    fn a_transfer_reads_as_loading_until_it_settles() {
        let expected = [
            (TransferState::Queued, ToastType::Loading),
            (TransferState::Hashing, ToastType::Loading),
            (TransferState::Running, ToastType::Loading),
            (TransferState::Stalled, ToastType::Warning),
            (TransferState::Done, ToastType::Success),
            (TransferState::Dedup, ToastType::Success),
            (TransferState::Failed, ToastType::Error),
            (TransferState::Ambiguous, ToastType::Warning),
        ];
        for (state, variant) in expected {
            assert_eq!(transfer_variant(&transfer(state)), variant, "{state:?}");
        }
    }
}
