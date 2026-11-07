//! UI-direct built-in agent chat and authentication RPCs.

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;
use roost_proto::*;
use roost_protocol::wire::agent_chat::{
    AgentBackend, ChatEvent, ModelRef, OmpSettingsView, Transcript,
    validate_agent_chat_image_payload,
};

macro_rules! simple_call {
 ($name:ident,$req:ty,$wire:ty,$out:ty,$method:literal,$convert:expr; $($field:ident:$field_type:ty),* $(,)?) => {
  #[derive(Debug,Clone,Default)] pub struct $name { $(pub $field:$field_type,)* }
  impl UnaryMethod for $name { const METHOD:&'static str=$method; type Response=$out;
   fn encode_request(&self)->Result<Vec<u8>,RpcCodecError>{#[allow(unused_mut)] let mut request=<$req>::default();$(request.$field=self.$field.clone();)*encode_message(Self::METHOD,&request)}
   fn decode_response(body:&[u8])->Result<Self::Response,RpcCodecError>{let wire:$wire=decode_message(Self::METHOD,body)?;($convert)(wire)}
  }
 }
}
fn empty<T>(_value: T) -> Result<(), RpcCodecError> {
    Ok(())
}
fn decode_json<T: serde::de::DeserializeOwned>(
    method: &'static str,
    text: String,
) -> Result<T, RpcCodecError> {
    serde_json::from_str(&text).map_err(|error| RpcCodecError::MalformedResponse {
        method,
        detail: error.to_string(),
    })
}

#[derive(Debug, Clone, Default)]
pub struct SubmitAgentChat {
    pub conversation_id: String,
    pub text: String,
    pub request_id: String,
    pub when_busy: String,
    pub attachments: Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>,
}
impl UnaryMethod for SubmitAgentChat {
    const METHOD: &'static str = "AgentChatSubmit";
    type Response = ();

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        validate_agent_chat_image_payload(&self.text, &self.attachments).map_err(|error| {
            RpcCodecError::RejectedRequest {
                method: Self::METHOD,
                detail: error.to_string(),
            }
        })?;
        encode_message(
            Self::METHOD,
            &AgentChatSubmitRequest {
                conversation_id: self.conversation_id.clone(),
                text: self.text.clone(),
                request_id: self.request_id.clone(),
                when_busy: self.when_busy.clone(),
                attachments: self
                    .attachments
                    .iter()
                    .map(|attachment| roost_proto::AgentChatAttachment {
                        filename: attachment.filename.clone(),
                        mime_type: attachment.mime_type.clone(),
                        data: attachment.data.clone(),
                        path: attachment.path.clone(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        empty(decode_message::<AgentChatSubmitResponse>(
            Self::METHOD,
            body,
        )?)
    }
}
simple_call!(AbortAgentChat,AgentChatAbortRequest,AgentChatAbortResponse,(),"AgentChatAbort",empty; conversation_id:String,request_id:String,background:Option<bool>);
simple_call!(WithdrawAgentChat,AgentChatWithdrawRequest,AgentChatWithdrawResponse,bool,"AgentChatWithdraw",|response:AgentChatWithdrawResponse| Ok(response.withdrawn); conversation_id:String,submission_id:String,request_id:String);
simple_call!(DeleteAgentChat,AgentChatDeleteRequest,AgentChatDeleteResponse,(),"AgentChatDelete",empty; conversation_id:String);
simple_call!(GetOmpSettings,AgentOmpSettingsGetRequest,AgentOmpSettingsGetResponse,OmpSettingsView,"AgentOmpSettingsGet",|response:AgentOmpSettingsGetResponse| decode_json("AgentOmpSettingsGet", response.settings_json););
simple_call!(SetOmpSettings,AgentOmpSettingsSetRequest,AgentOmpSettingsSetResponse,(),"AgentOmpSettingsSet",empty; scoped_json:String);
simple_call!(OpenOmpSession,AgentOmpSessionOpenRequest,AgentOmpSessionOpenResponse,(),"AgentOmpSessionOpen",empty; conversation_id:String);
#[derive(Debug, Clone, Default)]
pub struct SendOmpUi {
    pub conversation_id: String,
    pub request_id: String,
    pub action: serde_json::Value,
    pub view_id: String,
    /// This document's live UI tab id, the same one the UI feed reports.
    pub tab_id: String,
    /// The native transport the transcript currently names. Empty on open.
    pub transport_id: String,
    /// Whether the native handler must emit a private receipt.
    pub receipt: bool,
}
impl UnaryMethod for SendOmpUi {
    const METHOD: &'static str = "AgentOmpUi";
    type Response = ();

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        let action_json = serde_json::to_string(&self.action).map_err(|error| {
            RpcCodecError::RejectedRequest {
                method: Self::METHOD,
                detail: error.to_string(),
            }
        })?;
        let request = AgentOmpUiRequest {
            conversation_id: self.conversation_id.clone(),
            request_id: self.request_id.clone(),
            action_json,
            view_id: self.view_id.clone(),
            tab_id: self.tab_id.clone(),
            transport_id: self.transport_id.clone(),
            receipt: self.receipt,
            ..Default::default()
        };
        encode_message(Self::METHOD, &request)
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        empty(decode_message::<AgentOmpUiResponse>(Self::METHOD, body)?)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CreateAgentChat {
    pub worker_fp: String,
    pub cwd: String,
    pub model: Option<ModelRef>,
    pub backend: AgentBackend,
}
impl UnaryMethod for CreateAgentChat {
    const METHOD: &'static str = "AgentChatCreate";
    type Response = AgentConversation;
    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AgentChatCreateRequest {
                worker_fp: self.worker_fp.clone(),
                cwd: self.cwd.clone(),
                backend: self.backend.as_wire().to_owned(),
                model_provider: self
                    .model
                    .as_ref()
                    .map_or_else(String::new, |model| model.provider.clone()),
                model_id: self
                    .model
                    .as_ref()
                    .map_or_else(String::new, |model| model.model_id.clone()),
                ..Default::default()
            },
        )
    }
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        decode_message(Self::METHOD, body)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigureAgentChat {
    pub conversation_id: String,
    pub model_provider: Option<String>,
    pub model_id: Option<String>,
    pub thinking_level: Option<String>,
    pub worker_fp: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub read_only: Option<bool>,
    pub mode: Option<String>,
}
impl UnaryMethod for ConfigureAgentChat {
    const METHOD: &'static str = "AgentChatConfigure";
    type Response = AgentConversation;
    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AgentChatConfigureRequest {
                conversation_id: self.conversation_id.clone(),
                model_provider: self.model_provider.clone(),
                model_id: self.model_id.clone(),
                thinking_level: self.thinking_level.clone(),
                worker_fp: self.worker_fp.clone(),
                cwd: self.cwd.clone(),
                title: self.title.clone(),
                read_only: self.read_only,
                mode: self.mode.clone(),
                ..Default::default()
            },
        )
    }
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        decode_message(Self::METHOD, body)
    }
}
#[derive(Debug, Clone, Default)]
pub struct RespondOmpInteraction {
    pub conversation_id: String,
    pub item_id: String,
    pub action: String,
    pub content_json: String,
    pub request_id: String,
}
impl UnaryMethod for RespondOmpInteraction {
    const METHOD: &'static str = "AgentOmpRespond";
    type Response = ();
    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AgentOmpRespondRequest {
                conversation_id: self.conversation_id.clone(),
                item_id: self.item_id.clone(),
                action: self.action.clone(),
                content_json: self.content_json.clone(),
                request_id: self.request_id.clone(),
                ..Default::default()
            },
        )
    }
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        decode_message::<AgentOmpRespondResponse>(Self::METHOD, body).map(|_| ())
    }
}

