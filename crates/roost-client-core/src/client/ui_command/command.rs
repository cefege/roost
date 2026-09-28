//! The Sync-delivered UI command, read off the wire into this crate's types,
//! and the broadcast rule the eight legacy commands travel under.
//!
//! Ports the frame reading of `apps/web/src/lib/uiCommandDispatch.ts` and
//! `frameAccepted` from `apps/web/src/lib/uiCommandCore.ts`. Called by
//! `dispatch::drain_ui_commands`; the layout document of an acknowledged apply
//! goes through the shared adapter in `roost_protocol::proto_adapters`.

use roost_proto::__buffa::oneof::ui_command::Command as WireCommand;
use roost_protocol::proto_adapters::layout_document_proto::layout_document_from_proto;

use crate::client::ui_state::LayoutApplyCommand;

/// One of the eight fire-and-forget UI commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyUiCommand {
    /// Route the tab to `path`.
    Navigate { path: String },
    /// Split the pane holding `anchor_session_id` and put `session_id` in the new pane.
    PlaceSplit {
        session_id: String,
        anchor_session_id: String,
        /// `row` or `col`; anything else is refused.
        dir: String,
        insert_first: bool,
    },
    /// Select a tab in its pane, as a strip click does.
    SelectTab { session_id: String },
    /// Focus the pane that CONTAINS `session_id`.
    FocusPane { session_id: String },
    /// Move `session_id` into the pane holding `dest_session_id`.
    MoveTab {
        session_id: String,
        dest_session_id: String,
    },
    /// Re-arrange the viewed folder by a preset name.
    Arrange { preset: String },
    /// Soft-close a tab, with the tab ✕'s undo window.
    CloseTab { session_id: String },
    /// Float a session, or clear the float with `off`.
    Spotlight { session_id: String, off: bool },
}

impl LegacyUiCommand {
    /// The command's wire name, for a log line.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Navigate { .. } => "navigate",
            Self::PlaceSplit { .. } => "place_split",
            Self::SelectTab { .. } => "select_tab",
            Self::FocusPane { .. } => "focus_pane",
            Self::MoveTab { .. } => "move_tab",
            Self::Arrange { .. } => "arrange",
            Self::CloseTab { .. } => "close_tab",
            Self::Spotlight { .. } => "spotlight",
        }
    }
}

/// One inbound frame, read.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundUiCommand {
    /// A legacy command and the tab it names, where empty is every tab.
    Legacy {
        target_tab_id: String,
        command: LegacyUiCommand,
    },
    /// The acknowledged apply. Its document is `None` both when the frame
    /// carried none and when the carried one does not decode: either way the
    /// target answers "invalid document", after its folder check.
    ApplyLayout(LayoutApplyCommand),
}

/// Read one queued frame. `None` for a frame naming no command, which v2
/// drops before any targeting.
pub fn read_ui_command_frame(frame: &roost_proto::UiCommandFrame) -> Option<InboundUiCommand> {
    let command = frame.command.as_option()?.command.as_ref()?;
    let legacy = match command {
        WireCommand::ApplyLayout(apply) => {
            let document = apply.document.as_option().and_then(|document| {
                layout_document_from_proto(document)
                    .inspect_err(|error| {
                        tracing::warn!(
                            target: "ui_cc",
                            correlation_id = %frame.correlation_id,
                            reason = %error,
                            "apply-layout document did not decode"
                        );
                    })
                    .ok()
            });
            return Some(InboundUiCommand::ApplyLayout(LayoutApplyCommand {
                target_tab_id: frame.target_tab_id.clone(),
                target_socket_id: frame.target_socket_id.clone(),
                correlation_id: frame.correlation_id.clone(),
                document,
            }));
        }
        WireCommand::Navigate(navigate) => LegacyUiCommand::Navigate {
            path: navigate.path.clone(),
        },
        WireCommand::PlaceSplit(split) => LegacyUiCommand::PlaceSplit {
            session_id: split.session_id.clone(),
            anchor_session_id: split.anchor_session_id.clone(),
            dir: split.dir.clone(),
            insert_first: split.insert_first,
        },
        WireCommand::SelectTab(select) => LegacyUiCommand::SelectTab {
            session_id: select.session_id.clone(),
        },
        WireCommand::FocusPane(focus) => LegacyUiCommand::FocusPane {
            session_id: focus.session_id.clone(),
        },
        WireCommand::MoveTab(moved) => LegacyUiCommand::MoveTab {
            session_id: moved.session_id.clone(),
            dest_session_id: moved.dest_session_id.clone(),
        },
        WireCommand::Arrange(arrange) => LegacyUiCommand::Arrange {
            preset: arrange.preset.clone(),
        },
        WireCommand::CloseTab(close) => LegacyUiCommand::CloseTab {
            session_id: close.session_id.clone(),
        },
        WireCommand::Spotlight(spotlight) => LegacyUiCommand::Spotlight {
            session_id: spotlight.session_id.clone(),
            off: spotlight.off,
        },
    };
    Some(InboundUiCommand::Legacy {
        target_tab_id: frame.target_tab_id.clone(),
        command: legacy,
    })
}

/// Legacy targeting: an empty target broadcasts to every tab. The
/// acknowledged apply never reads this; it answers only its exact target.
pub fn legacy_frame_accepted(target_tab_id: &str, own_tab_id: &str) -> bool {
    target_tab_id.is_empty() || target_tab_id == own_tab_id
}
