//! Session-owned semantic UI requests, view leases and durable frame credits.
//! The connected actor supplies the live ACP identity; browsers cannot select it.
//! Native frame credits and action receipts return only after coordinator acknowledgement.

use super::{actor_types::AcpEvent, emitter::Emitter, session::Session};
use agent_client_protocol::{Agent, Error, v1::ConnectionTo};
use roost_proto::{DOmpUi, WOmpUiActionReceipt};
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};
mod host_services;
mod lifecycle;
mod receipts;
mod reconnect;
mod validation;

use host_services::HostServices;
use receipts::Receipts;
use validation::{fail, notify, send, valid_before, valid_view_id};

const VIEW_LEASE: Duration = Duration::from_secs(90);
const MAX_VIEWS: usize = 64;
const MAX_VIEW_ID_BYTES: usize = 128;
const MAX_REQUEST_ID_BYTES: usize = 128;
const MAX_CLIENT_ID_BYTES: usize = 256;
const MAX_ACTION_BYTES: usize =
    roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_MESSAGE_BYTES;
const MAX_REQUESTS_PER_VIEW: usize = 128;
const MAX_BEFORE_BYTES: usize = 16 * 1024;
const MAX_EDIT_LENGTH: u64 =
    roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_INPUT_BYTES as u64;

/// The authenticated envelope one native action ran under.
#[derive(Clone)]
pub(super) struct ViewOrigin {
    pub client_id: String,
    pub view_id: String,
    pub transport_id: String,
    pub request_id: String,
}

struct ViewLease {
    expires: Instant,
    view_id: String,
    transport_id: String,
    /// Admitted origin request ids, so a late asynchronous callback still owns
    /// the view it started in without trusting anything the child sends.
    requests: VecDeque<String>,
}

#[derive(Default)]
pub(super) struct UiState {
    views: HashMap<String, ViewLease>,
    opened: bool,
    acknowledgements: VecDeque<(u64, Value)>,
    streaming: bool,
    turn_id: Option<String>,
    transport_id: String,
    open_params: Option<Value>,
    receipts: Receipts,
    host_services: HostServices,
}

impl UiState {
    pub fn active(&self) -> bool {
        !self.views.is_empty()
    }
    pub fn streaming(&self) -> bool {
        self.streaming
    }

    pub fn request(
        &mut self,
        request: DOmpUi,
        connection: &ConnectionTo<Agent>,
        session: &Session,
        emitter: &mut Emitter,
        sender: &mpsc::UnboundedSender<AcpEvent>,
    ) {
        // A typed edit/input with no receipt flag still owns a private failure
        // receipt when dispatch refuses it before native handling, while the
        // tracked completion covers a later native rejection. Success never
        // gets one.
        let typed = !request.receipt && validation::typed_action(&request.action_json);
        let wants_failure_receipt = request.receipt || typed;
        let failure_receipt = wants_failure_receipt.then(|| {
            (
                request.request_id.clone(),
                request.client_id.clone(),
                request.view_id.clone(),
                if request.transport_id.is_empty() {
                    self.transport_id.clone()
                } else {
                    request.transport_id.clone()
                },
            )
        });
        if let Err(error) = self.dispatch(request, connection, session, emitter, sender) {
            let message = error.to_string();
            fail(emitter, error);
            if let Some((request_id, client_id, view_id, transport_id)) = failure_receipt {
                // Any admitted receipt-tracked or typed failure gets a terminal
                // receipt; otherwise the browser keeps its in-flight edit/input
                // forever.
                let error = message.chars().take(512).collect();
                emitter.receipt(WOmpUiActionReceipt {
                    conversation_id: emitter.conversation_id.clone(),
                    origin_request_id: request_id,
                    client_id,
                    view_id,
                    transport_id,
                    error,
                    ..Default::default()
                });
            }
        }
    }

