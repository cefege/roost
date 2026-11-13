//! Coordinator routing for ephemeral OMP host-service effects and their
//! private native-action receipts. Nothing here is a transcript event, and no
//! effect value is persisted or logged.
//!
//! Worker frames arrive on the link-owner replica and are fanned out on the
//! in-process UI bus, which is the same replica every browser socket connects
//! to under `cluster:links` ownership. A future multi-owner mode must
//! distribute these frames; it must not widen their audience.

#[path = "host_service_kind.rs"]
mod kind;
use kind::HostServiceKind;
#[path = "host_fork.rs"]
pub(super) mod fork;

use super::host_reply::bounded_error;
pub(crate) use super::host_reply::{HostReply, reply};
use super::host_service_store::{self as host_store, PendingHostService};
use super::store as omp_store;
use crate::agent::omp::DashboardCapability;
use crate::coord_core::CoordCore;
use crate::events::bus_messages::UiBusMsg;
use crate::serve::now_ms;
use roost_proto as proto;
use roost_protocol::native_dashboard::{DashboardOpen, capability_path};

/// One clipboard payload or URL per prompt; matches the browser inbox so
/// neither side accepts what the other refuses.
const MAX_HOST_SERVICE_VALUE_BYTES: usize = 1024 * 1024;

/// Route one worker-emitted host service request, or withdraw one.
pub async fn accept_request(
    core: &CoordCore,
    worker_fp: &str,
    request: proto::WOmpHostServiceRequest,
) {
    let now = now_ms();
    let Some(kind) = HostServiceKind::parse(&request.kind) else {
        tracing::warn!(worker_fp, conversation_id = %request.conversation_id,
            "OMP host service dropped: unknown kind");
        return;
    };
    let conversation = match omp_store::find(&core.services.db, &request.conversation_id).await {
        Ok(Some(conversation)) if conversation.worker_fp == worker_fp => conversation,
        Ok(_) => {
            tracing::warn!(worker_fp, conversation_id = %request.conversation_id,
                "OMP host service dropped: worker does not own the conversation");
            return;
        }
        Err(error) => {
            tracing::warn!(conversation_id = %request.conversation_id, %error,
                "OMP host service conversation lookup failed");
            return;
        }
    };
    let admission = match host_store::ui_admission(
        &core.services.db,
        &conversation.id,
        &request.origin_request_id,
        now,
    )
    .await
    {
        Ok(Some(admission)) => admission,
        Ok(None) => {
            tracing::warn!(conversation_id = %conversation.id, service_id = %request.service_id,
                "OMP host service dropped: origin action was not admitted");
            return;
        }
        Err(error) => {
            tracing::warn!(conversation_id = %conversation.id, %error,
                "OMP host service admission lookup failed");
            return;
        }
    };
    if admission.worker_fp != worker_fp
        || admission.client_id != request.client_id
        || admission.view_id != request.view_id
        || admission.transport_id != request.transport_id
        || admission.fingerprint.is_empty()
        || admission.tab_id.is_empty()
    {
        tracing::warn!(conversation_id = %conversation.id, service_id = %request.service_id,
            "OMP host service dropped: origin identity did not match its admission");
        return;
    }

    if request.cancelled {
        core.services
            .omp
            .dashboards
            .revoke_service(&request.service_id);
        if !request.value.is_empty() {
            tracing::warn!(conversation_id = %conversation.id, service_id = %request.service_id,
                "OMP host service cancellation dropped: it carried a value");
            return;
        }
        match host_store::delete_pending(&core.services.db, &request.service_id, &conversation.id)
            .await
        {
            Ok(true) => {
                if matches!(kind, HostServiceKind::OpenFork) {
                    let _ = fork::finish(core, &conversation.id, &request.service_id, false).await;
                }
                publish(
                    core,
                    &admission.fingerprint,
                    &admission.tab_id,
                    proto::OmpHostServiceFrame {
                        service_id: request.service_id,
                        origin_request_id: request.origin_request_id,
                        conversation_id: conversation.id,
                        view_id: admission.view_id,
                        transport_id: admission.transport_id,
                        kind: kind.wire().to_owned(),
                        value: String::new(),
                        cancelled: true,
                        ..Default::default()
                    },
                );
            }
            Ok(false) => tracing::debug!(conversation_id = %conversation.id,
                service_id = %request.service_id,
                "OMP host service cancellation had no pending prompt"),
            Err(error) => tracing::warn!(conversation_id = %conversation.id, %error,
                "OMP host service cancellation failed"),
        }
        return;
    }

    if request.value.len() > MAX_HOST_SERVICE_VALUE_BYTES || !kind.value_is_safe(&request.value) {
        tracing::warn!(conversation_id = %conversation.id, service_id = %request.service_id,
            kind = kind.wire(), "OMP host service dropped: invalid value");
        return;
    }
    match host_store::current_transport(&core.services.db, &conversation.id).await {
        Ok(current) if current == request.transport_id && !current.is_empty() => {}
        Ok(_) => {
            tracing::warn!(conversation_id = %conversation.id, service_id = %request.service_id,
                "OMP host service dropped: the native view is stale");
            return;
        }
        Err(error) => {
            tracing::warn!(conversation_id = %conversation.id, %error,
                "OMP host service transport lookup failed");
            return;
        }
    }
    let dashboard_target = if matches!(kind, HostServiceKind::OpenStatsDashboard) {
        DashboardOpen::parse(&request.value)
    } else {
        None
    };
    let pending = PendingHostService {
        service_id: request.service_id.clone(),
        conversation_id: conversation.id.clone(),
        origin_request_id: request.origin_request_id,
        fingerprint: admission.fingerprint.clone(),
        client_id: admission.client_id,
        tab_id: admission.tab_id.clone(),
        view_id: admission.view_id,
        transport_id: admission.transport_id,
        worker_fp: worker_fp.to_owned(),
        kind: kind.wire().to_owned(),
    };
    match host_store::insert_pending(&core.services.db, &pending, now).await {
        Ok(true) => {
            let fork_value = if matches!(kind, HostServiceKind::OpenFork) {
                match fork::prepare(core, &conversation, &pending, &request.value).await {
                    Ok(value) => Some(value),
                    Err(error) => {
                        let _ = host_store::delete_pending(
                            &core.services.db,
                            &pending.service_id,
                            &conversation.id,
                        )
                        .await;
                        tracing::warn!(conversation_id=%conversation.id, %error, "OMP fork child bootstrap failed");
                        return;
                    }
                }
            } else {
                None
            };
            let value = match dashboard_target {
                Some((open, target)) => {
                    let capability = DashboardCapability {
                        service_id: pending.service_id.clone(),
                        conversation_id: pending.conversation_id.clone(),
                        worker_fp: pending.worker_fp.clone(),
                        fingerprint: pending.fingerprint.clone(),
                        tab_id: pending.tab_id.clone(),
                        view_id: pending.view_id.clone(),
                        transport_id: pending.transport_id.clone(),
                        resource_id: open.resource_id,
                        target,
                        expires_ms: 0,
                        window_start_ms: 0,
                        window_count: 0,
                        active_streams: 0,
                        active_requests: 0,
                    };
                    let Some(token) = core.services.omp.dashboards.mint(capability, now) else {
                        let _ = host_store::delete_pending(
                            &core.services.db,
                            &pending.service_id,
                            &pending.conversation_id,
                        )
                        .await;
                        tracing::warn!(
                            conversation_id = %pending.conversation_id,
                            service_id = %pending.service_id,
                            "OMP host service dropped: no dashboard capability"
                        );
                        return;
                    };
                    format!("{}{}", capability_path(&token), open.fragment)
                }
                None => fork_value.unwrap_or(request.value),
            };
            publish(
                core,
                &admission.fingerprint,
                &admission.tab_id,
                proto::OmpHostServiceFrame {
                    service_id: pending.service_id,
                    origin_request_id: pending.origin_request_id,
                    conversation_id: conversation.id,
                    view_id: pending.view_id,
                    transport_id: pending.transport_id,
                    kind: pending.kind,
                    value,
                    cancelled: false,
                    ..Default::default()
                },
            );
        }
        Ok(false) => tracing::warn!(conversation_id = %conversation.id,
            service_id = %request.service_id,
            "OMP host service dropped: duplicate or too many pending prompts"),
        Err(error) => tracing::warn!(conversation_id = %conversation.id, %error,
            "OMP host service could not be recorded"),
    }
}

