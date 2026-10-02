//! Keeper write-ordering slots the smoke harness holds: a `query_reply` ticket
//! on a session's channel lane, taken and entered on command, so every later
//! terminal input and resize on that channel queues behind it until released.
//! Driven by `super::commands`; reaches the real lanes through the session
//! table and `ControlLanes`. Ports the admission half of v2
//! `TerminalPeerTestFaultState` (`terminal-peer-test-faults.ts:151-168`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::SessionId;

use super::lock_state;
use crate::session::control_lanes::ControlLanes;
use crate::session::ids::mint_uuid;
use crate::session::keeper_admission::{Admission, AdmissionKind, AdmissionTicket};
use crate::session::table::SessionTable;

/// The held tickets, by the id the harness releases them with.
#[derive(Debug)]
pub struct AdmissionHolds {
    sessions: Arc<SessionTable>,
    lanes: Arc<ControlLanes>,
    held: Mutex<HashMap<String, AdmissionTicket>>,
}

impl AdmissionHolds {
    pub fn new(sessions: Arc<SessionTable>, lanes: Arc<ControlLanes>) -> Self {
        Self {
            sessions,
            lanes,
            held: Mutex::default(),
        }
    }

    /// v2 `holdKeeperAdmission`: answers once the slot is actually held, so
    /// anything the harness sends next queues behind it.
    pub(super) async fn hold(&self, session_id: &str) -> Result<String, String> {
        const UNAVAILABLE: &str = "terminal session is unavailable for admission hold";
        let session = SessionId::try_from(session_id).map_err(|_| UNAVAILABLE.to_owned())?;
        let channel = self
            .sessions
            .channel_of(&session)
            .and_then(|channel| {
                self.sessions
                    .with_channel_record(channel, |record| record.channel_id())
            })
            .ok_or_else(|| UNAVAILABLE.to_owned())?;
        let ticket = match self.lanes.admit(channel, AdmissionKind::QueryReply) {
            Admission::Granted(ticket) => ticket,
            Admission::Refused(reason) => return Err(reason.to_owned()),
        };
        ticket.granted().await;
        let hold_id = mint_uuid().map_err(|error| error.to_string())?;
        lock_state(&self.held).insert(hold_id.clone(), ticket);
        tracing::info!(session_id, %hold_id, "a keeper admission slot is held by the smoke harness");
        Ok(hold_id)
    }

    /// v2 `releaseKeeperAdmission`; an unknown id is ignored.
    pub(super) fn release(&self, hold_id: &str) {
        let Some(ticket) = lock_state(&self.held).remove(hold_id) else {
            return;
        };
        ticket.release();
        tracing::info!(hold_id, "a held keeper admission slot was released");
    }

    /// v2 `dispose`: every held slot goes back.
    pub(super) fn release_all(&self) {
        let held: Vec<AdmissionTicket> = lock_state(&self.held)
            .drain()
            .map(|(_, ticket)| ticket)
            .collect();
        for ticket in held {
            ticket.release();
        }
    }
}
