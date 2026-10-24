//! One upload or download per file in the transfer list. The popup owns the
//! list and supplies the ledger record; progress stays in the shared transfer
//! store rather than a parallel upload-only state.
//! Ports `apps/web/src/components/notifications/TransferRow.tsx`; the route
//! chip and upload outcome wording use the current transport and ledger rules.

use dioxus::prelude::*;
use libero::{
    components::{ActionIcon, Box, Chip, Flex, Pictogram, ProgressBar, SvgData, Text},
    sx::sx,
};
use roost_client_core::store::transfers::{Transfer, TransferDirection, TransferState};

use super::store_write::write_store;
use super::transfer_outcome::{TransferOutcome, transfer_outcome};
use crate::display_format::{format_bytes, format_eta, format_speed};
use crate::pump::use_store;

/// One per-file transfer row.
#[component]
pub fn TransferRow(transfer: Transfer) -> Element {
    let pump = use_store();
    let row_sx = use_hook(|| sx().padding("xs"));
    let content_sx = use_hook(|| sx().min_width("0").flex("1"));
    let name_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .white_space("nowrap")
            .with("text-overflow", "ellipsis")
    });
    let preview_sx = use_hook(|| {
        sx().width("2rem")
            .height("2rem")
            .border_radius("xs")
            .with("object-fit", "cover")
    });
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

    rsx! {
        Flex { direction: "row", align: "start", gap: "xs", wrap: false, sx: row_sx,
            "data-testid": "transfer-row",
            if let Some(source) = preview {
                Box { component: "img",
                    attributes: vec![Attribute::new("src", source, None, false), Attribute::new("alt", "", None, false)],
                    "data-testid": "transfer-preview", sx: preview_sx,
                }
            } else {
                Pictogram { icon: glyph, width: "16", height: "16" }
            }
            Flex { direction: "column", gap: "xs", sx: content_sx,
                Flex { direction: "row", align: "center", gap: "xs", wrap: false,
                    Text { size: "sm", title: name.clone(), sx: name_sx, "{name}" }
                    if let Some(route) = route {
                        Chip { size: "xs", title: format!("{route_verb} {}", route.to_lowercase()),
                            "data-testid": "transfer-route", "{route}"
                        }
                    }
                }
                if !settled {
                    ProgressBar { value: progress_value, max: 1.0,
                        aria_label: format!("{name} progress"),
                    }
                }
                Text { size: "xs", color: meta_color, "data-outcome": outcome_name, "{meta}" }
            }
            ActionIcon { icon: pictogram_icons_lucide::x::outlined, size: "xs", variant: "standard",
                aria_label: format!("Dismiss {name}"), "data-testid": "transfer-dismiss",
                onclick: dismiss,
            }
        }
    }
}

/// The glyph a row without an image preview shows, read from the file name so
/// a video never paints a broken image.
fn transfer_glyph(direction: TransferDirection, name: &str) -> SvgData {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "svg" | "bmp" => {
            pictogram_icons_lucide::image::outlined
        }
        "mp4" | "mov" | "webm" | "mkv" | "avi" | "m4v" => pictogram_icons_lucide::film::outlined,
        "mp3" | "wav" | "m4a" | "ogg" | "flac" | "opus" => {
            pictogram_icons_lucide::file_audio::outlined
        }
        "pdf" => pictogram_icons_lucide::file_text::outlined,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z" | "rar" => {
            pictogram_icons_lucide::file_archive::outlined
        }
        _ => match direction {
            TransferDirection::Up => pictogram_icons_lucide::upload::outlined,
            TransferDirection::Down => pictogram_icons_lucide::download::outlined,
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

/// An unresolved write is neither a success nor a clean failure.
fn meta_color(transfer: &Transfer, outcome: Option<TransferOutcome>) -> &'static str {
    match outcome {
        Some(TransferOutcome::Rejected) => "error",
        Some(TransferOutcome::Ambiguous) => "warning",
        Some(TransferOutcome::Accepted) | Some(TransferOutcome::Deduplicated) => "info",
        None if transfer.state == TransferState::Stalled => "warning",
        None => "muted",
    }
}