/// Forward one native action receipt, fenced by the canonical chat sequence.
pub async fn accept_receipt(
    core: &CoordCore,
    worker_fp: &str,
    receipt: proto::WOmpUiActionReceipt,
) {
    let conversation = match omp_store::find(&core.services.db, &receipt.conversation_id).await {
        Ok(Some(conversation)) if conversation.worker_fp == worker_fp => conversation,
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(conversation_id = %receipt.conversation_id, %error,
                "OMP UI receipt conversation lookup failed");
            return;
        }
    };
    let now = now_ms();
    let admission = match host_store::ui_admission(
        &core.services.db,
        &conversation.id,
        &receipt.origin_request_id,
        now,
    )
    .await
    {
        // Receipt-tracked actions are owed a terminal credit; an untracked
        // input/event action is accepted only when the worker reports a
        // failure, never as a success credit.
        Ok(Some(admission))
            if !admission.receipt_consumed && (admission.receipt || !receipt.error.is_empty()) =>
        {
            admission
        }
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(conversation_id = %conversation.id, %error,
                "OMP UI receipt admission lookup failed");
            return;
        }
    };
    if admission.worker_fp != worker_fp
        || admission.client_id != receipt.client_id
        || admission.view_id != receipt.view_id
        || admission.transport_id != receipt.transport_id
        || admission.tab_id.is_empty()
    {
        return;
    }
    match host_store::current_transport(&core.services.db, &conversation.id).await {
        Ok(current) if current == receipt.transport_id && !current.is_empty() => {}
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(conversation_id = %conversation.id, %error,
                "OMP UI receipt transport lookup failed");
            return;
        }
    }
    match host_store::consume_receipt_admission(
        &core.services.db,
        &conversation.id,
        &receipt.origin_request_id,
        now,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::warn!(conversation_id = %conversation.id, %error,
                "OMP UI receipt consumption failed");
            return;
        }
    }
    let after_seq = host_store::transcript_cursor(&core.services.db, &conversation.id)
        .await
        .unwrap_or(0);
    let error = bounded_error(&receipt.error);
    core.services
        .buses
        .ui_bus
        .publish(UiBusMsg::OmpUiActionReceipt {
            fingerprint: admission.fingerprint,
            tab_id: admission.tab_id,
            frame: proto::OmpUiActionReceiptFrame {
                origin_request_id: receipt.origin_request_id,
                conversation_id: conversation.id,
                view_id: receipt.view_id,
                transport_id: receipt.transport_id,
                after_seq,
                error,
                ..Default::default()
            },
        });
}

