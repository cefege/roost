//! The keeper's per-frame handlers: the work `Keeper::dispatch` routes to.
//! Owned by the keeper daemon.
//!
//! Split from the dispatch table so the table stays readable as a table. Every
//! function here takes the keeper explicitly, which keeps the split from
//! hiding a mutation: a handler that cannot reach the keeper cannot change it.
//!
//! The contract is `protocol/spec/keeper.md`.

use crate::channel_history::ChannelHistory;
use crate::codec::{CodecError, MuxFrame, MuxFrameType};
use crate::frames::{ChannelBinding, ListChannelsResp, SpawnAck, SpawnErr, SpawnRequest};
use crate::keeper::{Channel, Keeper, resize_reject, result_frame};
use crate::payloads::TerminalState;
use crate::payloads::{
    KeeperHelloRequest, KeeperHelloResponse, KeeperObservation, PtyInRejectReason, PtyInRequest,
    PtyInResult, ResizeRequest, negotiate_features,
};
use crate::pty_channel::{PtyChannel, WriteOutcome};

impl Keeper {
    pub fn hello(&self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        let request: KeeperHelloRequest =
            frame.parse_json().ok_or_else(|| CodecError::BadJson {
                name: "Hello",
                reason: "unreadable".into(),
            })?;
        let features = negotiate_features(&request.requested_features);
        let response = KeeperHelloResponse {
            contract: self.contract.clone(),
            observation: KeeperObservation {
                contract: self.contract.clone(),
                live_channel_count: self.channels.len() as u32,
            },
            features,
        };
        Ok(vec![
            MuxFrame::json(MuxFrameType::HelloResp, 0, &response)
                .expect("a hello response is a small JSON object"),
        ])
    }

    pub fn list_channels(&self) -> Vec<MuxFrame> {
        let mut channels: Vec<ChannelBinding> = self
            .channels
            .values()
            .filter_map(|channel| {
                channel.pty.pid().map(|pid| ChannelBinding {
                    channel_id: channel.pty.channel_id(),
                    pid,
                })
            })
            .collect();
        // Sorted so two workers polling the same keeper see the same order,
        // which a HashMap's iteration order does not promise.
        channels.sort_by_key(|binding| binding.channel_id);
        let response = ListChannelsResp { channels };
        vec![
            MuxFrame::json(MuxFrameType::ListChannelsResp, 0, &response)
                .expect("a channel list is small JSON"),
        ]
    }

    /// Answer a conditional shutdown.
    ///
    /// The check and the answer are one operation, with no await between them,
    /// so a keeper handed a new PTY cannot retire itself out from under the
    /// channel it was just given. That is the whole reason this frame exists
    /// separately from `Shutdown`.
    pub fn shutdown_if_empty(&self) -> Vec<MuxFrame> {
        let (tag, channel) = if self.channels.is_empty() {
            (MuxFrameType::ShutdownIfEmptyAck, 0)
        } else {
            (MuxFrameType::ShutdownIfEmptyReject, 0)
        };
        vec![
            MuxFrame::new(tag, channel, Vec::new())
                .expect("an empty payload is within every frame bound"),
        ]
    }

    pub fn spawn(&mut self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        let request: SpawnRequest = frame.parse_json().ok_or_else(|| CodecError::BadJson {
            name: "Spawn",
            reason: "unreadable".into(),
        })?;

        // A respawn on a live channel replaces it, because that is what a
        // worker reconnecting and re-admitting a channel means. The old PTY is
        // killed rather than leaked: leaving it would keep a process running
        // that nothing can ever reach again.
        if let Some(mut existing) = self.channels.remove(&request.channel_id) {
            existing.pty.kill();
        }

        match PtyChannel::spawn(
            request.channel_id,
            &request.shell_spec,
            request.cols,
            request.rows,
        ) {
            Ok(pty) => {
                let pid = pty.pid().unwrap_or(0);
                self.channels.insert(
                    request.channel_id,
                    Channel {
                        pty,
                        history: ChannelHistory::new(),
                        next_output_seq: 0,
                    },
                );
                let ack = SpawnAck {
                    channel_id: request.channel_id,
                    pid,
                };
                Ok(vec![
                    MuxFrame::json(MuxFrameType::SpawnAck, request.channel_id, &ack)
                        .expect("a spawn ack is small JSON"),
                ])
            }
            Err(err) => {
                tracing::warn!(
                    "keeper: spawn refused channel={} error={err}",
                    request.channel_id
                );
                let failure = SpawnErr {
                    channel_id: request.channel_id,
                    error: err.to_string(),
                };
                Ok(vec![
                    MuxFrame::json(MuxFrameType::SpawnErr, request.channel_id, &failure)
                        .expect("a spawn error is small JSON"),
                ])
            }
        }
    }

    pub fn legacy_input(&mut self, frame: &MuxFrame) -> Vec<MuxFrame> {
        // Unacknowledged by definition: there is no sequence to report, which is
        // why the sequenced form exists.
        if let Some(channel) = self.channels.get_mut(&frame.channel_id) {
            channel.pty.write_input(&frame.payload);
        }
        Vec::new()
    }

