//! One upload or download per file in the transfer list. The popup owns the
//! list and supplies the ledger record; progress stays in the shared transfer
//! store rather than a parallel upload-only state.
//! Ports `apps/web/src/components/notifications/TransferRow.tsx`; the route
//! chip and upload outcome wording use the current transport and ledger rules.

use dioxus::prelude::*;
use roost_client_core::store::transfers::{Transfer, TransferDirection, TransferState};

use super::store_write::write_store;
use super::transfer_outcome::{TransferOutcome, transfer_outcome};
use crate::components::ui::{ButtonSize, Chip, IconButton, IconType, ListRow, Progress};
use crate::display_format::{format_bytes, format_eta, format_speed};
use crate::pump::use_store;

/// One per-file transfer row.
#[component]
pub fn TransferRow(transfer: Transfer) -> Element {
    let pump = use_store();
    let id = transfer.id.clone();
    let name = transfer.name.clone();
    let preview = transfer.preview_url.clone();
    let release_preview = preview.clone();
    use_drop(move || {
        if let Some(url) = release_preview.as_deref() {
            crate::components::terminal_chrome::dom::revoke_preview(url);
        }
    });
    let glyph = transfer_glyph(transfer.direction, &transfer.name);
    let outcome = transfer_outcome(&transfer);
    let settled = transfer.state.is_terminal();
    let progress_value = progress_fraction(&transfer);
    let meta = meta_line(&transfer, outcome);
    let meta_color = meta_color(&transfer, outcome);
    let outcome_name = outcome.map(TransferOutcome::as_str);
    let route = transfer.route.map(|route| route.label());
    let route_verb = match transfer.direction {
        TransferDirection::Up => "Sent",
        TransferDirection::Down => "Received",
    };

    let dismiss = move |_event: MouseEvent| {
        write_store(&pump, |store| {
            roost_client_core::store::transfers::remove_transfer(store, &id);
        });
    };

    let leading = preview.map(|source| {
        rsx! {
            img {
                "data-testid": "transfer-preview",
                src: source,
                alt: "",
                class: "h-full w-full rounded object-cover",
            }
        }
    });
    let leading_icon = leading.is_none().then_some(glyph);
    let headline = rsx! {
        span {
            class: "flex min-w-0 items-center gap-2",
            span {
                title: name.clone(),
                class: "min-w-0 flex-auto truncate",
                "{name}"
            }
            if let Some(route) = route {
                Chip {
                    label: route.to_owned(),
                    small: true,
                    title: format!("{route_verb} {}", route.to_lowercase()),
                    test_id: "transfer-route".to_owned(),
                }
            }
        }
    };
    let support = rsx! {
        span {
            class: "flex flex-col gap-1 pt-1",
            if !settled {
                Progress { value: progress_value, label: format!("{name} progress") }
            }
            span {
                class: meta_color,
                "data-outcome": outcome_name,
                "{meta}"
            }
        }
    };
    let trailing = rsx! {
        IconButton {
            icon: IconType::X,
            label: format!("Dismiss {name}"),
            size: ButtonSize::IconXs,
            "data-testid": "transfer-dismiss",
            onclick: dismiss,
        }
    };

    rsx! {
        ListRow {
            test_id: Some("transfer-row".to_owned()),
            leading_icon,
            leading,
            headline,
            support: Some(support),
            trailing: Some(trailing),
        }
    }
}

/// The glyph a row without an image preview shows, read from the file name so
/// a video never paints a broken image.
fn transfer_glyph(direction: TransferDirection, name: &str) -> IconType {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "svg" | "bmp" => IconType::Image,
        "mp4" | "mov" | "webm" | "mkv" | "avi" | "m4v" => IconType::Film,
        "mp3" | "wav" | "m4a" | "ogg" | "flac" | "opus" => IconType::FileAudio,
        "pdf" => IconType::FileText,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z" | "rar" => IconType::FileArchive,
        _ => match direction {
            TransferDirection::Up => IconType::Upload,
            TransferDirection::Down => IconType::Download,
        },
    }
}

/// The progress bar's value, or `None` for a row with no meaningful fraction
/// yet: a queued or hashing row has sent nothing, and a running row with no
/// declared total has nothing to be a fraction OF.
/// Both use an indeterminate bar rather than one that jumps to full.
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

/// The row's second line, including the settled outcomes.
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
        // The row must say what decision an ambiguous result asks the user to
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

/// The settled line's text colour class and the ambiguous row's warning class.
/// A write whose fate is unknown is neither a success nor a clean failure.
fn meta_color(transfer: &Transfer, outcome: Option<TransferOutcome>) -> &'static str {
    match outcome {
        Some(TransferOutcome::Rejected) => "text-xs leading-4 text-destructive",
        Some(TransferOutcome::Ambiguous) => "text-xs leading-4 text-warning",
        Some(TransferOutcome::Accepted) | Some(TransferOutcome::Deduplicated) => {
            "text-xs leading-4 text-info"
        }
        None if transfer.state == TransferState::Stalled => "text-xs leading-4 text-warning",
        None => "text-xs leading-4 text-muted-foreground",
    }
}
