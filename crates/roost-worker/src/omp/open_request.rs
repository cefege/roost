//! Services browser decisions while ACP initialization requests are in flight.
//! Startup extensions can hold NewSession until a user answers a form.

use super::{
    actor::OmpCommand,
    actor_types::AcpEvent,
    emitter::Emitter,
    interaction_pending::{self, PendingInteractions},
};
use agent_client_protocol::Error;
use std::{collections::VecDeque, future::Future};
use tokio::sync::mpsc;

pub(super) struct OpenRequestPump<'a> {
    pub events: &'a mut mpsc::UnboundedReceiver<AcpEvent>,
    pub inbox: &'a mut mpsc::UnboundedReceiver<OmpCommand>,
    pub pending: &'a mut PendingInteractions,
    pub deferred: &'a mut VecDeque<OmpCommand>,
    pub emitter: &'a mut Emitter,
}

impl OpenRequestPump<'_> {
    pub async fn service<T, F>(&mut self, future: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        let Self {
            events,
            inbox,
            pending,
            deferred,
            emitter,
        } = self;
        tokio::pin!(future);
        let mut events_closed = false;
        loop {
            tokio::select! {
                biased;
                command = inbox.recv() => match command {
                    Some(OmpCommand::Respond(response)) => {
                        interaction_pending::respond_interaction(response, pending, emitter);
                    }
                    Some(OmpCommand::Cancel | OmpCommand::Close) | None => {
                        interaction_pending::clear_interactions(pending, emitter);
                        return Err(Error::new(-32800, "OMP startup was stopped"));
                    }
                    Some(command) => deferred.push_back(command),
                },
                result = &mut future => {
                    while let Ok(event) = events.try_recv() {
                        interaction_pending::consume_open_event(event, pending, emitter);
                    }
                    loop {
                        match inbox.try_recv() {
                            Ok(OmpCommand::Respond(response)) => {
                                interaction_pending::respond_interaction(response, pending, emitter);
                            }
                            Ok(OmpCommand::Cancel | OmpCommand::Close)
                            | Err(mpsc::error::TryRecvError::Disconnected) => {
                                interaction_pending::clear_interactions(pending, emitter);
                                return Err(Error::new(-32800, "OMP startup was stopped"));
                            }
                            Ok(command) => deferred.push_back(command),
                            Err(mpsc::error::TryRecvError::Empty) => return result,
                        }
                    }
                },
                event = events.recv(), if !events_closed => match event {
                    Some(event) => interaction_pending::consume_open_event(event, pending, emitter),
                    None => events_closed = true,
                },
            }
        }
    }
}
