//! The cumulative delivery-sequence ACK window: what may be in flight, how long
//! it may sit there, and every close the window can take.
//!
//! Owned by the coordinator's Sync link and consulted before every application
//! frame is handed to the socket (`apps/coord/src/sync/sync-ws-v1-delivery.ts`).
//! Pure arithmetic over an injected `now_ms` and an injected "did the socket
//! accept it" answer, so the whole contract is testable with no socket and no
//! sleeping.
//!
//! WHY A WINDOW AT ALL, AND WHY THESE NUMBERS. The socket's own send buffer
//! bounds bytes in the kernel; this bounds **frames the application has sent but
//! the client has not confirmed processing**. Without it a browser that stops
//! dispatching -- a backgrounded tab, a laptop resuming, a client that threw in
//! its own dispatch handler -- accumulates an unbounded coordinator-side queue
//! while the coordinator keeps writing it. 512 frames / 4 MiB / 3 s is a
//! `flow=1` client's own acknowledgement cadence, so exceeding any of them means
//! the client is not merely slow, it is not answering.
//!
//! WHY EVERY BOUND FAILS CLOSED WITH 1013. `1013` is "try again later", and a
//! slow client is exactly that: the socket is closed, the client reconnects,
//! negotiates, and recovers from the durable log. Refusing to send is the one
//! option that cannot be wrong here -- buffering more is how a coordinator's
//! memory becomes a function of how long a browser tab was hidden.
//!
//! WHY AN ACK ABOVE THE LAST SENT SEQUENCE CLOSES WITH 1008 AND NOT 1013. That
//! is a protocol violation, not backpressure: a client cannot have processed a
//! sequence the coordinator never sent, and continuing would mean the window's
//! accounting no longer describes reality. `1008` is the policy-violation code
//! and it is the only place a Sync socket uses it
//! (`sync-ws-v1-delivery.ts:270-275`).

/// Application frames that may be sent but not yet acknowledged.
pub const MAX_UNACKED_FRAMES: usize = 512;

/// Encoded bytes that may be sent but not yet acknowledged.
pub const MAX_UNACKED_BYTES: u64 = 4 * 1024 * 1024;

/// How long the oldest unacknowledged frame may wait.
pub const ACK_TIMEOUT_MS: u64 = 3_000;

/// The close code every backpressure path uses.
pub const BACKPRESSURE_CLOSE_CODE: u16 = 1013;
/// Its reason string, shared by every backpressure path.
pub const BACKPRESSURE_REASON: &str = "sync backpressure";

/// The close code an invalid acknowledgement uses, and its reason.
pub const INVALID_ACK_CLOSE_CODE: u16 = 1008;
/// Its reason string.
pub const INVALID_ACK_REASON: &str = "invalid sync ack";

/// Why a socket was closed for backpressure. The five are the complete
/// vocabulary (`apps/coord/src/sync/sync-ws-v1-delivery.ts:27-32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BackpressureReason {
    /// The socket's own buffered bytes passed its high-water mark.
    #[error("high_water")]
    HighWater,
    /// The socket reported backpressure and did not recover inside its
    /// timeout.
    #[error("timeout")]
    Timeout,
    /// The unacknowledged frame count passed [`MAX_UNACKED_FRAMES`].
    #[error("frame_limit")]
    FrameLimit,
    /// The unacknowledged byte count passed [`MAX_UNACKED_BYTES`].
    #[error("byte_limit")]
    ByteLimit,
    /// The oldest unacknowledged frame waited [`ACK_TIMEOUT_MS`].
    #[error("age_limit")]
    AgeLimit,
}

/// The window's state at the moment it closed, for a `signal` and a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowStats {
    /// Unacknowledged frames.
    pub unacked_frames: usize,
    /// Unacknowledged encoded bytes.
    pub unacked_bytes: u64,
    /// How long the oldest unacknowledged frame has waited, in milliseconds.
    pub oldest_age_ms: u64,
}

