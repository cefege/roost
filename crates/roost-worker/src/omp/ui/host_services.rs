//! Browser-owned effects requested by one authenticated native view.
//! Requests never enter the durable transcript; only identifiers and kinds persist here.
//! Every pending entry is bounded, single-use and withdrawn when its view goes away.

use super::ViewOrigin;
use crate::omp::emitter::Emitter;
use roost_proto::WOmpHostServiceRequest;
use serde_json::Value;
use std::collections::HashMap;
use tokio::time::{Duration, Instant};

pub(super) const MAX_PENDING: usize = 32;
const MAX_COPY_BYTES: usize = 1024 * 1024;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_ERROR_CHARS: usize = 512;
const MAX_SERVICE_ID_BYTES: usize = 128;
/// The native side times its own promise out at 30 s and withdraws it here.
pub(super) const HOST_SERVICE_TIMEOUT: Duration = Duration::from_secs(35);

struct PendingService {
    origin: ViewOrigin,
    kind: &'static str,
    deadline: Instant,
}

#[derive(Default)]
pub(super) struct HostServices {
    pending: HashMap<String, PendingService>,
}

impl HostServices {
    /// Admit one native request. The caller has already matched the origin to
    /// a live view lease and the current transport.
    pub fn request(
        &mut self,
        payload: &Value,
        origin: ViewOrigin,
        emitter: &Emitter,
    ) -> Result<(), &'static str> {
        let fields = payload
            .as_object()
            .ok_or("malformed host service request")?;
        let service_id = fields
            .get("serviceId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= MAX_SERVICE_ID_BYTES)
            .ok_or("host service id is empty or too long")?
            .to_owned();
        if self.pending.contains_key(&service_id) {
            return Err("host service id is already pending");
        }
        if self.pending.len() >= MAX_PENDING {
            return Err("too many pending host services");
        }
        let raw_kind = fields
            .get("kind")
            .and_then(Value::as_str)
            .ok_or("host service kind is missing")?;
        let (kind, limit) = match raw_kind {
            "copy_text" => ("copy_text", MAX_COPY_BYTES),
            "open_url" => ("open_url", MAX_URL_BYTES),
            "open_stats_dashboard" => ("open_stats_dashboard", MAX_URL_BYTES),
            _ => return Err("unsupported host service kind"),
        };
        let value = fields
            .get("value")
            .and_then(Value::as_str)
            .ok_or("host service value is missing")?;
        if value.len() > limit
            || (matches!(kind, "open_url" | "open_stats_dashboard") && value.trim().is_empty())
        {
            return Err("host service value is empty or too large");
        }
        if kind == "open_stats_dashboard"
            && roost_protocol::native_dashboard::parse_dashboard_origin(value).is_none()
        {
            return Err("the stats dashboard host is not reachable through the hosted bridge");
        }
        let request = WOmpHostServiceRequest {
            conversation_id: emitter.conversation_id.clone(),
            service_id: service_id.clone(),
            origin_request_id: origin.request_id.clone(),
            client_id: origin.client_id.clone(),
            view_id: origin.view_id.clone(),
            transport_id: origin.transport_id.clone(),
            kind: kind.to_owned(),
            value: value.to_owned(),
            cancelled: false,
            ..Default::default()
        };
        if !emitter.host_service(request) {
            return Err("the coordinator link is unavailable");
        }
        self.pending.insert(
            service_id,
            PendingService {
                origin,
                kind,
                deadline: Instant::now() + HOST_SERVICE_TIMEOUT,
            },
        );
        Ok(())
    }

