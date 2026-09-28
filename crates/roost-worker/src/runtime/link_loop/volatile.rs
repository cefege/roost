//! The volatile half of the coordinator link: the two producers whose newest
//! record SUPERSEDES its own predecessor. Owned by [`super::LinkLoop`], which
//! encodes and drains. Depends on `roost_protocol`'s agent-status and terminal
//! metadata value models and on [`crate::outbox`].
//!
//! v2 gave each one a module of its own — `coord-link-agent-status.ts` and
//! `coord-link-terminal-metadata.ts` — and both do the same thing: hold ONE
//! record per key and replace it in place while the link is applying
//! backpressure, rather than accumulate every version and then ship them all in
//! order. Shipping them all in order walks the coordinator through a
//! replacement edge it has already passed, so the accumulation is not merely
//! wasteful; it is wrong.
//!
//! So both are ONE fold here, [`crate::outbox::Outbox::admit_coalescing`], and
//! the keyed state is the outbox's own rather than a second queue's. The only
//! thing that stays per-producer is the FIELD MERGE, because a title change
//! dropped by backpressure is a title the coordinator never learns, and the
//! merge is a pure function of the last record and the new one.

use std::time::Instant;

use roost_protocol::wire::agent_status::{
    AgentStatus, AgentStatusUpdate, is_identified_agent_status,
};
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::{AgentStatusFrame, CoordWorkerUpstream, TerminalMetadata};

use crate::outbox::{Admitted, Lane};

use super::{AdmitRefusal, LinkLoop};

/// The coalescing key one session's agent status lives under.
///
/// The session, not the occupant: v2 keyed the map by session and carried the
/// occupant inside the record (`occupantKey`), because a session's status is
/// replaced by the next occupant's and there is only ever one retained row for
/// it.
fn agent_status_key(status: &AgentStatusUpdate) -> String {
    status.common.session_id.to_string()
}

impl LinkLoop {
    /// v2's `coord-link-terminal-metadata.ts` sender.
    ///
    /// One record per channel. A title change and an activity change that both
    /// happen while the link is backpressured are MERGED into one frame by
    /// [`merge_terminal_metadata`], because two frames would ship the title
    /// twice and an activity stamp the coordinator has already superseded.
    pub fn send_terminal_metadata(
        &mut self,
        channel_id: ChannelId,
        metadata: &TerminalMetadata,
    ) -> Result<Admitted, AdmitRefusal> {
        let frame = CoordWorkerUpstream::TerminalMetadata(metadata.clone());
        let bytes = self.encode_volatile(&frame, "terminal-metadata")?;
        let key = channel_id.to_string();
        let admitted = self
            .outbox
            .admit_coalescing(
                &key,
                Lane::Control,
                bytes,
                "terminal-metadata",
                Instant::now(),
            )
            .map_err(AdmitRefusal::Outbox)?;
        if let Admitted::Queued | Admitted::Coalesced = admitted {
            self.wake();
        }
        Ok(admitted)
    }

    /// v2's `coord-link-agent-status.ts` sender.
    ///
    /// An UNIDENTIFIED status is refused, not queued: a status with no session
    /// or no occupant is not a status any reader could place, and v2 dropped it
    /// with a `transport.frame_dropped` line for the same reason.
    pub fn send_agent_status(
        &mut self,
        status: &AgentStatusUpdate,
    ) -> Result<Admitted, AdmitRefusal> {
        if !is_identified_agent_status(&status.common) {
            tracing::warn!(
                session = %status.common.session_id,
                revision = status.common.revision,
                "an agent status with no session or occupant was refused rather than queued"
            );
            return Err(AdmitRefusal::UnidentifiedAgentStatus);
        }
        let frame = CoordWorkerUpstream::AgentStatus(AgentStatusFrame {
            status: AgentStatus {
                common: status.common.clone(),
                active: status.active,
            },
        });
        let bytes = self.encode_volatile(&frame, "agent-status")?;
        let key = agent_status_key(status);
        let admitted = self
            .outbox
            .admit_coalescing(&key, Lane::Control, bytes, "agent-status", Instant::now())
            .map_err(AdmitRefusal::Outbox)?;
        if let Admitted::Queued | Admitted::Coalesced = admitted {
            self.wake();
        }
        Ok(admitted)
    }

    /// Encode a volatile frame, or report it as unencodable.
    ///
    /// An encode failure is a LOGGED refusal and never a silent drop: the caller
    /// is a status or a title, and both are worse absent than reported.
    fn encode_volatile(
        &self,
        frame: &CoordWorkerUpstream,
        label: &str,
    ) -> Result<Vec<u8>, AdmitRefusal> {
        let bytes =
            self.wire
                .encode_upstream(frame)
                .map_err(|error| AdmitRefusal::Unencodable {
                    label: label.to_owned(),
                    reason: error.to_string(),
                })?;
        Ok(bytes)
    }
}

/// Fold a new terminal-metadata record into the one already held for a channel.
///
/// A flag survives if EITHER record raised it, and the value that flag names is
/// the newest one that raised it. That asymmetry is the whole rule: a title
/// change and an activity change are independent, so dropping the second frame's
/// `title_changed` because an `activity_changed` arrived with it would leave the
/// coordinator with an activity stamp and a title it has not been told about.
///
/// Pure, so the rule is testable without a link.
pub fn merge_terminal_metadata(
    previous: Option<&TerminalMetadata>,
    update: &TerminalMetadata,
) -> TerminalMetadata {
    let held = previous.filter(|held| held.channel_id == update.channel_id);
    TerminalMetadata {
        channel_id: update.channel_id,
        title_changed: update.title_changed || held.is_some_and(|held| held.title_changed),
        title: if update.title_changed {
            update.title.clone()
        } else {
            held.map_or_else(String::new, |held| held.title.clone())
        },
        activity_changed: update.activity_changed || held.is_some_and(|held| held.activity_changed),
        activity_ts_ms: if update.activity_changed {
            update.activity_ts_ms
        } else {
            held.map_or(0, |held| held.activity_ts_ms)
        },
    }
}
