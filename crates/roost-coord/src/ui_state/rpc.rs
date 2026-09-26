//! The four browser UI Connect methods: report, list, dispatch, apply.
//!
//! Ported from `apps/coord/src/ui-state/handlers-ui.ts`. Every method requires
//! the tab fence and validates the caller's own text before it reaches the
//! database, the retained state or the UI bus; the apply additionally reserves
//! the target's exact live socket and waits for that tab's acknowledgement.
//!
//! THE RETAINED STATE IS REACHED THROUGH THE CORE, NOT PASSED IN. Each handler
//! takes `(core, caller, request)` -- the shape every Connect method in this
//! crate has -- and reads `core.services.ui_state`. A handler that took the
//! runtime as a parameter could be called with a runtime that is not the
//! coordinator's, and the caller would have no way to tell.
//! THE REPORT'S FINGERPRINT IS THE CALLER'S, never the request's. A tab that
//! could name another browser's fingerprint would write into that browser's
//! retained state and appear in its list, so the identity comes from the
//! verified key and only the tab id comes from the body.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_proto::__buffa::oneof::ui_command::Command;

use crate::coord_core::{Caller, CoordCore};
use crate::auth::principal::require_account_device;
use crate::events::bus_messages::UiBusMsg;
use crate::rpc::service::ok_response;
use crate::ui_state::fence::{
    labels_for_fingerprints, require_bounded_ui_text, require_persisted_sessions, require_tab_fence,
};
use crate::ui_state::layout_apply::LayoutApplyRequest;
use crate::ui_state::layout_proto::canonical_layout_document;
use crate::ui_state::legacy_command::{canonical_legacy_ui_command, legacy_ui_command_session_ids};
use crate::ui_state::limits::{
    UI_ACTIVE_PATH_MAX_UTF8_BYTES, UI_FOLDER_KEY_MAX_UTF8_BYTES, UI_TAB_ID_MAX_UTF8_BYTES,
};
use crate::ui_state::state_owner::UiStateReportError;

/// `CoordinatorService.UiReportState` -- retain and fan out one tab's report.
pub async fn handle_ui_report_state(
    core: &CoordCore,
    caller: &Caller,
    request: proto::UiReportStateRequest,
) -> ServiceResult<proto::UiReportStateResponse> {
    require_account_device(caller)?;
    require_tab_fence(caller, "UiReportState")?;
    require_bounded_ui_text(
        &request.tab_id,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI report tab id",
        true,
    )?;
    require_bounded_ui_text(
        &request.active_path,
        UI_ACTIVE_PATH_MAX_UTF8_BYTES,
        "UI report active path",
        false,
    )?;
    require_bounded_ui_text(
        &request.folder_key,
        UI_FOLDER_KEY_MAX_UTF8_BYTES,
        "UI report folder key",
        false,
    )?;
    let layout_document = match request.layout_document.as_option() {
        Some(document) => {
            let canonical = canonical_layout_document(document).map_err(|error| {
                ConnectError::new(ErrorCode::InvalidArgument, error.to_string())
            })?;
            let session_ids: Vec<String> = canonical
                .bindings
                .iter()
                .map(|binding| binding.session_id.clone())
                .collect();
            require_persisted_sessions(core.services.db.pool(), &session_ids).await?;
            Some(canonical)
        }
        None => None,
    };
    let state = proto::UiReportStateRequest {
        tab_id: request.tab_id,
        active_path: request.active_path,
        folder_key: request.folder_key,
        layout_document: layout_document
            .map(roost_proto::buffa::MessageField::some)
            .unwrap_or_else(roost_proto::buffa::MessageField::none),
        ..Default::default()
    };
    let fingerprint = caller.fingerprint().to_owned();
    core.services
        .ui_state
        .states()
        .report(&fingerprint, &state.tab_id, state.clone())
        .map_err(refuse_report)?;
    core.services.buses.ui_bus.publish(UiBusMsg::State {
        fp: fingerprint,
        tab_id: state.tab_id.clone(),
        state,
    });
    ok_response(proto::UiReportStateResponse::default())
}

/// `CoordinatorService.UiListStates` -- every live tab, with its key's label.
pub async fn handle_ui_list_states(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::UiListStatesRequest,
) -> ServiceResult<proto::UiListStatesResponse> {
    require_account_device(caller)?;
    require_tab_fence(caller, "UiListStates")?;
    let entries = core.services.ui_state.states().list();
    let fingerprints: Vec<String> = entries
        .iter()
        .map(|entry| entry.fingerprint.clone())
        .collect();
    let labels = labels_for_fingerprints(core.services.db.pool(), &fingerprints).await?;
    let tabs = entries
        .into_iter()
        .map(|entry| proto::UiTabState {
            fp: entry.fingerprint.clone(),
            label: labels.get(&entry.fingerprint).cloned().unwrap_or_default(),
            tab_id: entry.tab_id,
            last_ms: u64::try_from(entry.last_ms).unwrap_or(0),
            state: roost_proto::buffa::MessageField::some(entry.state),
            ..Default::default()
        })
        .collect();
    ok_response(proto::UiListStatesResponse {
        tabs,
        ..Default::default()
    })
}