/// One in-flight record. Metadata only: payloads and encoded buffers are never
/// stored here, because a queue of frames that holds their payloads is a second
/// copy of everything the socket is holding
/// (`apps/coord/src/sync/sync-ws-upgrade.ts:109`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryRecord {
    /// The `delivery_seq` this frame was sent under.
    pub seq: u64,
    /// Its encoded size on the wire.
    pub encoded_bytes: u64,
    /// When the socket accepted it, in epoch milliseconds.
    pub sent_at_ms: u64,
}

/// A close the window requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowClose {
    /// Backpressure: close `1013` with [`BACKPRESSURE_REASON`].
    Backpressure(BackpressureReason, WindowStats),
    /// An acknowledgement above the last sent sequence: close `1008` with
    /// [`INVALID_ACK_REASON`].
    InvalidAck,
}

/// One Sync socket's cumulative-ACK window.
#[derive(Debug, Default)]
pub struct AckWindow {
    records: Vec<DeliveryRecord>,
    unacked_bytes: u64,
    last_sent_seq: u64,
    highest_ack_seq: u64,
    enabled: bool,
}

impl AckWindow {
    /// A window for a socket, with cumulative flow control on or off.
    ///
    /// Off only for a socket that did not ask for `flow=1`. An off window
    /// sequences nothing, tracks nothing, and can never close for backpressure:
    /// the client has declined the contract, and a coordinator that enforced it
    /// anyway would be inventing a limit the client never agreed to.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    /// Whether flow control is on.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The sequence the next application frame will carry.
    ///
    /// Zero-based on a fresh socket, so the first frame is `1`. Controls use
    /// `delivery_seq = 0` and never consume the window
    /// (`protocol/spec/sync.md:29`), which is why a control is not routed
    /// through here at all.
    #[must_use]
    pub fn next_sequence(&self) -> u64 {
        self.last_sent_seq.saturating_add(1)
    }

    /// The current counters, for a log line or a `signal`.
    #[must_use]
    pub fn stats(&self, now_ms: u64) -> WindowStats {
        WindowStats {
            unacked_frames: self.records.len(),
            unacked_bytes: self.unacked_bytes,
            oldest_age_ms: self.oldest_age(now_ms),
        }
    }

    /// Whether a frame of `encoded_bytes` may be sent now.
    ///
    /// Checked **before** the send, and both limits are inclusive: the 513th
    /// unacknowledged frame closes, and so does the frame that would take the
    /// unacknowledged bytes past 4 MiB. Checking before sending rather than
    /// after is what keeps the window from being one frame over the moment it
    /// closes.
    pub fn may_send(&self, encoded_bytes: u64, now_ms: u64) -> Result<(), WindowClose> {
        if !self.enabled {
            return Ok(());
        }
        if self.records.len() + 1 > MAX_UNACKED_FRAMES {
            return Err(self.close(BackpressureReason::FrameLimit, now_ms));
        }
        if self.unacked_bytes.saturating_add(encoded_bytes) > MAX_UNACKED_BYTES {
            return Err(self.close(BackpressureReason::ByteLimit, now_ms));
        }
        if self.oldest_age(now_ms) >= ACK_TIMEOUT_MS {
            return Err(self.close(BackpressureReason::AgeLimit, now_ms));
        }
        Ok(())
    }

    /// Record a frame the socket accepted.
    ///
    /// Only ever called for an accepted send. A socket that dropped the frame
    /// (its send returned zero) never had it, so counting it would make the
    /// window describe a delivery that did not happen
    /// (`sync-ws-v1-delivery.ts:205-215`).
    pub fn record_sent(&mut self, encoded_bytes: u64, now_ms: u64) -> u64 {
        if !self.enabled {
            // Zero is the wire value a CONTROL frame carries, and an unnegotiated
            // socket carries no application sequence at all. Returning the
            // sequence it *would* have used would put a positive number on a
            // socket that never negotiated one.
            return 0;
        }
        let sequence = self.next_sequence();
        if self.enabled {
            self.last_sent_seq = sequence;
            self.unacked_bytes = self.unacked_bytes.saturating_add(encoded_bytes);
            self.records.push(DeliveryRecord {
                seq: sequence,
                encoded_bytes,
                sent_at_ms: now_ms,
            });
        }
        sequence
    }

