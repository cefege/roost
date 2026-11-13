//! Private ACP registration of actual SDK stats-server resources.
//!
//! The actor supplies the conversation identity and current child transport;
//! a browser URL or a stale child can never create a local HTTP privilege.

use roost_protocol::native_dashboard::ResourceKey;
use serde_json::Value;

use super::{
    super::{emitter::Emitter, session::Session},
    UiState,
};

impl UiState {
    pub fn dashboard_resource(&mut self, payload: Value, session: &Session, emitter: &Emitter) {
        if payload.get("sessionId").and_then(Value::as_str) != Some(session.id.0.as_ref())
            || !self.opened
        {
            return;
        }
        let Some(origin) = payload.get("origin") else {
            return;
        };
        let Some(transport_id) = origin.get("transportId").and_then(Value::as_str) else {
            return;
        };
        if transport_id != self.transport_id {
            return;
        }
        let Some(resource_id) = payload.get("resourceId").and_then(Value::as_str) else {
            return;
        };
        let key = ResourceKey {
            conversation_id: emitter.conversation_id.clone(),
            transport_id: transport_id.to_owned(),
            resource_id: resource_id.to_owned(),
        };
        match payload.get("active").and_then(Value::as_bool) {
            Some(false) => self
                .resources
                .registry
                .revoke(&self.resources.owner_id, &key),
            Some(true) => {
                let admitted = self.views.values().any(|lease| {
                    Some(lease.view_id.as_str()) == origin.get("viewId").and_then(Value::as_str)
                        && lease.transport_id == transport_id
                        && lease.requests.iter().any(|request| {
                            Some(request.as_str())
                                == origin.get("requestId").and_then(Value::as_str)
                        })
                });
                let registered = admitted
                    && payload
                        .get("url")
                        .and_then(Value::as_str)
                        .is_some_and(|url| {
                            self.resources
                                .registry
                                .register(&self.resources.owner_id, key, url)
                        });
                if !registered {
                    tracing::warn!(conversation_id = %emitter.conversation_id, "OMP stats registration rejected");
                }
            }
            None => {}
        }
    }
}