    /// Consume one browser reply. Returns the native notification payload, or
    /// `None` when the reply is not from the owning live view.
    pub fn reply(&mut self, payload: &Value, origin: &ViewOrigin) -> Option<Value> {
        let fields = payload.as_object()?;
        let service_id = fields.get("serviceId").and_then(Value::as_str)?;
        let pending = self.pending.get(service_id)?;
        // The reply is its own action; only the owning view and transport bind it.
        if pending.origin.client_id != origin.client_id
            || pending.origin.view_id != origin.view_id
            || pending.origin.transport_id != origin.transport_id
        {
            return None;
        }
        let accepted = fields.get("accepted").and_then(Value::as_bool)?;
        let error = fields
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !accepted && error.is_empty() {
            return None;
        }
        let pending = self.pending.remove(service_id)?;
        let error = if accepted {
            ""
        } else {
            &error[..error
                .char_indices()
                .nth(MAX_ERROR_CHARS)
                .map_or(error.len(), |(at, _)| at)]
        };
        Some(serde_json::json!({
            "origin": {
                "requestId": pending.origin.request_id,
                "viewId": pending.origin.view_id,
                "transportId": pending.origin.transport_id,
            },
            "serviceId": service_id,
            "accepted": accepted,
            "error": error,
        }))
    }

    /// The native action withdrew its own request before any reply arrived.
    pub fn cancel_native(&mut self, payload: &Value, emitter: &Emitter) {
        let Some(service_id) = payload
            .get("serviceId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        let origin = payload.get("origin");
        let owns = |pending: &PendingService| {
            origin
                .and_then(|origin| origin.get("requestId"))
                .and_then(Value::as_str)
                .is_some_and(|id| pending.origin.request_id == id)
                && origin
                    .and_then(|origin| origin.get("transportId"))
                    .and_then(Value::as_str)
                    .is_some_and(|id| pending.origin.transport_id == id)
        };
        if let Some(pending) = self.pending.get(&service_id)
            && !owns(pending)
        {
            return;
        }
        if let Some(pending) = self.pending.remove(&service_id) {
            withdraw(&service_id, &pending, emitter);
        }
    }

    /// A view closed or its lease expired: its prompts must disappear too.
    pub fn cancel_view(&mut self, view_id: &str, transport_id: &str, emitter: &Emitter) {
        self.cancel_matching(
            |origin| origin.view_id == view_id && origin.transport_id == transport_id,
            emitter,
        );
    }

    /// A child process, transport or conversation ended: nothing may survive it.
    pub fn cancel_all(&mut self, emitter: &Emitter) {
        self.cancel_matching(|_| true, emitter);
    }

    pub fn expire(&mut self, emitter: &Emitter) {
        let now = Instant::now();
        let expired = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(service_id, _)| service_id.clone())
            .collect::<Vec<_>>();
        for service_id in expired {
            if let Some(pending) = self.pending.remove(&service_id) {
                withdraw(&service_id, &pending, emitter);
            }
        }
    }

    fn cancel_matching(&mut self, matches: impl Fn(&ViewOrigin) -> bool, emitter: &Emitter) {
        let cancelled = self
            .pending
            .iter()
            .filter(|(_, pending)| matches(&pending.origin))
            .map(|(service_id, _)| service_id.clone())
            .collect::<Vec<_>>();
        for service_id in cancelled {
            if let Some(pending) = self.pending.remove(&service_id) {
                withdraw(&service_id, &pending, emitter);
            }
        }
    }
}

fn withdraw(service_id: &str, pending: &PendingService, emitter: &Emitter) {
    let sent = emitter.host_service(WOmpHostServiceRequest {
        conversation_id: emitter.conversation_id.clone(),
        service_id: service_id.to_owned(),
        origin_request_id: pending.origin.request_id.clone(),
        client_id: pending.origin.client_id.clone(),
        view_id: pending.origin.view_id.clone(),
        transport_id: pending.origin.transport_id.clone(),
        kind: pending.kind.to_owned(),
        value: String::new(),
        cancelled: true,
        ..Default::default()
    });
    if !sent {
        tracing::debug!(
            conversation_id = %emitter.conversation_id,
            service_id,
            "host service withdrawal could not reach the coordinator"
        );
    }
}