#[derive(Debug, Clone, Default)]
pub struct GetAgentChatSnapshot {
    pub conversation_id: String,
}
impl UnaryMethod for GetAgentChatSnapshot {
    const METHOD: &'static str = "AgentChatSnapshot";
    type Response = (u64, Transcript);
    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AgentChatSnapshotRequest {
                conversation_id: self.conversation_id.clone(),
                ..Default::default()
            },
        )
    }
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: AgentChatSnapshotResponse = decode_message(Self::METHOD, body)?;
        Ok((
            response.seq,
            decode_json(Self::METHOD, response.transcript_json)?,
        ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct GetAgentChatEvents {
    pub conversation_id: String,
    pub after_cursor: u64,
    pub limit: u32,
}
impl UnaryMethod for GetAgentChatEvents {
    const METHOD: &'static str = "AgentChatEvents";
    type Response = (Vec<(u64, Vec<ChatEvent>)>, bool);

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AgentChatEventsRequest {
                conversation_id: self.conversation_id.clone(),
                after_cursor: self.after_cursor,
                limit: self.limit,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: AgentChatEventsResponse = decode_message(Self::METHOD, body)?;
        let batches = response
            .batches
            .into_iter()
            .map(|batch| Ok((batch.cursor, decode_json(Self::METHOD, batch.events_json)?)))
            .collect::<Result<Vec<_>, RpcCodecError>>()?;
        Ok((batches, response.truncated))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_codec_rejects_aggregate_inline_image_payloads() {
        let oversized = SubmitAgentChat {
            attachments: vec![roost_protocol::wire::agent_chat::AgentChatAttachment {
                filename: "large.png".to_owned(),
                mime_type: "image/png".to_owned(),
                data: vec![
                    0;
                    roost_protocol::wire::agent_chat::MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES
                        as usize
                ],
                path: String::new(),
            }],
            ..Default::default()
        };
        let error = oversized
            .encode_request()
            .expect_err("oversized image should fail before protobuf encoding");
        assert!(error.to_string().contains("limit is"));
    }

    #[test]
    fn submit_codec_keeps_text_only_requests_unrestricted_by_image_budget() {
        let text_only = SubmitAgentChat {
            text: "x".repeat(
                roost_protocol::wire::agent_chat::MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES as usize + 1,
            ),
            ..Default::default()
        };
        assert!(text_only.encode_request().is_ok());
    }
}
