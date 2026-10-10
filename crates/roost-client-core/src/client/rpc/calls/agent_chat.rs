//! UI-direct built-in agent chat and authentication RPCs.

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;
use roost_proto::*;
use roost_protocol::wire::agent_chat::{LoginState, ModelRef, ModelsCatalog, Transcript};

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

simple_call!(SubmitAgentChat,AgentChatSubmitRequest,AgentChatSubmitResponse,(),"AgentChatSubmit",empty; conversation_id:String,text:String,request_id:String);
simple_call!(AbortAgentChat,AgentChatAbortRequest,AgentChatAbortResponse,(),"AgentChatAbort",empty; conversation_id:String);
simple_call!(DeleteAgentChat,AgentChatDeleteRequest,AgentChatDeleteResponse,(),"AgentChatDelete",empty; conversation_id:String);
simple_call!(StartAgentLogin,AgentAuthLoginStartRequest,AgentAuthLoginStartResponse,String,"AgentAuthLoginStart",|response:AgentAuthLoginStartResponse| Ok(response.login_id); provider:String);
simple_call!(RespondAgentLogin,AgentAuthLoginRespondRequest,AgentAuthLoginRespondResponse,(),"AgentAuthLoginRespond",empty; login_id:String,prompt_id:String,value:String);
simple_call!(CancelAgentLogin,AgentAuthLoginCancelRequest,AgentAuthLoginCancelResponse,(),"AgentAuthLoginCancel",empty; login_id:String);
simple_call!(SetAgentApiKey,AgentAuthSetApiKeyRequest,AgentAuthSetApiKeyResponse,(),"AgentAuthSetApiKey",empty; provider:String,api_key:String);
simple_call!(RemoveAgentAccount,AgentAccountRemoveRequest,AgentAccountRemoveResponse,(),"AgentAccountRemove",empty; credential_id:i64);
simple_call!(ListAgentAccounts,AgentAccountsListRequest,AgentAccountsListResponse,String,"AgentAccountsList",|response:AgentAccountsListResponse| Ok(response.accounts_json););
simple_call!(GetAgentUsage,AgentUsageGetRequest,AgentUsageGetResponse,String,"AgentUsageGet",|response:AgentUsageGetResponse| Ok(response.usage_json););
simple_call!(GetAgentSettings,AgentSettingsGetRequest,AgentSettingsGetResponse,String,"AgentSettingsGet",|response:AgentSettingsGetResponse| Ok(response.settings_json););
simple_call!(SetAgentSettings,AgentSettingsSetRequest,AgentSettingsSetResponse,(),"AgentSettingsSet",empty; settings_json:String);
simple_call!(DecideAgentPlan,AgentChatPlanDecideRequest,AgentChatPlanDecideResponse,String,"AgentChatPlanDecide",|response:AgentChatPlanDecideResponse| Ok(response.new_conversation_id); conversation_id:String,item_id:String,decision:String,feedback:String);

#[derive(Debug, Clone, Default)]
pub struct CreateAgentChat {
    pub worker_fp: String,
    pub cwd: String,
    pub model: Option<ModelRef>,
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
                ..Default::default()
            },
        )
    }
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        decode_message(Self::METHOD, body)
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

#[derive(Debug, Clone, Copy, Default)]
pub struct ListAgentModels;
impl UnaryMethod for ListAgentModels {
    const METHOD: &'static str = "AgentModelsList";
    type Response = ModelsCatalog;
    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &AgentModelsListRequest::default())
    }
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: AgentModelsListResponse = decode_message(Self::METHOD, body)?;
        decode_json(Self::METHOD, response.catalog_json)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PollAgentLogin {
    pub login_id: String,
}
impl UnaryMethod for PollAgentLogin {
    const METHOD: &'static str = "AgentAuthLoginPoll";
    type Response = LoginState;
    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AgentAuthLoginPollRequest {
                login_id: self.login_id.clone(),
                ..Default::default()
            },
        )
    }
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: AgentAuthLoginPollResponse = decode_message(Self::METHOD, body)?;
        decode_json(Self::METHOD, response.state_json)
    }
}
