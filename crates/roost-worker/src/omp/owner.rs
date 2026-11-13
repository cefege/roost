//! Machine owner of one ACP actor per OMP conversation.
//! Downstream ports route synchronously; actors survive coordinator link loss.
//! Owner drop aborts every actor so the SDK drops its supervised child group.

use super::{
    actor::{OmpCommand, conversation_actor},
    launcher::OmpLauncher,
};
use crate::{
    link_ports::{LinkLifecyclePort, OmpPort},
    uplink::Uplink,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{sync::mpsc, task::JoinHandle};

pub struct OmpOwner<L: OmpLauncher> {
    launcher: Arc<L>,
    uplink: Uplink,
    dashboard_resources:
        Arc<crate::browser_commands::native_dashboard::resources::DashboardResources>,
    conversations: Mutex<HashMap<String, OmpHandle>>,
    counter: AtomicU64,
}
struct OmpHandle {
    commands: mpsc::UnboundedSender<OmpCommand>,
    task: JoinHandle<()>,
}
impl<L: OmpLauncher> std::fmt::Debug for OmpOwner<L> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OmpOwner").finish_non_exhaustive()
    }
}
impl<L: OmpLauncher> OmpOwner<L> {
    pub fn new(
        launcher: Arc<L>,
        uplink: Uplink,
        dashboard_resources: Arc<
            crate::browser_commands::native_dashboard::resources::DashboardResources,
        >,
    ) -> Self {
        Self {
            launcher,
            uplink,
            dashboard_resources,
            conversations: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        }
    }
    fn forward(&self, id: &str, command: OmpCommand) {
        if let Some(actor) = self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
        {
            let _ = actor.commands.send(command);
        }
    }
    fn forward_response(&self, request: roost_proto::DOmpRespond) {
        let conversations = self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(actor) = conversations.get(&request.conversation_id) {
            let _ = actor.commands.send(OmpCommand::Respond(request));
        }
    }
    fn start(&self, id: String, command: OmpCommand) {
        let mut conversations = self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if conversations
            .get(&id)
            .is_none_or(|actor| actor.task.is_finished())
        {
            let (commands, inbox) = mpsc::unbounded_channel();
            let counter = self.counter.fetch_add(1, Ordering::Relaxed);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let run_id = format!("r-{stamp}-{counter}");
            let task = tokio::spawn(conversation_actor(
                Arc::clone(&self.launcher),
                self.uplink.clone(),
                id.clone(),
                run_id,
                inbox,
                Arc::clone(&self.dashboard_resources),
            ));
            conversations.insert(id.clone(), OmpHandle { commands, task });
        }
        if let Some(actor) = conversations.get(&id) {
            let _ = actor.commands.send(command);
        }
    }
}
impl<L: OmpLauncher> OmpPort for OmpOwner<L> {
    fn open(&self, request: roost_proto::DOmpOpen) {
        self.start(request.conversation_id.clone(), OmpCommand::Open(request));
    }
    fn prompt(&self, request: roost_proto::DOmpPrompt) {
        self.start(request.conversation_id.clone(), OmpCommand::Prompt(request));
    }
    fn cancel(&self, request: roost_proto::DOmpCancel) {
        self.forward(&request.conversation_id, OmpCommand::Cancel);
    }
    fn respond(&self, request: roost_proto::DOmpRespond) {
        self.forward_response(request);
    }
    fn set_mode(&self, request: roost_proto::DOmpSetMode) {
        self.start(
            request.conversation_id.clone(),
            OmpCommand::SetMode(request),
        );
    }
    fn ui(&self, request: roost_proto::DOmpUi) {
        self.start(request.conversation_id.clone(), OmpCommand::Ui(request));
    }
    fn ack(&self, request: roost_proto::DOmpEventsAck) {
        self.forward(
            &request.conversation_id,
            OmpCommand::Ack {
                run_id: request.run_id,
                seq: request.seq,
            },
        );
    }
    fn close(&self, request: roost_proto::DOmpClose) {
        if let Some(actor) = self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&request.conversation_id)
        {
            let _ = actor.commands.send(OmpCommand::Close);
            actor.task.abort();
            tracing::info!(conversation_id=%request.conversation_id,"OMP conversation closed");
        }
    }
}
impl<L: OmpLauncher> Drop for OmpOwner<L> {
    fn drop(&mut self) {
        for (_, actor) in self
            .conversations
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
        {
            actor.task.abort();
        }
    }
}

pub trait OmpResend: Send + Sync + std::fmt::Debug {
    fn resend_all(&self);
}
impl<L: OmpLauncher> OmpResend for OmpOwner<L> {
    fn resend_all(&self) {
        for actor in self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
        {
            let _ = actor.commands.send(OmpCommand::Resend);
        }
    }
}
#[derive(Debug)]
pub struct OmpLinkLifecycle(pub Arc<dyn OmpResend>);
impl LinkLifecyclePort for OmpLinkLifecycle {
    fn on_open(&self) {}
    fn on_hello_ack(&self, _capabilities: &[String]) {}
    fn on_detach(&self) {}
    fn on_writable(&self) {}
    fn on_snapshot_ready(&self) {
        self.0.resend_all();
    }
}
