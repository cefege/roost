//! Lease expiry, acknowledgements, completion receipts and child teardown.
//! These transitions cancel pending effects before they can apply to a new view.

use super::{
    UiState, ViewOrigin,
    validation::{fail, notify, send},
};
use crate::omp::{actor_types::AcpEvent, emitter::Emitter, session::Session};
use agent_client_protocol::{Agent, Error, v1::ConnectionTo};
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use serde_json::{Value, json};
use tokio::{sync::mpsc, time::Instant};

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;

impl UiState {
    pub fn session_state(
        &mut self,
        payload: Value,
        session: &mut Session,
        emitter: &mut Emitter,
        outstanding: u32,
    ) -> bool {
        if payload.get("sessionId").and_then(Value::as_str) != Some(session.id.0.as_ref()) {
            return false;
        }
        let (Some(resume_id), Some(cwd), Some(streaming)) = (
            payload.get("resumeId").and_then(Value::as_str),
            payload.get("cwd").and_then(Value::as_str),
            payload.get("streaming").and_then(Value::as_bool),
        ) else {
            fail(
                emitter,
                Error::new(-32000, "OMP returned malformed session state."),
            );
            return false;
        };
        if resume_id.is_empty() || cwd.is_empty() {
            fail(
                emitter,
                Error::new(
                    -32000,
                    "OMP returned an empty resume identity or working directory.",
                ),
            );
            return false;
        }
        session.resume_id = resume_id.to_owned();
        session.cwd = cwd.to_owned();
        emitter.emit(OmpWorkerEvent::UiState {
            resume_id: resume_id.to_owned(),
            cwd: cwd.to_owned(),
            streaming,
            reset_history: payload
                .get("resetHistory")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
        if streaming && !self.streaming && outstanding == 0 {
            let request_id = format!("ui:{}:{}", emitter.run_id, emitter.last_sequence());
            self.turn_id = Some(request_id.clone());
            tracing::info!(conversation_id=%emitter.conversation_id,%request_id,"OMP UI turn started");
            emitter.emit(OmpWorkerEvent::TurnStarted { request_id });
        } else if !streaming
            && self.streaming
            && let Some(request_id) = self.turn_id.take()
        {
            tracing::info!(conversation_id=%emitter.conversation_id,%request_id,"OMP UI turn ended");
            emitter.emit(OmpWorkerEvent::TurnEnded {
                request_id,
                stop_reason: payload
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("idle")
                    .to_owned(),
                error: payload
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                outstanding,
            });
        }
        self.streaming = streaming;
        payload.get("restartRequested").and_then(Value::as_bool) == Some(true)
    }

    pub fn acknowledge(
        &mut self,
        sequence: u64,
        connection: &ConnectionTo<Agent>,
        session: &Session,
        sender: &mpsc::UnboundedSender<AcpEvent>,
        emitter: &mut Emitter,
    ) {
        self.receipts.acknowledge(sequence, emitter);
        while self
            .acknowledgements
            .front()
            .is_some_and(|(pending, _)| *pending <= sequence)
        {
            if let Some((_, event)) = self.acknowledgements.pop_front()
                && let Err(error) = send(
                    connection,
                    "event",
                    json!({"sessionId":session.id,"transportId":self.transport_id,"event":event}),
                    sender,
                )
            {
                fail(emitter, error);
            }
        }
    }

    pub fn complete(
        &mut self,
        method: &str,
        transport_id: &str,
        request_id: &str,
        result: Result<Value, Error>,
        emitter: &mut Emitter,
    ) {
        if transport_id != self.transport_id {
            return;
        }
        match result {
            Ok(_) => {
                if method == "input" || method == "event" {
                    self.receipts.complete(request_id, None, emitter);
                }
            }
            Err(error) => {
                if method == "open" {
                    self.opened = false;
                    emitter.emit(OmpWorkerEvent::UiReset { transport_id: None });
                }
                let message = error.to_string();
                // The failure notice is part of what the receipt must fence.
                fail(emitter, error);
                if method == "input" || method == "event" {
                    self.receipts.complete(request_id, Some(message), emitter);
                }
            }
        }
        // UI frames emitted by the handler precede its response on ACP. Flush
        // that ordered batch now instead of waiting for the streaming tick;
        // internal frame acknowledgements have no browser request identity.
        if !request_id.is_empty() && matches!(method, "input" | "event") {
            emitter.flush();
        }
    }

    /// Lease sweep: withdraw the prompts of every view that just went away.
    pub fn expire(
        &mut self,
        connection: &ConnectionTo<Agent>,
        session: &Session,
        sender: &mpsc::UnboundedSender<AcpEvent>,
        emitter: &mut Emitter,
    ) {
        self.host_services.expire(emitter);
        self.receipts.expire(emitter);
        let now = Instant::now();
        let expired = self
            .views
            .iter()
            .filter(|(_, lease)| lease.expires <= now)
            .map(|(client_id, lease)| {
                (
                    client_id.clone(),
                    lease.view_id.clone(),
                    lease.transport_id.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (client_id, view_id, transport_id) in expired {
            self.views.remove(&client_id);
            self.host_services
                .cancel_view(&view_id, &transport_id, emitter);
            self.receipts.cancel_view(&view_id, &transport_id);
        }
        if self.opened && !self.active() {
            self.resources.registry.revoke_transport(
                &self.resources.owner_id,
                &emitter.conversation_id,
                &self.transport_id,
            );
            self.opened = false;
            self.acknowledgements.clear();
            self.receipts.cancel_transport(&self.transport_id);
            emitter.emit(OmpWorkerEvent::UiReset { transport_id: None });
            tracing::info!(conversation_id=%emitter.conversation_id, "OMP semantic UI view leases expired");
            if let Err(error) = send(
                connection,
                "close",
                json!({"sessionId":session.id,"transportId":self.transport_id}),
                sender,
            ) {
                fail(emitter, error);
            }
        }
    }

    /// The ACP child is gone: its transport, prompts and receipts cannot outlive it.
    /// Mounted view leases survive so a replacement child can reattach them.
    pub fn disconnected(&mut self, emitter: &mut Emitter) {
        self.resources.registry.revoke_transport(
            &self.resources.owner_id,
            &emitter.conversation_id,
            &self.transport_id,
        );
        self.host_services.cancel_all(emitter);
        self.receipts.cancel_all();
        self.acknowledgements.clear();
        self.streaming = false;
        self.turn_id = None;
        if self.opened {
            self.opened = false;
            emitter.emit(OmpWorkerEvent::UiReset { transport_id: None });
        }
        self.transport_id.clear();
    }
}

/// Native effect requests and withdrawals bound to the filing view.
impl UiState {
    /// One native effect request; only a live, matching view lease may file it.
    pub fn host_service(
        &mut self,
        payload: Value,
        session: &Session,
        connection: &ConnectionTo<Agent>,
        emitter: &Emitter,
    ) {
        if payload.get("sessionId").and_then(Value::as_str) != Some(session.id.0.as_ref())
            || !self.opened
        {
            return;
        }
        let Some(origin) = payload.get("origin") else {
            return;
        };
        let (Some(request_id), Some(view_id), Some(transport_id)) = (
            origin.get("requestId").and_then(Value::as_str),
            origin.get("viewId").and_then(Value::as_str),
            origin.get("transportId").and_then(Value::as_str),
        ) else {
            return;
        };
        if transport_id != self.transport_id {
            return;
        }
        let Some((client_id, _)) = self.views.iter().find(|(_, lease)| {
            lease.view_id == view_id
                && lease.transport_id == transport_id
                && lease.requests.iter().any(|id| id == request_id)
        }) else {
            return;
        };
        let denial = match payload.get("kind").and_then(Value::as_str) {
            Some("open_stats_dashboard") => {
                let registered = payload
                    .get("value")
                    .and_then(Value::as_str)
                    .and_then(roost_protocol::native_dashboard::DashboardOpen::parse)
                    .is_some_and(|(open, target)| {
                        self.resources.registry.contains(
                            &roost_protocol::native_dashboard::ResourceKey {
                                conversation_id: emitter.conversation_id.clone(),
                                transport_id: transport_id.to_owned(),
                                resource_id: open.resource_id,
                            },
                            &target.host,
                            target.port,
                        )
                    });
                (!registered)
                    .then_some("The stats dashboard is not registered to this OMP transport.")
            }
            Some("open_fork") => {
                let owned = payload
                    .get("value")
                    .and_then(Value::as_str)
                    .and_then(roost_protocol::native_fork::ForkOpen::parse)
                    .is_some_and(|fork| fork.parent_session_id == session.resume_id);
                (!owned).then_some("The fork does not belong to this OMP session.")
            }
            _ => None,
        };
        if let Some(reason) = denial {
            let _ = notify(
                connection,
                "_omp/ui/host_service_reply",
                json!({
                    "sessionId": session.id, "transportId": self.transport_id,
                    "origin": origin, "serviceId": payload.get("serviceId"),
                    "accepted": false, "error": reason,
                }),
            );
            return;
        }
        let admitted = ViewOrigin {
            client_id: client_id.clone(),
            view_id: view_id.to_owned(),
            transport_id: transport_id.to_owned(),
            request_id: request_id.to_owned(),
        };
        if let Err(reason) = self
            .host_services
            .request(&payload, admitted.clone(), emitter)
        {
            tracing::debug!(
                conversation_id = %emitter.conversation_id,
                request_id,
                reason,
                "OMP host service request rejected"
            );
            let service_id = payload
                .get("serviceId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let _ = notify(
                connection,
                "_omp/ui/host_service_reply",
                json!({
                    "sessionId": session.id,
                    "transportId": self.transport_id,
                    "origin": {"requestId": admitted.request_id, "viewId": admitted.view_id, "transportId": admitted.transport_id},
                    "serviceId": service_id,
                    "accepted": false,
                    "error": reason,
                }),
            );
        }
    }

    pub fn host_service_cancel(&mut self, payload: Value, session: &Session, emitter: &Emitter) {
        if payload.get("sessionId").and_then(Value::as_str) != Some(session.id.0.as_ref()) {
            return;
        }
        self.host_services.cancel_native(&payload, emitter);
    }
    pub fn notification(&mut self, mut payload: Value, session: &Session, emitter: &mut Emitter) {
        if payload.get("sessionId").and_then(Value::as_str) != Some(session.id.0.as_ref()) {
            return;
        }
        if !self.opened
            || payload.get("transportId").and_then(Value::as_str)
                != Some(self.transport_id.as_str())
        {
            return;
        }
        let Some(message) = payload.get_mut("message").map(Value::take) else {
            return;
        };
        let acknowledgement = if message.get("verb").and_then(Value::as_str) == Some("f") {
            message.get("body").and_then(|body| Some(json!({"ev":"ack", "sf":body.get("sf")?.as_str()?, "s":body.get("s")?.as_u64()?})))
        } else {
            None
        };
        emitter.emit(OmpWorkerEvent::Ui { message });
        if let Some(acknowledgement) = acknowledgement {
            self.acknowledgements
                .push_back((emitter.last_sequence(), acknowledgement));
        }
    }
}