    fn dispatch(
        &mut self,
        request: DOmpUi,
        connection: &ConnectionTo<Agent>,
        session: &Session,
        emitter: &mut Emitter,
        sender: &mpsc::UnboundedSender<AcpEvent>,
    ) -> Result<(), Error> {
        if request.conversation_id != emitter.conversation_id
            || request.client_id.is_empty()
            || request.client_id.len() > MAX_CLIENT_ID_BYTES
            || request.request_id.is_empty()
            || request.request_id.len() > MAX_REQUEST_ID_BYTES
            || request.action_json.len() > MAX_ACTION_BYTES
        {
            return Err(Error::invalid_params());
        }
        let action: Value =
            serde_json::from_str(&request.action_json).map_err(|_| Error::invalid_params())?;
        let method = action
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(Error::invalid_params)?;
        let mut params = action.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Err(Error::invalid_params());
        }
        if method == "host_reply" {
            // A reply that no longer owns a pending request is dropped, never replayed.
            let Ok(origin) = self.envelope(&request) else {
                tracing::debug!(conversation_id=%emitter.conversation_id, "stale OMP host service reply dropped");
                return Ok(());
            };
            let Some(reply) = self.host_services.reply(&params, &origin) else {
                tracing::debug!(conversation_id=%emitter.conversation_id, "unmatched OMP host service reply dropped");
                return Ok(());
            };
            let mut payload = reply;
            if let Some(fields) = payload.as_object_mut() {
                fields.insert("sessionId".into(), json!(session.id));
                fields.insert("transportId".into(), json!(self.transport_id));
            }
            return notify(connection, "_omp/ui/host_service_reply", payload);
        }
        // The native method list is the only source of these names, and the
        // notification path takes them as `&'static str`; mapping each valid
        // method to a literal keeps that contract without copying the input.
        let method: &'static str = match method {
            "open" => {
                if !self.open(&request, &params, emitter)? {
                    return Ok(());
                }
                "open"
            }
            "close" => {
                if !self.close(&request, emitter)? {
                    return Ok(());
                }
                "close"
            }
            "event" => {
                let origin = self.envelope(&request)?;
                if params.pointer("/event/ev").and_then(Value::as_str) == Some("ack") {
                    return Err(Error::invalid_params());
                }
                if !self
                    .receipts
                    .track(&request.request_id, origin, request.receipt)
                {
                    return Err(Error::new(-32000, "Too many pending OMP actions."));
                }
                "event"
            }
            "input" => {
                let origin = self.envelope(&request)?;
                if params
                    .get("data")
                    .and_then(Value::as_str)
                    .is_some_and(|data| {
                        data.len()
                            > roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_INPUT_BYTES
                    })
                {
                    return Err(Error::new(-32000, "OMP input exceeds its size limit."));
                }
                if !valid_before(&params) {
                    return Err(Error::invalid_params());
                }
                if !self
                    .receipts
                    .track(&request.request_id, origin, request.receipt)
                {
                    return Err(Error::new(-32000, "Too many pending OMP actions."));
                }
                "input"
            }
            "command" => {
                let _origin = self.envelope(&request)?;
                "command"
            }
            _ => return Err(Error::method_not_found()),
        };
        let fields = params.as_object_mut().ok_or_else(Error::invalid_params)?;
        fields.insert("sessionId".into(), json!(session.id));
        fields.insert("transportId".into(), json!(self.transport_id));
        fields.insert(
            "origin".into(),
            json!({
                "requestId": request.request_id,
                "viewId": request.view_id,
                "transportId": self.transport_id,
            }),
        );
        if let Err(error) = send(connection, method, params, sender) {
            self.receipts.forget(&request.request_id);
            return Err(error);
        }
        Ok(())
    }

    fn open(
        &mut self,
        request: &DOmpUi,
        params: &Value,
        emitter: &mut Emitter,
    ) -> Result<bool, Error> {
        if !valid_view_id(&request.view_id) || !valid_view_id(&request.tab_id) {
            return Err(Error::invalid_params());
        }
        if !self.views.contains_key(&request.client_id) && self.views.len() >= MAX_VIEWS {
            return Err(Error::new(-32000, "Too many open OMP views."));
        }
        let reset = params.get("reset").and_then(Value::as_bool) == Some(true);
        if self.opened && !reset {
            let transport_id = self.transport_id.clone();
            self.lease(request, &transport_id);
            if self.open_params.as_ref() != Some(params) {
                self.open_params = Some(params.clone());
            }
            return Ok(false);
        }
        // A new transport invalidates every binding of the previous generation.
        self.views.clear();
        self.acknowledgements.clear();
        self.receipts.cancel_all();
        self.host_services.cancel_all(emitter);
        self.transport_id = request.request_id.clone();
        self.open_params = Some(params.clone());
        self.opened = true;
        let transport_id = self.transport_id.clone();
        self.lease(request, &transport_id);
        emitter.emit(OmpWorkerEvent::UiReset {
            transport_id: Some(self.transport_id.clone()),
        });
        tracing::info!(
            conversation_id = %emitter.conversation_id,
            tab_id = %request.tab_id,
            "OMP semantic UI opening"
        );
        Ok(true)
    }

    fn close(&mut self, request: &DOmpUi, emitter: &mut Emitter) -> Result<bool, Error> {
        if !self.opened {
            self.views.remove(&request.client_id);
            return Ok(false);
        }
        let origin = self.envelope(request)?;
        let lease = self.views.get(&request.client_id);
        let transport_id = lease
            .map(|lease| lease.transport_id.clone())
            .unwrap_or_else(|| self.transport_id.clone());
        self.views.remove(&request.client_id);
        self.host_services
            .cancel_view(&origin.view_id, &transport_id, emitter);
        self.receipts.cancel_view(&origin.view_id, &transport_id);
        if self.active() || !self.opened {
            return Ok(false);
        }
        self.opened = false;
        self.acknowledgements.clear();
        self.receipts.cancel_transport(&self.transport_id);
        emitter.emit(OmpWorkerEvent::UiReset { transport_id: None });
        tracing::info!(conversation_id=%emitter.conversation_id, "OMP semantic UI closing");
        Ok(true)
    }

    /// Validate the browser's envelope against the live lease and extend it.
    fn envelope(&mut self, request: &DOmpUi) -> Result<ViewOrigin, Error> {
        if !self.opened || !valid_view_id(&request.view_id) {
            return Err(Error::new(
                -32000,
                "Reopen the OMP view before interacting.",
            ));
        }
        if request.transport_id.is_empty() || request.transport_id != self.transport_id {
            return Err(Error::new(
                -32000,
                "Reopen the OMP view before interacting.",
            ));
        }
        let now = Instant::now();
        let lease = self
            .views
            .get_mut(&request.client_id)
            .ok_or_else(|| Error::new(-32000, "Reopen the OMP view before interacting."))?;
        if lease.view_id != request.view_id
            || lease.transport_id != request.transport_id
            || lease.expires <= now
        {
            return Err(Error::new(
                -32000,
                "Reopen the OMP view before interacting.",
            ));
        }
        lease.expires = now + VIEW_LEASE;
        if !lease.requests.iter().any(|id| id == &request.request_id) {
            if lease.requests.len() >= MAX_REQUESTS_PER_VIEW {
                lease.requests.pop_front();
            }
            lease.requests.push_back(request.request_id.clone());
        }
        Ok(ViewOrigin {
            client_id: request.client_id.clone(),
            view_id: lease.view_id.clone(),
            transport_id: lease.transport_id.clone(),
            request_id: request.request_id.clone(),
        })
    }

    fn lease(&mut self, request: &DOmpUi, transport_id: &str) {
        let entry = self
            .views
            .entry(request.client_id.clone())
            .or_insert_with(|| ViewLease {
                expires: Instant::now(),
                view_id: request.view_id.clone(),
                transport_id: transport_id.to_owned(),
                requests: VecDeque::new(),
            });
        entry.expires = Instant::now() + VIEW_LEASE;
        entry.view_id = request.view_id.clone();
        entry.transport_id = transport_id.to_owned();
        if !entry.requests.iter().any(|id| id == &request.request_id) {
            if entry.requests.len() >= MAX_REQUESTS_PER_VIEW {
                entry.requests.pop_front();
            }
            entry.requests.push_back(request.request_id.clone());
        }
    }
}