/// `CoordinatorService.UiDispatch` -- relay one legacy command, unreliably.
pub async fn handle_ui_dispatch(
    core: &CoordCore,
    caller: &Caller,
    request: proto::UiDispatchRequest,
) -> ServiceResult<proto::UiDispatchResponse> {
    require_account_device(caller)?;
    require_tab_fence(caller, "UiDispatch")?;
    require_bounded_ui_text(
        &request.target_tab_id,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI dispatch target tab id",
        false,
    )?;
    let command = request.command.as_option().ok_or_else(|| {
        ConnectError::new(ErrorCode::InvalidArgument, "uiDispatch requires a command")
    })?;
    if matches!(command.command, Some(Command::ApplyLayout(_))) {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "uiDispatch does not accept applyLayout",
        ));
    }
    if let Some(Command::Navigate(navigate)) = command.command.as_ref() {
        require_bounded_ui_text(
            &navigate.path,
            UI_ACTIVE_PATH_MAX_UTF8_BYTES,
            "UI navigation path",
            false,
        )?;
    }
    let session_ids = legacy_ui_command_session_ids(command)?;
    require_persisted_sessions(core.services.db.pool(), &session_ids).await?;
    let canonical = canonical_legacy_ui_command(command)?;
    // The count is the live Sync subscriber count, which is an UPPER bound on
    // the tabs that will execute it; zero is the answer a headless caller needs,
    // because it means nobody is listening at all.
    let delivered =
        u32::try_from(core.services.buses.ui_bus.subscriber_count()).unwrap_or(u32::MAX);
    core.services.buses.ui_bus.publish(UiBusMsg::Command {
        target_tab_id: request.target_tab_id,
        command: canonical,
    });
    ok_response(proto::UiDispatchResponse {
        delivered,
        ..Default::default()
    })
}

/// `CoordinatorService.UiApplyLayout` -- one acknowledged apply, to one socket.
///
/// The requested fingerprint is pinned into the live socket reservation, so a
/// second device that later claims the same tab id cannot receive or acknowledge
/// this apply. The call blocks until that tab answers or the reservation
/// expires, which is the difference from `uiDispatch` and the reason this method
/// exists separately from it.
pub async fn handle_ui_apply_layout(
    core: &CoordCore,
    caller: &Caller,
    request: proto::UiApplyLayoutRequest,
) -> ServiceResult<proto::UiApplyLayoutResponse> {
    require_account_device(caller)?;
    require_tab_fence(caller, "UiApplyLayout")?;
    require_bounded_ui_text(
        &request.target_tab_id,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI apply target tab id",
        true,
    )?;
    require_bounded_ui_text(
        &request.target_fingerprint,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI apply target fingerprint",
        true,
    )?;
    let document = request
        .document
        .as_option()
        .ok_or_else(|| ConnectError::new(ErrorCode::InvalidArgument, "layout document is required"))
        .and_then(|document| {
            canonical_layout_document(document)
                .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))
        })?;
    let session_ids: Vec<String> = document
        .bindings
        .iter()
        .map(|binding| binding.session_id.clone())
        .collect();
    require_persisted_sessions(core.services.db.pool(), &session_ids).await?;
    let command = proto::UiCommand {
        command: Some(Command::ApplyLayout(Box::new(proto::UiApplyLayout {
            document: document.clone().into(),
            ..Default::default()
        }))),
        ..Default::default()
    };
    let buses = Arc::clone(&core.services.buses);
    let target_tab_id = request.target_tab_id.clone();
    let requested = core
        .services
        .ui_state
        .layout_applies()
        .request_apply(
            &request.target_fingerprint,
            &request.target_tab_id,
            move |publication| {
                buses.ui_bus.publish(UiBusMsg::Apply {
                    target_tab_id: target_tab_id.clone(),
                    target_socket_id: publication.target.socket_id.clone(),
                    correlation_id: publication.correlation_id.clone(),
                    command: command.clone(),
                });
            },
        )
        .map_err(|capacity| ConnectError::new(ErrorCode::ResourceExhausted, capacity.message()))?;
    let resolution = match requested {
        LayoutApplyRequest::TargetGone(resolution) => resolution,
        LayoutApplyRequest::Pending(pending) => pending.await_resolution().await,
    };
    ok_response(proto::UiApplyLayoutResponse {
        outcome: resolution.outcome.into(),
        correlation_id: resolution.correlation_id,
        reason: resolution.reason,
        ..Default::default()
    })
}

fn refuse_report(error: UiStateReportError) -> ConnectError {
    ConnectError::new(ErrorCode::ResourceExhausted, error.message())
}

/// The Connect method each handler answers, and the function that answers it.
///
/// The integrator's list: every row is one arm of the single `impl
/// CoordinatorService` block in `rpc/service_impl.rs`, so wiring a domain is
/// reading this table rather than matching on names by hand.
pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    ("UiReportState", "ui_state::rpc::handle_ui_report_state"),
    ("UiListStates", "ui_state::rpc::handle_ui_list_states"),
    ("UiDispatch", "ui_state::rpc::handle_ui_dispatch"),
    ("UiApplyLayout", "ui_state::rpc::handle_ui_apply_layout"),
];