    /// Apply a cumulative acknowledgement.
    ///
    /// Releases every record at or below `ack_seq`. Three behaviours are
    /// deliberate:
    ///
    /// * an ack **above** the last sent sequence closes `1008`: a client cannot
    ///   have processed something never sent;
    /// * a **stale or repeated** ack is harmless and returns `Ok` -- a
    ///   reordered or duplicated ack is normal on a socket that also carries
    ///   controls, and closing on it would kill healthy clients;
    /// * the age deadline is re-armed from the new oldest record, so the window
    ///   measures the *current* oldest, not the first one ever sent.
    pub fn apply_ack(&mut self, ack_seq: u64, now_ms: u64) -> Result<u64, WindowClose> {
        if !self.enabled {
            return Ok(0);
        }
        if ack_seq > self.last_sent_seq {
            return Err(WindowClose::InvalidAck);
        }
        if ack_seq <= self.highest_ack_seq {
            return Ok(0);
        }
        let released: Vec<DeliveryRecord> = self
            .records
            .iter()
            .copied()
            .take_while(|record| record.seq <= ack_seq)
            .collect();
        let count = released.len();
        for record in released {
            self.unacked_bytes = self.unacked_bytes.saturating_sub(record.encoded_bytes);
        }
        self.records.drain(..count);
        self.highest_ack_seq = ack_seq;
        // The window's age deadline is read by the caller against the NEW oldest
        // record, so the acknowledgement itself does not consult the clock; the
        // parameter stays in the signature so the caller cannot pass a clock at
        // admission and a different one here.
        let _ = now_ms;
        Ok(count as u64)
    }

    /// Whether the oldest unacknowledged frame has waited long enough to close.
    #[must_use]
    pub fn age_deadline_passed(&self, now_ms: u64) -> bool {
        self.enabled && self.oldest_age(now_ms) >= ACK_TIMEOUT_MS
    }

    /// Release everything, for a socket teardown.
    pub fn clear(&mut self) {
        self.records.clear();
        self.unacked_bytes = 0;
        self.last_sent_seq = 0;
        self.highest_ack_seq = 0;
    }

    fn oldest_age(&self, now_ms: u64) -> u64 {
        self.records
            .first()
            .map_or(0, |oldest| now_ms.saturating_sub(oldest.sent_at_ms))
    }

    fn close(&self, reason: BackpressureReason, now_ms: u64) -> WindowClose {
        WindowClose::Backpressure(reason, self.stats(now_ms))
    }
}

/// The native-buffer high-water decision, separate from the application window.
///
/// The socket's own buffered bytes are a different resource from the
/// unacknowledged application window, and they are checked *after* a successful
/// send, because only then is there a buffered amount to read
/// (`sync-ws-v1-delivery.ts:216-219`). A port that folds the two together loses
/// the distinction between "the client is behind" and "the kernel buffer is
/// full", which are different problems with different fixes.
#[must_use]
pub fn buffered_over_high_water(buffered_bytes: u64, limit_bytes: u64) -> bool {
    buffered_bytes > limit_bytes
}

/// Whether a backpressure-reported send should start the recovery timer.
///
/// The socket reports backpressure with a negative return rather than dropping
/// the frame, so the frame is still counted and the timer is armed once
/// (`sync-ws-v1-delivery.ts:220-226`). Arming per send would reset the timer on
/// every frame and the socket would never time out.
#[must_use]
pub fn should_arm_recovery_timer(socket_reported_backpressure: bool, timer_armed: bool) -> bool {
    socket_reported_backpressure && !timer_armed
}
