//! The one `host_reply` sender for OMP host-service prompts.
//!
//! A verdict is dispatched only while the store still holds the exact service
//! this view saw. The worker injects the original origin from its own pending
//! entry; only successful live preparation may also return transient SDP.

use dioxus::prelude::*;
use roost_client_core::client::agent_chat::{
    OmpActionIdentity, OmpHostService, OmpHostServiceKind,
};
use roost_client_core::store::complete_omp_host_service;
use roost_protocol::wire::agent_chat::{MAX_OMP_LIVE_AUDIO_BYTES, OmpLiveAudioRequest};
use serde_json::{Value, json};

use crate::components::agent_chat::omp_ui_mount_transport::send_omp_ui_request;
use crate::pump::Pump;

/// Whether the store still holds this service under this controller's identity.
pub(super) fn service_is_pending(pump: &Pump, service: &OmpHostService) -> bool {
    let core = pump.core();
    let core = core.borrow();
    core.store()
        .omp_host
        .service(&service.service_id)
        .is_some_and(|pending| pending == service)
}

/// Send a `host_reply`, dropping the prompt once the RPC lands.
///
/// Returns whether the reply was dispatched: a cancellation or a view change
/// wins over a late gesture. `admitted` runs with the RPC's verdict so a file
/// open can wait for admission before navigating.
pub(super) fn send_host_reply(
    pump: Pump,
    service: OmpHostService,
    accepted: bool,
    error: Option<String>,
    value: Option<String>,
    admitted: Option<EventHandler<Result<(), String>>>,
) -> bool {
    if !service_is_pending(&pump, &service) {
        return false;
    }
    let prepare = accepted
        && service.kind == OmpHostServiceKind::LiveAudio
        && OmpLiveAudioRequest::parse_json(&service.value)
            .is_ok_and(|request| request.is_prepare());
    if match value.as_ref() {
        Some(value) => !prepare || value.is_empty() || value.len() > MAX_OMP_LIVE_AUDIO_BYTES,
        None => prepare,
    } {
        return false;
    }
    let mut params = json!({"serviceId": service.service_id, "accepted": accepted});
    if let Some(error) = error.filter(|_| !accepted) {
        params["error"] = Value::String(error);
    }
    if let Some(value) = value {
        params["value"] = Value::String(value);
    }
    let action = json!({"method": "host_reply", "params": params});
    let identity = OmpActionIdentity {
        conversation_id: service.conversation_id,
        view_id: service.view_id,
        transport_id: service.transport_id,
    };
    let service_id = service.service_id;
    let reply_pump = pump.clone();
    let request_id = crate::components::agent_chat::request_ids::new_request_id();
    send_omp_ui_request(
        pump,
        request_id,
        identity,
        action,
        false,
        EventHandler::new(move |result: Result<(), String>| match result {
            Ok(()) => {
                reply_pump.write_store(|store| complete_omp_host_service(store, &service_id));
                if let Some(admitted) = admitted {
                    admitted.call(Ok(()));
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "agent_chat",
                    service_id = %service_id,
                    "OMP host reply did not reach the coordinator"
                );
                if let Some(admitted) = admitted {
                    admitted.call(Err(error));
                }
            }
        }),
    );
    true
}
