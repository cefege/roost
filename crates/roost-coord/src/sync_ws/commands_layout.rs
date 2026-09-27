//! What a Sync socket's frame did to the UI state: the one place a layout
//! acknowledgement reaches the layout-apply owner, and the one place a tab
//! socket is registered as a live apply target.
//!
//! Owned by the Sync session's caller -- the socket driver -- and called with
//! `sync_ws::CommandOutcome`, because every other outcome belongs to the
//! terminal lane and only a caller that sees them all can route. Split from
//! `commands.rs` for the 400-line cap, not for a boundary: these two functions
//! read the same `ClientContext` the frame gate read, and a caller must not be
//! able to settle a layout result without the gate having run.
//!
//! The target is registered on the way IN and settled on the way OUT, and both
//! directions are fenced on the exact `(fingerprint, tab, socket)` the RPC
//! reserved against. A socket that redials therefore cannot acknowledge an
//! apply meant for its predecessor, and a browser that answers twice settles
//! once.

use crate::sync_ws::commands::{ClientContext, CommandOutcome};
use crate::ui_state::UiStateRuntime;
use crate::ui_state::layout_apply::{
    LayoutApplyTargetGuard, UiLayoutApplyCapacityError, UiLayoutApplyTarget,
};

/// Register the live socket that a tab's layout applies may be reserved on.
///
/// The returned guard IS the registration: dropping it unregisters the target
/// and settles everything reserved on it as target-gone, which is what a closed
/// socket means. A socket with no tab -- a worker's, or a browser that has not
/// named one -- has no layout to be sent, so it registers nothing and `None`
/// says so rather than a guard over a target nothing can reserve against.
pub fn register_layout_target(
    runtime: &UiStateRuntime,
    context: &ClientContext,
    socket_id: &str,
) -> Result<Option<LayoutApplyTargetGuard>, UiLayoutApplyCapacityError> {
    let Some(tab_id) = context.tab_id.as_deref().filter(|_| !context.read_only) else {
        return Ok(None);
    };
    let target = UiLayoutApplyTarget {
        fingerprint: context.fingerprint.clone(),
        tab_id: tab_id.to_owned(),
        socket_id: socket_id.to_owned(),
    };
    let guard = runtime
        .layout_applies()
        .register_target(target.clone())
        .inspect_err(|capacity| {
            tracing::warn!(
                event = "sync-ws",
                action = "layout_target_refused",
                caller_fp = %context.fingerprint,
                socket_id,
                reason = capacity.message(),
                "a tab socket could not be registered for layout applies"
            );
        })?;
    tracing::info!(
        event = "sync-ws",
        action = "layout_target_registered",
        caller_fp = %context.fingerprint,
        tab_id,
        socket_id,
        "this socket is now the one layout applies are reserved on"
    );
    Ok(Some(guard))
}

/// Settle one frame's outcome against the UI runtime, and report whether it
/// did anything.
///
/// The socket driver calls this for EVERY frame's outcome, so there is one
/// place that knows a `LayoutResult` belongs to the layout-apply owner and not
/// to the terminal sink. A `false` is not a failure: most outcomes are not a
/// layout result, and a layout result for a correlation this owner never
/// issued, or from a socket that is no longer the registered target, settles
/// nothing -- which is the fence, not an error to report.
pub fn settle_layout_result(
    runtime: &UiStateRuntime,
    context: &ClientContext,
    socket_id: &str,
    outcome: &CommandOutcome,
) -> bool {
    let CommandOutcome::LayoutResult { tab_id, result } = outcome else {
        return false;
    };
    let source = UiLayoutApplyTarget {
        fingerprint: context.fingerprint.clone(),
        tab_id: tab_id.clone(),
        socket_id: socket_id.to_owned(),
    };
    let accepted = runtime.layout_applies().accept_result(&source, result);
    tracing::info!(
        event = "sync-ws",
        action = "layout_result",
        caller_fp = %context.fingerprint,
        tab_id,
        socket_id,
        correlation_id = %result.correlation_id,
        accepted,
        "a browser answered a layout apply"
    );
    accepted
}