    pub fn sequenced_input(&mut self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        let request = PtyInRequest::decode(&frame.payload, "PtyInRequest")?;
        let Some(channel) = self.channels.get_mut(&frame.channel_id) else {
            return Ok(vec![result_frame(
                MuxFrameType::PtyInReject,
                frame.channel_id,
                PtyInResult::Reject {
                    input_seq: request.input_seq,
                    reason: PtyInRejectReason::NoSuchChannel,
                },
            )]);
        };

        let outcome = channel.pty.write_input(&request.bytes);
        let result = match outcome {
            WriteOutcome::Complete { written } => PtyInResult::Ack {
                input_seq: request.input_seq,
                written,
            },
            WriteOutcome::Rejected { reason } => PtyInResult::Reject {
                input_seq: request.input_seq,
                reason,
            },
            // A partial write is the case a client must NOT retry, and saying
            // so is the entire reason this frame exists.
            WriteOutcome::Partial { written, reason } => PtyInResult::Ambiguous {
                input_seq: request.input_seq,
                written,
                reason,
            },
        };
        let tag = match result {
            PtyInResult::Ack { .. } => MuxFrameType::PtyInAck,
            PtyInResult::Reject { .. } => MuxFrameType::PtyInReject,
            PtyInResult::Ambiguous { .. } => MuxFrameType::PtyInAmbiguous,
        };
        Ok(vec![result_frame(tag, frame.channel_id, result)])
    }

    pub fn legacy_resize(&mut self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        #[derive(serde::Deserialize)]
        struct LegacyResize {
            cols: u16,
            rows: u16,
        }
        let request: LegacyResize = frame.parse_json().ok_or_else(|| CodecError::BadJson {
            name: "Resize",
            reason: "unreadable".into(),
        })?;
        // Unacknowledged, so it is given the next sequence: the keeper is the
        // ordering authority, and an unsequenced resize must not be able to
        // overtake a sequenced one that came before it.
        let Some(channel) = self.channels.get_mut(&frame.channel_id) else {
            return Ok(Vec::new());
        };
        let seq = channel.next_output_seq + 1;
        channel.next_output_seq = seq;
        let applied = channel.pty.apply_resize(seq, request.cols, request.rows);
        if let Ok(state) = applied.map(|_| channel.pty.terminal_state()) {
            channel.history.record_resize(seq, state);
        }
        Ok(Vec::new())
    }

    pub fn sequenced_resize(&mut self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        let request = ResizeRequest::decode(&frame.payload, "ResizeRequest")?;
        let Some(channel) = self.channels.get_mut(&frame.channel_id) else {
            // The refusal names the sequence it is refusing. A reject that
            // carried a zero instead would leave the client unable to match it
            // to the request, which is a hang wearing a different hat.
            return Ok(vec![resize_reject(frame.channel_id, request.seq, 1)]);
        };

        match channel
            .pty
            .apply_resize(request.seq, request.cols, request.rows)
        {
            Ok(_) => {
                let state = channel.pty.terminal_state();
                channel.history.record_resize(request.seq, state);
                let ack = ResizeRequest {
                    seq: state.applied_seq,
                    cols: state.cols,
                    rows: state.rows,
                };
                let frame =
                    MuxFrame::new(MuxFrameType::ResizeAck, frame.channel_id, ack.encode()?)?;
                Ok(vec![frame])
            }
            Err(err) => {
                tracing::warn!(
                    "keeper: resize refused channel={} error={err}",
                    frame.channel_id
                );
                Ok(vec![resize_reject(frame.channel_id, request.seq, 1)])
            }
        }
    }

    pub fn resize_status(&mut self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        let state = self.state_of(frame.channel_id);
        let ack = ResizeRequest {
            seq: state.applied_seq,
            cols: state.cols,
            rows: state.rows,
        };
        let frame = MuxFrame::new(MuxFrameType::ResizeAck, frame.channel_id, ack.encode()?)?;
        Ok(vec![frame])
    }

    /// The answer to a worker that lost a `ResizeAck` and has no retained
    /// marker left to ask about. It is answered from LIVE channel state, which
    /// is the only source that cannot itself have been evicted.
    pub fn terminal_state(&mut self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        let state = self.state_of(frame.channel_id);
        Ok(vec![
            MuxFrame::new(
                MuxFrameType::GetTerminalStateResp,
                frame.channel_id,
                state.encode()?,
            )
            .expect("a terminal state payload is within every frame bound"),
        ])
    }

    pub fn state_of(&self, channel_id: u16) -> TerminalState {
        self.channels
            .get(&channel_id)
            .map(|channel| channel.pty.terminal_state())
            // A channel that is gone has no geometry, and reporting the default
            // is better than refusing: the caller's next step is to respawn,
            // and a refusal sends it looking for a cause it cannot act on.
            .unwrap_or(TerminalState {
                applied_seq: 0,
                cols: 80,
                rows: 24,
            })
    }

    pub fn history_records(
        &mut self,
        frame: &MuxFrame,
        legacy_head: Option<u64>,
    ) -> Result<Vec<MuxFrame>, CodecError> {
        let Some(channel) = self.channels.get(&frame.channel_id) else {
            return Ok(Vec::new());
        };
        let payload = match legacy_head {
            Some(_) => channel.history.records().encode()?,
            None => channel.history.records().encode()?,
        };
        Ok(vec![
            MuxFrame::new(
                MuxFrameType::GetHistoryRecordsResp,
                frame.channel_id,
                payload,
            )
            .expect("a history payload is within every frame bound"),
        ])
    }
}
