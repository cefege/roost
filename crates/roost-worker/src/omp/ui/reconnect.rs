//! Reattaches browser-owned native views after an ACP child replacement.
//! The conversation actor retains leases and negotiated geometry across reloads.
//! Frame credits and transport identity belong only to the new connection.

use super::{UiState, fail, send};
use crate::omp::{actor_types::AcpEvent, emitter::Emitter, session::Session};
use agent_client_protocol::{Agent, Error, v1::ConnectionTo};
use roost_proto::DOmpUi;
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use serde_json::json;
use tokio::{sync::mpsc, time::Instant};

impl UiState {
    pub fn reconnect(
        &mut self,
        connection: &ConnectionTo<Agent>,
        session: &Session,
        emitter: &mut Emitter,
        sender: &mpsc::UnboundedSender<AcpEvent>,
        initial: Option<DOmpUi>,
    ) {
        self.opened = false;
        self.acknowledgements.clear();
        self.receipts.cancel_all();
        self.host_services.cancel_all(emitter);
        self.streaming = false;
        self.turn_id = None;
        self.views.retain(|_, lease| lease.expires > Instant::now());
        if let Some(request) = initial {
            self.request(request, connection, session, emitter, sender);
            return;
        }
        if self.views.is_empty() {
            return;
        }
        let Some(mut params) = self.open_params.as_deref().cloned() else {
            return;
        };
        let Some(fields) = params.as_object_mut() else {
            return;
        };
        let mut nonce = [0_u8; 16];
        if let Err(error) = getrandom::fill(&mut nonce) {
            fail(
                emitter,
                Error::new(-32000, format!("Could not reopen OMP view: {error}")),
            );
            return;
        }
        self.transport_id = format!("reopen:{:032x}", u128::from_le_bytes(nonce));
        for lease in self.views.values_mut() {
            lease.transport_id = self.transport_id.clone();
        }
        fields.insert("sessionId".into(), json!(session.id));
        fields.insert("transportId".into(), json!(self.transport_id));
        fields.insert(
            "origin".into(),
            json!({"requestId": self.transport_id, "viewId": "", "transportId": self.transport_id}),
        );
        self.opened = true;
        emitter.emit(OmpWorkerEvent::UiReset {
            transport_id: Some(self.transport_id.clone()),
        });
        tracing::info!(conversation_id=%emitter.conversation_id, "OMP semantic views reattaching after process restart");
        if let Err(error) = send(connection, "open", params, sender) {
            self.opened = false;
            fail(emitter, error);
        }
    }
}
