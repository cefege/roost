//! The keeper's channel registry and frame dispatcher: everything the socket
//! loop does, minus the socket. Owned by the keeper daemon.
//!
//! The split is deliberate. A dispatcher that takes a frame and returns frames
//! can be driven by a test with no listener, no socket, and no timing, which is
//! what makes the protocol's edge cases testable at all — the ones that need a
//! real second endpoint to reproduce are the ones nobody writes tests for.
//!
//! The contract is `protocol/spec/keeper.md`.

use std::collections::HashMap;

use crate::channel_history::ChannelHistory;
use crate::codec::{CodecError, MuxFrame, MuxFrameType, write_sequence};
use crate::frames::ExitFrame;
use crate::history::HistoryRecords;
use crate::payloads::{
    KEEPER_PROTOCOL_VERSION, KeeperContractV1, KeeperFeature, PtyInRejectReason, PtyInResult,
};
use crate::pty_channel::PtyChannel;

/// The keeper's own version, reported in the `Hello` contract. Overridden at
/// build time from the crate version so it cannot drift.
pub fn keeper_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The SHA-256 of a keeper binary, lowercase hex, 64 characters.
///
/// Three things this must be, and an earlier draft got all three wrong:
///
/// It is SHA-256 because `roost_protocol`'s contract validator calls
/// `hex_of_len(digest, SHA256_DIGEST_LENGTH)` where that length is 64. A
/// shorter digest can never validate against the wire contract.
///
/// It is a STABLE hash, not `DefaultHasher`. The whole point of the field is
/// that two builds of one source produce the same digest, so a deploy is
/// admitted as "same keeper binary, keep the PTYs". `DefaultHasher` is
/// explicitly documented as not stable across toolchain releases, so a
/// toolchain bump would read as "the keeper changed" and strand every live PTY
/// on every machine — with no code change and no operator action.
///
/// It takes the binary PATH rather than reading `current_exe()`. In the
/// keeper daemon those are the same file, but this function is also called
/// from the `roost` binary to report a contract, and there `current_exe()` is
/// `roost` — the digest would describe the wrong program.
pub fn implementation_digest_of(binary: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(binary).ok()?;
    // `LowerHex` is not implemented for sha2's array wrapper, so the digest is
    // rendered byte by byte. Lowercase and two digits per byte is what
    // `hex_of_len(.., 64)` accepts.
    let digest = Sha256::digest(&bytes);
    Some(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The digest of the running keeper binary, when it can be read.
pub fn implementation_digest() -> Option<String> {
    implementation_digest_of(&std::env::current_exe().ok()?)
}

/// Every live channel the keeper owns.
pub struct Keeper {
    pub(crate) channels: HashMap<u16, Channel>,
    /// The contract reported at `Hello`, computed once because digesting the
    /// binary on every handshake would be a denial-of-service vector.
    pub(crate) contract: KeeperContractV1,
}

/// One channel: its PTY, its retained history, and the sequence the keeper
/// stamps on the next thing it emits.
pub(crate) struct Channel {
    pub(crate) pty: PtyChannel,
    pub(crate) history: ChannelHistory,
    /// The next sequence the keeper will stamp on this channel's output. It
    /// advances for every chunk the keeper EMITS, which is what makes the
    /// history a log of what a client could have seen rather than of what the
    /// program wrote.
    pub(crate) next_output_seq: u64,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("pty", &self.pty)
            .field("head_seq", &self.history.head_seq())
            .field("next_output_seq", &self.next_output_seq)
            .finish()
    }
}

/// Feature names in the order the wire validator requires.
fn sorted_feature_names(features: &[KeeperFeature]) -> Vec<String> {
    let mut names: Vec<String> = features.iter().map(|f| f.wire_name().to_string()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The contract this keeper reports: the protocol's own shape, with the digest
/// present only when the binary could actually be read.
fn contract() -> KeeperContractV1 {
    KeeperContractV1 {
        protocol_version: KEEPER_PROTOCOL_VERSION,
        // SORTED, because the protocol validator requires it
        // (`validate_sorted_features`) and the declaration order of the enum is
        // not alphabetical. Advertising them in enum order produced a contract
        // the wire rejected outright.
        supported_features: sorted_feature_names(&KeeperFeature::SUPPORTED),
        required_features: sorted_feature_names(&KeeperFeature::REQUIRED),
        implementation_digest: implementation_digest(),
        // `as_str`, the wire spelling (darwin/linux/win32) — NOT
        // `display_name`, which is for humans and which the validator rejects.
        platform: roost_platform::HostPlatform::current()
            .map(|platform| platform.as_str().to_string())
            .unwrap_or_else(|| std::env::consts::OS.to_string()),
        arch: std::env::consts::ARCH.to_string(),
        // `roost_host` owns the build identity, including the dev stamp for
        // an un-stamped build. Reading the env var again here would be a second
        // answer to "what sha is this" and the two would disagree.
        build_sha: roost_host::build_identity(&roost_host::ProcessEnv::new()).build_sha,
    }
}

impl Default for Keeper {
    fn default() -> Self {
        Self::new()
    }
}

impl Keeper {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            contract: self::contract(),
        }
    }

    /// The live channels, for `ListChannels` and for the `ShutdownIfEmpty`
    /// check.
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn channel_ids(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self.channels.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Handle one frame, returning everything the keeper owes the worker.
    ///
    /// Returning a Vec rather than writing to a socket is what lets a test
    /// drive every branch, and what makes "a frame produces exactly these
    /// frames" checkable.
    pub fn handle(&mut self, frame: &MuxFrame) -> Vec<MuxFrame> {
        match self.dispatch(frame) {
            Ok(frames) => frames,
            // A malformed payload is answered rather than dropped, because a
            // worker waiting on a reply with no answer is the silent-spawn-hang
            // class this protocol already has an incident for.
            Err(err) => self.reject(frame, &err),
        }
    }

    fn reject(&mut self, frame: &MuxFrame, err: &CodecError) -> Vec<MuxFrame> {
        match frame.frame_type {
            MuxFrameType::PtyInRequest => {
                // Nothing was written, so the client may safely resend.
                let result = PtyInResult::Reject {
                    input_seq: 0,
                    reason: PtyInRejectReason::NoSuchChannel,
                };
                vec![
                    MuxFrame::new(MuxFrameType::PtyInReject, frame.channel_id, result.encode())
                        .expect("a 13-byte payload is within every frame bound"),
                ]
            }
            _ => {
                // A frame the keeper cannot parse has no sequence to answer
                // with. Closing the connection is the only honest response: a
                // silent drop leaves the worker waiting forever.
                tracing::warn!(
                    "keeper: unparseable frame type={:?} channel={} error={err}",
                    frame.frame_type,
                    frame.channel_id
                );
                Vec::new()
            }
        }
    }

    fn dispatch(&mut self, frame: &MuxFrame) -> Result<Vec<MuxFrame>, CodecError> {
        // The control lane is the only place these are legal. A client that
        // sends one per-channel is confused about the protocol, and answering
        // would hide that.
        let control_only = |frame: &MuxFrame| frame.channel_id == 0;
        let per_channel = |frame: &MuxFrame| frame.channel_id != 0;

        Ok(match frame.frame_type {
            MuxFrameType::Ping => vec![
                MuxFrame::new(MuxFrameType::Pong, 0, Vec::new())
                    .expect("an empty payload is within every frame bound"),
            ],
            MuxFrameType::Pong => Vec::new(),

            MuxFrameType::Hello if control_only(frame) => self.hello(frame)?,
            MuxFrameType::ListChannels if control_only(frame) => self.list_channels(),
            MuxFrameType::Shutdown if control_only(frame) => {
                vec![
                    MuxFrame::new(MuxFrameType::ShutdownAck, 0, Vec::new())
                        .expect("an empty payload is within every frame bound"),
                ]
            }
            MuxFrameType::ShutdownIfEmpty if control_only(frame) => self.shutdown_if_empty(),

            MuxFrameType::Spawn if per_channel(frame) => self.spawn(frame)?,
            MuxFrameType::PtyIn if per_channel(frame) => self.legacy_input(frame),
            MuxFrameType::PtyInRequest if per_channel(frame) => self.sequenced_input(frame)?,
            MuxFrameType::Resize if per_channel(frame) => self.legacy_resize(frame)?,
            MuxFrameType::ResizeRequest if per_channel(frame) => self.sequenced_resize(frame)?,
            MuxFrameType::ResizeStatus if per_channel(frame) => self.resize_status(frame)?,
            MuxFrameType::GetTerminalState if per_channel(frame) => self.terminal_state(frame)?,
            MuxFrameType::GetHistoryRecords if per_channel(frame) => {
                self.history_records(frame, None)?
            }
            MuxFrameType::GetHistory if per_channel(frame) => {
                self.history_records(frame, Some(u64::MAX))?
            }
            MuxFrameType::KillChild if per_channel(frame) => {
                if let Some(channel) = self.channels.get_mut(&frame.channel_id) {
                    channel.pty.kill();
                }
                Vec::new()
            }

            _ => {
                // A tag the keeper does not serve, or one sent on the wrong
                // lane. Ignoring it is right: the worker is newer or older, and
                // neither deserves a connection dropped over a frame the other
                // side will simply not send again.
                Vec::new()
            }
        })
    }

    /// Drain whatever output is ready on every channel, stamping each chunk
    /// with the next sequence and retaining it.
    ///
    /// Called by the socket loop on a tick. Returns nothing for a channel with
    /// nothing to say, which is the common case and must stay cheap: a keeper
    /// that allocates per idle channel is a keeper that burns a core.
    pub fn drain_output(&mut self, limit: usize) -> Vec<MuxFrame> {
        let mut frames = Vec::new();
        for (channel_id, channel) in self.channels.iter_mut() {
            let Some(bytes) = channel.pty.read_output(limit) else {
                continue;
            };
            channel.next_output_seq += 1;
            let seq = channel.next_output_seq;
            channel.history.record_output(seq, &bytes);
            frames.push(
                MuxFrame::new(MuxFrameType::PtyOut, *channel_id, bytes)
                    .expect("a drained chunk is within the read limit"),
            );
        }
        frames
    }

    /// Channels whose child has exited and whose output is fully drained.
    ///
    /// Reported once each, then the channel is dropped: a channel left behind
    /// would pin its history and answer `ListChannels` with a process that is
    /// gone, which is exactly the lie that makes cross-process resume unsafe.
    pub fn reap_exited(&mut self) -> Vec<MuxFrame> {
        let mut exits = Vec::new();
        let finished: Vec<(u16, Option<i32>)> = self
            .channels
            .iter_mut()
            .filter_map(|(channel_id, channel)| {
                let status = channel.pty.exited()?;
                if !channel.pty.output_closed() {
                    return None;
                }
                // `portable-pty` exposes only a numeric code, so a child
                // killed by a signal is indistinguishable from one that
                // exited 0-and-failed. Reporting the code this build actually
                // has beats inventing a null the wire reserves for a keeper
                // that can really tell the two apart.
                Some((*channel_id, Some(status.exit_code() as i32)))
            })
            .collect();
        for (channel_id, exit_code) in finished {
            self.channels.remove(&channel_id);
            exits.push(
                MuxFrame::json(MuxFrameType::Exit, channel_id, &ExitFrame { exit_code })
                    .expect("an exit frame is small JSON"),
            );
        }
        exits
    }
}

/// A resize refusal, carrying the sequence it refuses so the client can match
/// it to the request that provoked it.
pub(crate) fn resize_reject(channel_id: u16, seq: u64, reason: u8) -> MuxFrame {
    let mut payload = Vec::with_capacity(9);
    write_sequence(&mut payload, seq);
    payload.push(reason);
    MuxFrame::new(MuxFrameType::ResizeReject, channel_id, payload)
        .expect("a 9-byte payload is within every frame bound")
}

pub(crate) fn result_frame(tag: MuxFrameType, channel_id: u16, result: PtyInResult) -> MuxFrame {
    MuxFrame::new(tag, channel_id, result.encode())
        .expect("a 13-byte payload is within every frame bound")
}

/// Whether this keeper would accept a shutdown right now. Exposed so the
/// daemon can decide its own exit without duplicating the rule.
impl Keeper {
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The contract this build reports, for `roost doctor` to compare against a
    /// worker's.
    pub fn contract(&self) -> &KeeperContractV1 {
        &self.contract
    }

    /// The history a channel has retained, for the socket layer's legacy
    /// `GetHistoryResp` framing, which is the head sequence plus the raw ring.
    pub fn legacy_history(&self, channel_id: u16) -> Option<(u64, HistoryRecords)> {
        let channel = self.channels.get(&channel_id)?;
        Some((channel.history.head_seq(), channel.history.records()))
    }
}
