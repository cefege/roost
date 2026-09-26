//! The eight legacy UI commands: which sessions each one names, and the
//! canonical copy that goes onto the bus.
//!
//! Ported from `apps/coord/src/ui-state/ui-legacy-command.ts`. The session
//! identities are extracted BEFORE the database is asked about them, so an
//! oversized id is refused as a malformed argument rather than as a lookup that
//! happens to miss; the canonical copy is rebuilt field by field, so a command
//! carrying unknown wire fields relays without them.
//!
//! `applyLayout` is deliberately NOT canonicalisable here. It is admitted only
//! by `UiApplyLayout`, which reserves a target socket and waits for an
//! acknowledgement; relaying it here would publish an apply nobody is waiting
//! for.

use connectrpc::{ConnectError, ErrorCode};
use roost_proto as proto;
use roost_proto::__buffa::oneof::ui_command::Command;
use roost_protocol::layout::LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES;
use roost_protocol::validate::max_utf8_bytes;

/// Every session a command names, in the order the wire presents them.
///
/// A command that names no session returns nothing, so the caller's database
/// check is skipped rather than run against an empty list.
pub fn legacy_ui_command_session_ids(command: &proto::UiCommand) -> Result<Vec<String>, ConnectError> {
    let session_ids = match command.command.as_ref() {
        Some(Command::PlaceSplit(split)) => {
            vec![split.session_id.clone(), split.anchor_session_id.clone()]
        }
        Some(Command::SelectTab(select)) => vec![select.session_id.clone()],
        Some(Command::FocusPane(focus)) => vec![focus.session_id.clone()],
        Some(Command::CloseTab(close)) => vec![close.session_id.clone()],
        Some(Command::Spotlight(spotlight)) => vec![spotlight.session_id.clone()],
        Some(Command::MoveTab(move_tab)) => {
            vec![move_tab.session_id.clone(), move_tab.dest_session_id.clone()]
        }
        Some(Command::Navigate(_)) | Some(Command::Arrange(_)) | Some(Command::ApplyLayout(_)) | None => {
            Vec::new()
        }
    };
    for session_id in &session_ids {
        max_utf8_bytes("ui command session id", session_id, LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES)
            .map_err(|_| {
                ConnectError::new(
                    ErrorCode::InvalidArgument,
                    "invalid UI command session id",
                )
            })?;
    }
    Ok(session_ids)
}

/// The command as the bus may carry it.
///
/// Each arm rebuilds the message rather than forwarding the caller's, so a
/// retired or unknown field cannot ride along, and each arm re-checks the one
/// enum-shaped field the wire leaves as a string.
pub fn canonical_legacy_ui_command(command: &proto::UiCommand) -> Result<proto::UiCommand, ConnectError> {
    let canonical = match command.command.as_ref() {
        Some(Command::Navigate(navigate)) => Command::Navigate(Box::new(proto::UiNavigate {
            path: navigate.path.clone(),
            ..Default::default()
        })),
        Some(Command::PlaceSplit(split)) => {
            if split.dir != "row" && split.dir != "col" {
                return Err(invalid("invalid UI split direction"));
            }
            Command::PlaceSplit(Box::new(proto::UiPlaceSplit {
                session_id: split.session_id.clone(),
                anchor_session_id: split.anchor_session_id.clone(),
                dir: split.dir.clone(),
                insert_first: split.insert_first,
                ..Default::default()
            }))
        }
        Some(Command::SelectTab(select)) => Command::SelectTab(Box::new(proto::UiSelectTab {
            session_id: select.session_id.clone(),
            ..Default::default()
        })),
        Some(Command::FocusPane(focus)) => Command::FocusPane(Box::new(proto::UiFocusPane {
            session_id: focus.session_id.clone(),
            ..Default::default()
        })),
        Some(Command::MoveTab(move_tab)) => Command::MoveTab(Box::new(proto::UiMoveTab {
            session_id: move_tab.session_id.clone(),
            dest_session_id: move_tab.dest_session_id.clone(),
            ..Default::default()
        })),
        Some(Command::Arrange(arrange)) => {
            if !matches!(
                arrange.preset.as_str(),
                "even" | "rows" | "tiled" | "main-vertical" | "balance"
            ) {
                return Err(invalid("invalid UI arrange preset"));
            }
            Command::Arrange(Box::new(proto::UiArrange {
                preset: arrange.preset.clone(),
                ..Default::default()
            }))
        }
        Some(Command::CloseTab(close)) => Command::CloseTab(Box::new(proto::UiCloseTab {
            session_id: close.session_id.clone(),
            ..Default::default()
        })),
        Some(Command::Spotlight(spotlight)) => Command::Spotlight(Box::new(proto::UiSpotlight {
            session_id: spotlight.session_id.clone(),
            off: spotlight.off,
            ..Default::default()
        })),
        Some(Command::ApplyLayout(_)) | None => {
            return Err(invalid("unsupported UI command"));
        }
    };
    Ok(proto::UiCommand {
        command: Some(canonical),
        ..Default::default()
    })
}

fn invalid(message: &'static str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}
