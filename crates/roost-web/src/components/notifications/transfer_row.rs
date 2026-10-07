//! One upload or download inside the transfer popup. The popup owns the sole
//! surface and supplies the card; the lifecycle and the per-job progress are
//! `roost_client_core::store::transfers`, read here rather than reimplemented.
//! Ports `apps/web/src/components/notifications/TransferRow.tsx`, and its
//! settled-line wording for the three outcomes; the route chip is v3's.

use dioxus::prelude::*;
use roost_client_core::store::transfers::{Transfer, TransferDirection, TransferState};

use super::store_write::write_store;
use super::transfer_outcome::{TransferOutcome, transfer_outcome};
use crate::components::md::{Chip, IconButton, IconButtonSize, ListRow, ProgressBar};
use crate::display_format::{format_bytes, format_eta, format_speed};
use crate::pump::use_store;

/// One card.
#[component]
pub fn TransferRow(transfer: Transfer) -> Element {
    let pump = use_store();
    let id = transfer.id.clone();
    let name = transfer.name.clone();
    let preview = transfer.preview_url.clone();
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
                style: "width: 100%; height: 100%; border-radius: var(--md-shape-sm); object-fit: cover;",
            }
        }
    });
    let leading_icon = leading.is_none().then(|| glyph.to_owned());
    let headline = rsx! {
        span { style: "display: flex; align-items: center; gap: var(--md-space-2); min-width: 0;",
            span {
                title: name.clone(),
                style: "flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
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
            style: "display: flex; flex-direction: column; gap: var(--md-space-1); padding-block-start: var(--md-space-1);",
            if !settled {
                ProgressBar { value: progress_value, label: format!("{name} progress") }
            }
            span {
                class: "md-body-s",
                "data-outcome": outcome_name,
                style: "font-size: var(--md-body-s-size); line-height: var(--md-body-s-line); color: {meta_color};",
                "{meta}"
            }
        }
    };
    let trailing = rsx! {
        IconButton {
            icon: "close",
            label: format!("Dismiss {name}"),
            size: IconButtonSize::IconXs,
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

/// The glyph a card without an image preview shows: what kind of file it is,
/// read from the name, so a video never paints a broken image.
fn transfer_glyph(direction: TransferDirection, name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "svg" | "bmp" => "image",
        "mp4" | "mov" | "webm" | "mkv" | "avi" | "m4v" => "movie",
        "mp3" | "wav" | "m4a" | "ogg" | "flac" | "opus" => "audio_file",
        "pdf" => "picture_as_pdf",
        "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z" | "rar" => "folder_zip",
        _ => match direction {
            TransferDirection::Up => "upload_file",
            TransferDirection::Down => "download",
        },
    }
}

/// The progress bar's value, or `None` for a card with no meaningful fraction
/// yet: a queued or hashing card has sent nothing, and a running card with no
/// declared total has nothing to be a fraction OF. Those render as the
/// indeterminate bar rather than a bar that jumps to full.
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

/// The card's second line, and the only place the three settled outcomes read
/// differently from one another.
fn meta_line(transfer: &Transfer, outcome: Option<TransferOutcome>) -> String {
    let line = match transfer.state {
        TransferState::Queued => "Queued…".to_owned(),
        TransferState::Hashing => "Checking…".to_owned(),
        TransferState::Dedup => "Already uploaded · reused".to_owned(),
        TransferState::Failed | TransferState::Ambiguous => {
            transfer.err.clone().unwrap_or_default()
        }
        TransferState::Done => {
            format!(
                "{} · {}",
                verb(transfer),
                format_bytes(transfer.bytes_total as f64)
            )
        }
        _ => progress_line(transfer),
    };
    match outcome {
        // The card must say what the user is being asked to decide: these bytes
        // may already be on the worker, so sending them again is a doubled
        // upload. Nothing here retries on its own.
        Some(TransferOutcome::Ambiguous) => {
            format!("{line} · not retried — check the worker before sending again")
        }
        _ => line,
    }
}

fn verb(transfer: &Transfer) -> &'static str {
    match transfer.direction {
        roost_client_core::store::transfers::TransferDirection::Up => "Uploaded",
        roost_client_core::store::transfers::TransferDirection::Down => "Downloaded",
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

/// The settled line's colour, and the ambiguous card's own warning colour: a
/// write whose fate is unknown is neither a success nor a clean failure.
fn meta_color(transfer: &Transfer, outcome: Option<TransferOutcome>) -> &'static str {
    match outcome {
        Some(TransferOutcome::Rejected) => "var(--md-sys-color-error)",
        Some(TransferOutcome::Ambiguous) => "var(--status-warn)",
        Some(TransferOutcome::Accepted) | Some(TransferOutcome::Deduplicated) => {
            "var(--md-sys-color-tertiary)"
        }
        None if transfer.state == TransferState::Stalled => "var(--status-warn)",
        None => "var(--md-sys-color-on-surface-variant)",
    }
}
