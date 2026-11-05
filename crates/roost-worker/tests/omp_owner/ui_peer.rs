//! Controlled semantic ACP peer for view, credit and generation regressions.
//! Uses the real SDK transport and the real worker conversation owner.
//! Explicit barriers order assertions without wall-clock absence checks.

use agent_client_protocol::{
    Agent, ByteStreams, Client,
    v1::{ConnectionTo, Responder, schema::*},
};
use roost_worker::omp::launcher::{OmpLaunch, OmpLaunchError, OmpLauncher};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{DuplexStream, ReadHalf, WriteHalf},
    sync::mpsc,
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Transport = ByteStreams<Compat<WriteHalf<DuplexStream>>, Compat<ReadHalf<DuplexStream>>>;

#[derive(Debug)]
pub enum Control {
    State { streaming: bool, resume_id: String },
    Frame { transport_id: String, text: String },
    Notify { method: String, params: Value },
}
#[derive(Debug)]
pub struct PeerRequest {
    pub method: String,
    pub params: Value,
}
#[derive(Debug)]
pub struct UiLauncher {
    pub launches: AtomicUsize,
    requests: mpsc::UnboundedSender<PeerRequest>,
    controls: Mutex<Option<mpsc::UnboundedSender<Control>>>,
}
impl UiLauncher {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<PeerRequest>) {
        let (requests, receiver) = mpsc::unbounded_channel();
        (
            Self {
                launches: AtomicUsize::new(0),
                requests,
                controls: Mutex::new(None),
            },
            receiver,
        )
    }
    pub fn control(&self, control: Control) {
        self.controls
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(control)
            .unwrap();
    }
}
impl OmpLauncher for UiLauncher {
    type Transport = Transport;
    fn launch(&self, _cwd: &str) -> Result<OmpLaunch<Transport>, OmpLaunchError> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let (client, agent) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client);
        let (agent_read, agent_write) = tokio::io::split(agent);
        let (control_tx, mut controls) = mpsc::unbounded_channel();
        *self.controls.lock().unwrap() = Some(control_tx);
        let requests = self.requests.clone();
        tokio::spawn(async move {
            let replies = requests.clone();
            Agent.v1().on_receive_notification(async move |notification: ClientNotification, _connection| {
                if let ClientNotification::ExtNotification(notification) = notification {
                    let params: Value =
                        serde_json::from_str(notification.params.get()).unwrap_or(Value::Null);
                    let _ = replies.send(PeerRequest {
                        method: notification.method.to_string(),
                        params,
                    });
                }
                Ok(())
            }, agent_client_protocol::on_receive_notification!())
            .on_receive_request(async move |request:ClientRequest,responder:Responder<Value>,cx:ConnectionTo<Client>|{
                match request {
                    ClientRequest::InitializeRequest(request)=>responder.respond(json!({"protocolVersion":request.protocol_version,"agentCapabilities":{"_meta":{"commandUi":{"version":1,"protocol":"tsp"}}}})),
                    ClientRequest::NewSessionRequest(_)=>responder.respond(json!({"sessionId":"live-handle"})),
                    ClientRequest::LoadSessionRequest(request)=>{
                        requests.send(PeerRequest {method:"load".into(),params:json!({"resumeId":request.session_id})}).unwrap();
                        responder.respond(json!({}))
                    }
                    ClientRequest::ExtMethodRequest(request)=>{
                        let method=request.method.to_string();
                        let params:Value=serde_json::from_str(request.params.get()).unwrap();
                        requests.send(PeerRequest {method:method.clone(),params:params.clone()}).unwrap();
                        if method=="_omp/ui/input" && params["data"]=="fail" {
                            return responder.respond_with_internal_error("native input failed");
                        }
                        if method=="_omp/ui/open" {
                            let transport_id=params["transportId"].as_str().unwrap();
                            emit(&cx,"_omp/ui/message",json!({"sessionId":"live-handle","transportId":transport_id,"message":{"verb":"o","body":{"id":"s:0","mode":"inline"}}}))?;
                            emit(&cx,"_omp/ui/message",json!({"sessionId":"live-handle","transportId":transport_id,"message":{"verb":"f","body":{"sf":"s:0","s":1,"ops":[["add","main","s:0",null,{"id":"main","k":"col"}],["add","entry","main",null,{"id":"entry","k":"text","p":{"text":"fresh"}}]]}}}))?;
                        }
                        responder.respond(json!({}))
                    }
                    _=>responder.respond(json!({})),
                }
            },agent_client_protocol::on_receive_request!())
            .connect_with(ByteStreams::new(agent_write.compat_write(),agent_read.compat()),async move |cx:ConnectionTo<Client>|{
                loop {
                    tokio::select! {
                        _=cx.incoming_closed()=>return Ok(()),
                        control=controls.recv()=>match control {
                            Some(Control::State {streaming,resume_id})=>emit(&cx,"_omp/ui/state",json!({"sessionId":"live-handle","resumeId":resume_id,"cwd":"/work","streaming":streaming,"stopReason":"end_turn"}))?,
                            Some(Control::Frame {transport_id,text})=>{
                                emit(&cx,"_omp/ui/message",json!({"sessionId":"live-handle","transportId":transport_id,"message":{"verb":"f","body":{"sf":"s:0","s":2,"ops":[["text","entry","append",text]]}}}))?;
                                emit(&cx,"_omp/ui/state",json!({"sessionId":"live-handle","resumeId":"live-handle","cwd":"/work","streaming":false}))?;
                            }
                            Some(Control::Notify {method,params})=>{
                                emit(&cx,&method,params)?;
                            }
                            None=>return Ok(()),
                        }
                    }
                }
            }).await.unwrap();
        });
        Ok(OmpLaunch {
            transport: ByteStreams::new(client_write.compat_write(), client_read.compat()),
            description: "semantic ACP peer".into(),
            catalog: None,
        })
    }
}
fn emit(
    cx: &ConnectionTo<Client>,
    method: &str,
    params: Value,
) -> Result<(), agent_client_protocol::Error> {
    cx.send_notification(AgentNotification::ExtNotification(ExtNotification::new(
        method.to_owned(),
        Arc::<serde_json::value::RawValue>::from(serde_json::value::to_raw_value(&params).unwrap()),
    )))
}