/// Forward a duplicate receipt-tracked action's fence so its consumer cannot
/// wait forever on an effect that was deliberately not re-executed.
pub(crate) async fn publish_duplicate_receipt(
    core: &CoordCore,
    fingerprint: &str,
    tab_id: &str,
    conversation_id: &str,
    request_id: &str,
    view_id: &str,
    transport_id: &str,
) {
    let after_seq = host_store::transcript_cursor(&core.services.db, conversation_id)
        .await
        .unwrap_or(0);
    core.services
        .buses
        .ui_bus
        .publish(UiBusMsg::OmpUiActionReceipt {
            fingerprint: fingerprint.to_owned(),
            tab_id: tab_id.to_owned(),
            frame: proto::OmpUiActionReceiptFrame {
                origin_request_id: request_id.to_owned(),
                conversation_id: conversation_id.to_owned(),
                view_id: view_id.to_owned(),
                transport_id: transport_id.to_owned(),
                after_seq,
                error: "duplicate OMP UI action ignored".into(),
                ..Default::default()
            },
        });
}

fn publish(core: &CoordCore, fingerprint: &str, tab_id: &str, frame: proto::OmpHostServiceFrame) {
    core.services
        .buses
        .ui_bus
        .publish(UiBusMsg::OmpHostService {
            fingerprint: fingerprint.to_owned(),
            tab_id: tab_id.to_owned(),
            frame,
        });
}
