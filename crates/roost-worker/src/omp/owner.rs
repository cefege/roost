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
    pub fn new(launcher: Arc<L>, uplink: Uplink) -> Self {
        Self {
            launcher,
            uplink,
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
}
impl<L: OmpLauncher> OmpPort for OmpOwner<L> {
    fn prompt(&self, request: roost_proto::DOmpPrompt) {
        let mut conversations = self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let id = request.conversation_id.clone();
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
            ));
            conversations.insert(id.clone(), OmpHandle { commands, task });
        }
        if let Some(actor) = conversations.get(&id) {
            let _ = actor.commands.send(OmpCommand::Prompt(request));
        }
    }
    fn cancel(&self, request: roost_proto::DOmpCancel) {
        self.forward(&request.conversation_id, OmpCommand::Cancel);
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
