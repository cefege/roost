//! Bounds, typed-action classification and transport helpers shared by the
//! semantic UI state machine. Nothing here logs or echoes browser payload text.

use super::{MAX_BEFORE_BYTES, MAX_EDIT_LENGTH, MAX_VIEW_ID_BYTES};
use crate::omp::{actor_types::AcpEvent, emitter::Emitter};
use agent_client_protocol::{
    Agent, Error,
    v1::{
        ConnectionTo,
        schema::{ClientNotification, ClientRequest, ExtNotification, ExtRequest},
    },
};
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(super) fn valid_view_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_VIEW_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
}

/// Whether a browser action is a typed input or event. Only the method field
/// is decoded, so the payload is not materialised a second time; a duplicate
/// `method` resolves to its last value, as the dispatch parse does.
pub(super) fn typed_action(action_json: &str) -> bool {
    struct MethodOnly(Option<String>);

    impl<'de> serde::Deserialize<'de> for MethodOnly {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct MethodField;

            impl<'de> serde::de::Visitor<'de> for MethodField {
                type Value = MethodOnly;

                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("a semantic UI action object")
                }

                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut fields: A,
                ) -> Result<MethodOnly, A::Error> {
                    let mut method = None;
                    while let Some(field) = fields.next_key::<String>()? {
                        if field == "method" {
                            method = fields.next_value::<Option<String>>()?;
                        } else {
                            fields.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                    Ok(MethodOnly(method))
                }
            }

            deserializer.deserialize_map(MethodField)
        }
    }

    serde_json::from_str::<MethodOnly>(action_json)
        .ok()
        .and_then(|action| action.0)
        .is_some_and(|method| matches!(method.as_str(), "event" | "input"))
}

/// The browser may prepend one caret-only edit; nothing else rides with input.
pub(super) fn valid_before(params: &Value) -> bool {
    let Some(before) = params.get("before") else {
        return true;
    };
    let Some(fields) = before.as_object() else {
        return false;
    };
    if fields.get("ev").and_then(Value::as_str) != Some("edit") {
        return false;
    }
    let number = |name: &str| {
        fields
            .get(name)
            .and_then(Value::as_u64)
            .filter(|value| *value <= MAX_EDIT_LENGTH)
    };
    number("from").is_some()
        && number("to").is_some()
        && number("len").is_some()
        && number("cursor").is_some()
        && fields
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.len() <= MAX_BEFORE_BYTES && !text.contains('\0'))
        && fields.len() <= 8
}

pub(super) fn send(
    connection: &ConnectionTo<Agent>,
    method: &'static str,
    params: Value,
    sender: &mpsc::UnboundedSender<AcpEvent>,
) -> Result<(), Error> {
    let transport_id = params
        .get("transportId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let request_id = params
        .get("origin")
        .and_then(|origin| origin.get("requestId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let raw = serde_json::value::to_raw_value(&params).map_err(|_| Error::invalid_params())?;
    let request = ClientRequest::ExtMethodRequest(ExtRequest::new(
        format!("_omp/ui/{method}"),
        Arc::from(raw),
    ));
    let sender = sender.clone();
    connection
        .send_request(request)
        .on_receiving_result(async move |result| {
            let _ = sender.send(AcpEvent::UiCompleted {
                method,
                transport_id,
                request_id,
                result,
            });
            Ok(())
        })
}

pub(super) fn notify(
    connection: &ConnectionTo<Agent>,
    method: &'static str,
    params: Value,
) -> Result<(), Error> {
    let raw = serde_json::value::to_raw_value(&params).map_err(|_| Error::invalid_params())?;
    connection.send_notification(ClientNotification::ExtNotification(ExtNotification::new(
        method.to_owned(),
        Arc::from(raw),
    )))
}

pub(super) fn fail(emitter: &mut Emitter, error: Error) {
    tracing::warn!(conversation_id=%emitter.conversation_id, %error, "OMP semantic UI request failed");
    emitter.emit(OmpWorkerEvent::UiFailed {
        error: error.to_string(),
    });
}
