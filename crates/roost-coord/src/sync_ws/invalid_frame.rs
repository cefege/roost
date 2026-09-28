//! Which legality check refused one client frame, and the evidence a close
//! line carries so five different causes stop sharing one reason.
//!
//! Owned by `sync_ws::ingress` and `sync_ws::commands`, which are the two
//! places a Sync socket decides a client frame is not a legal client frame. v2
//! refused all of them with one string, `closeForInvalidAck` -- and the string
//! names none of them, so a Rust-only close read as one cause when it had five.
//! A reason that cannot tell five causes apart is not a diagnosis.

/// One reason a client frame is not a legal client frame, and every close it
/// can produce is `1008` (`sync-ws-v1-delivery.ts:274`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidFrame {
    /// The bytes did not decode, or did not re-encode to themselves.
    NotCanonical,
    /// A cumulative acknowledgement above the last sequence this socket sent.
    AckAboveLastSent,
    /// The frame carried neither an acknowledgement nor a command.
    NeitherAckNorCommand,
    /// A command named a domain this build does not know.
    UnknownDomain,
    /// A terminal-domain reset could not be announced to the client.
    TerminalResetRefused,
}

impl InvalidFrame {
    /// The close's log reason. Distinct per cause, because the close itself is
    /// the same `1008` for all of them and the code says nothing.
    #[must_use]
    pub fn cause(self) -> &'static str {
        match self {
            Self::NotCanonical => "invalid_client_frame_not_canonical",
            Self::AckAboveLastSent => "invalid_client_frame_ack_above_last_sent",
            Self::NeitherAckNorCommand => "invalid_client_frame_no_ack_no_command",
            Self::UnknownDomain => "invalid_client_frame_unknown_domain",
            Self::TerminalResetRefused => "invalid_client_frame_terminal_reset_refused",
        }
    }
}

/// The frame's bytes as hex, for the close line. Bounded: a hostile client can
/// make this frame any length, and a log line is not a place to copy one.
#[must_use]
pub fn frame_hex(raw: &[u8]) -> String {
    const MAX_BYTES: usize = 96;
    let shown = raw.len().min(MAX_BYTES);
    let mut hex = String::with_capacity(shown * 2);
    for byte in &raw[..shown] {
        hex.push_str(&format!("{byte:02x}"));
    }
    if shown < raw.len() {
        hex.push_str("..");
    }
    hex
}
