//! The coordinator-to-tab UI command channel, and the tab's report back.
//!
//! Ports `apps/web/src/lib/{uiCommandCore,uiCommandDispatch,uiLayoutApply,uiStateReport}.ts`.
//! The Sync fold queues frames in `Store::ui_commands`; the UI bridge (roost-web
//! SHELL) drains them into `UiCommandAction`s, runs the acknowledged apply
//! through `client::ui_state`, and reports this tab's state on the cadence here.

pub mod command;
pub mod dispatch;
pub mod layout_map;
pub mod membership;
pub mod report;

pub use command::{InboundUiCommand, LegacyUiCommand, legacy_frame_accepted, read_ui_command_frame};
pub use dispatch::{UiCommandAction, UiCommandScope, drain_ui_commands};
pub use layout_map::{LayoutReshape, apply_ui_command_to_layout, reshape_folder_layout};
pub use membership::{
    FolderMembership, OpenUiSession, folder_live_session_ids, layout_apply_folder,
    open_ui_session, project_folder_membership,
};
pub use report::{
    UI_STATE_REPORT_DEBOUNCE_MS, UI_STATE_REPORT_HEARTBEAT_MS, UiReportRoute, UiStateReport,
    UiStateReportCadence, authoritative_ui_report_session_id, build_ui_state_report,
    session_resolution_owes_report,
};
