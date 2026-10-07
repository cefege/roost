//! The volatile half of the coordinator link: the terminal metadata producer,
//! whose newest record SUPERSEDES its predecessor. Ports v2
//! `transport/coord-link-terminal-metadata.ts` (latest merged record per
//! channel, bounded, dropped on reconnect); agent status lives in
//! `agent_status.rs`. Owned by [`super::LinkLoop`]; the uplink admission routes
//! `TerminalMetadata` frames here, and the link end calls
//! [`LinkLoop::forget_terminal_metadata`]. Depends on `crate::outbox` only.

use std::collections::HashMap;
use std::time::Instant;

use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, TerminalMetadata};

use crate::outbox::{AdmitError, Admitted, Lane};

use super::{AdmitRefusal, LinkLoop};

/// v2 `TERMINAL_METADATA_PENDING_CAP` (`WORKER_SNAPSHOT_MAX_SESSIONS`): at most
/// one pending record per session the snapshot can describe.
pub const TERMINAL_METADATA_PENDING_CAP: usize = 1_024;

/// v2 `TERMINAL_METADATA_PENDING_BYTES_CAP`: 2 KiB per pending record.
pub const TERMINAL_METADATA_PENDING_BYTES_CAP: usize = TERMINAL_METADATA_PENDING_CAP * 2_048;

/// The lane the metadata frames coalesce in.
const TERMINAL_METADATA_LANE: Lane = Lane::Control;

/// The last record admitted for each channel, so the next one can MERGE with it
/// while it is still pending (v2 `pendingByChannel`). The encoded bytes live in
/// the outbox; a remembered record whose frame already drained is ignored.
#[derive(Debug, Default)]
pub struct TerminalMetadataLane {
    held: HashMap<ChannelId, (TerminalMetadata, usize)>,
}

/// The coalescing key one channel's terminal metadata lives under.
fn terminal_metadata_key(channel_id: ChannelId) -> String {
    format!("terminal-metadata:{channel_id}")
}

impl LinkLoop {
    /// v2 `CoordLinkTerminalMetadataOutbox.send`: merge with the channel's
    /// still-pending record, then admit it in place of that record.
    pub fn send_terminal_metadata(
        &mut self,
        channel_id: ChannelId,
        metadata: &TerminalMetadata,
    ) -> Result<Admitted, AdmitRefusal> {
        let key = terminal_metadata_key(channel_id);
        let pending = self.outbox.coalesces(TERMINAL_METADATA_LANE, &key);
        let previous = if pending {
            self.terminal_metadata
                .held
                .get(&channel_id)
                .map(|(held, _)| held)
        } else {
            None
        };
        let merged = merge_terminal_metadata(previous, metadata);
        let frame = CoordWorkerUpstream::TerminalMetadata(merged.clone());
        let bytes = self.encode_volatile(&frame, "terminal-metadata")?;
        if !pending {
            self.refuse_over_metadata_caps(bytes.len())?;
        }
        let encoded = bytes.len();
        let admitted = self
            .outbox
            .admit_coalescing(
                &key,
                TERMINAL_METADATA_LANE,
                bytes,
                "terminal-metadata",
                Instant::now(),
            )
            .map_err(AdmitRefusal::Outbox)?;
        self.terminal_metadata
            .held
            .insert(channel_id, (merged, encoded));
        tracing::trace!(%channel_id, ?admitted, "terminal metadata was admitted to the link");
        if let Admitted::Queued | Admitted::Coalesced = admitted {
            self.wake();
        }
        Ok(admitted)
    }

    /// v2 `disconnect()`/`clear()`: the reconnect's replay re-asserts every
    /// retained fact, so nothing pending survives the socket generation.
    pub fn forget_terminal_metadata(&mut self) {
        let forgotten = self.terminal_metadata.held.len();
        self.terminal_metadata.held.clear();
        if forgotten > 0 {
            tracing::info!(
                forgotten,
                "pending terminal metadata was dropped with its link"
            );
        }
    }

    /// v2's pending-record caps, counted over the records still in the outbox.
    fn refuse_over_metadata_caps(&mut self, incoming: usize) -> Result<(), AdmitRefusal> {
        let outbox = &self.outbox;
        self.terminal_metadata.held.retain(|channel_id, _| {
            outbox.coalesces(TERMINAL_METADATA_LANE, &terminal_metadata_key(*channel_id))
        });
        let pending = self.terminal_metadata.held.len();
        let bytes: usize = self
            .terminal_metadata
            .held
            .values()
            .map(|(_, bytes)| bytes)
            .sum();
        if pending >= TERMINAL_METADATA_PENDING_CAP {
            tracing::warn!(
                pending,
                "terminal metadata dropped: the pending frame cap is reached"
            );
            return Err(AdmitRefusal::Outbox(AdmitError::Full {
                pending,
                cap: TERMINAL_METADATA_PENDING_CAP,
            }));
        }
        if bytes + incoming > TERMINAL_METADATA_PENDING_BYTES_CAP {
            tracing::warn!(
                bytes,
                incoming,
                "terminal metadata dropped: the pending byte cap is reached"
            );
            return Err(AdmitRefusal::Outbox(AdmitError::OverBytes {
                bytes: bytes + incoming,
                cap: TERMINAL_METADATA_PENDING_BYTES_CAP,
            }));
        }
        Ok(())
    }

    /// Encode a volatile frame, or report it as unencodable (a LOGGED refusal).
    fn encode_volatile(
        &self,
        frame: &CoordWorkerUpstream,
        label: &str,
    ) -> Result<Vec<u8>, AdmitRefusal> {
        self.wire
            .encode_upstream(frame)
            .map_err(|error| AdmitRefusal::Unencodable {
                label: label.to_owned(),
                reason: error.to_string(),
            })
    }
}

/// Fold a new terminal-metadata record into the one still pending for a
/// channel (v2 `coord-link-terminal-metadata.ts:40-50`).
///
/// A flag survives if EITHER record raised it, and the value it names is the
/// newest one that raised it: a title change and an activity change are
/// independent, so an activity-only record must not erase a pending title.
pub fn merge_terminal_metadata(
    previous: Option<&TerminalMetadata>,
    update: &TerminalMetadata,
) -> TerminalMetadata {
    let held = previous.filter(|held| held.channel_id == update.channel_id);
    let clipboard_changed =
        update.clipboard_changed || held.is_some_and(|held| held.clipboard_changed);
    let command_finished =
        update.command_finished || held.is_some_and(|held| held.command_finished);
    let command_update = update.command_finished;
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
        clipboard_changed,
        clipboard: if update.clipboard_changed {
            update.clipboard.clone()
        } else {
            held.map_or_else(String::new, |held| held.clipboard.clone())
        },
        command_finished,
        command_exit_code: if command_update {
            update.command_exit_code
        } else {
            held.and_then(|held| held.command_exit_code)
        },
        command_duration_ms: if command_update {
            update.command_duration_ms
        } else {
            held.map_or(0, |held| held.command_duration_ms)
        },
    }
}
