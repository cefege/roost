//! The lease state machine behind a terminal recording: which recordings are
//! armed, the always-on windows, and the answers a lease step can produce.
//! `capture::recorder` owns the recorder and the bundle it freezes; this file
//! owns the STATE and the rules that move it.
//!
//! It is a separate file because the rules here are the ones with a v2 authority
//! to argue about. A reader checking "what does a second START do to somebody
//! else's evidence" should find the answer here, without wading through a
//! diagnostic report builder and a JSON assembler to reach it.
//!
//! THREE RULES, AND EACH ONE IS ABOUT NOT TAKING SOMEBODY ELSE'S EVIDENCE.
//!
//! A REPEAT START FROM THE SAME RECORDING RENEWS THE LEASE AND KEEPS EVERY
//! RETAINED RECORD. The browser re-sends START on a timer while the debugging
//! pane is visible, so a repeat is the normal case and must not start over.
//!
//! A DIFFERENT RECORDING ON A LIVE LEASE IS A CONFLICT, not a replacement.
//! Evicting another operator's evidence to make room for a second request is
//! never the right answer, and `lease_conflict` says so instead of doing it.
//!
//! LEASE EXPIRY IS DECIDED ON SERVER TIME AND DISARMS ON OBSERVATION. An
//! expired lease is never silently renewed: a browser that went away for half
//! an hour must not find its recording still armed when it comes back.

use std::collections::HashMap;
use std::sync::{MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use roost_protocol::terminal_capture::{
    TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode, TerminalCaptureFileRef,
    TerminalCaptureStatus, TerminalCaptureWorkerAck,
};

use crate::browser_commands::diagnostics::CaptureCommand;

use super::byte_window::ByteWindow;

/// One armed recording, and the evidence it holds.
#[derive(Debug)]
pub(super) struct Armed {
    pub(super) recording_id: String,
    pub(super) expires_at_ms: u64,
    /// The peer's own evidence, kept apart from the coordinator's because the
    /// two have different provenance and different trust: the coordinator's is
    /// authoritative and destination-free, and a bundle that merged them into
    /// one string could not say which half came from where.
    pub(super) browser_evidence: String,
    pub(super) coordinator_evidence: String,
}

/// Everything the recorder owns, behind one lock.
#[derive(Debug, Default)]
pub(super) struct Registry {
    pub(super) armed: HashMap<String, Armed>,
    /// The always-on window per session, whether or not anything is armed.
    pub(super) windows: HashMap<String, ByteWindow>,
    /// The last bundle this worker froze, for the answer's own field.
    pub(super) recent: Option<TerminalCaptureFileRef>,
}
impl Registry {
    /// the second has already been logged and disarmed.
    pub(super) fn live_lease(&mut self, session_id: &str) -> Option<Armed> {
        let armed = self.armed.get(session_id)?;
        if now_ms() < armed.expires_at_ms {
            return Some(Armed {
                recording_id: armed.recording_id.clone(),
                expires_at_ms: armed.expires_at_ms,
                browser_evidence: armed.browser_evidence.clone(),
                coordinator_evidence: armed.coordinator_evidence.clone(),
            });
        }
        let expired = self.armed.remove(session_id);
        if let Some(expired) = expired {
            tracing::warn!(
                session_id,
                recording_id = %expired.recording_id,
                expires_at_ms = expired.expires_at_ms,
                "a terminal recording lease expired and was disarmed"
            );
        }
        None
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

pub(super) fn failed(error: TerminalCaptureErrorCode) -> TerminalCaptureWorkerAck {
    TerminalCaptureWorkerAck::failed(error)
}

/// An armed or released answer, naming the last bundle this worker froze.
pub(super) fn recording_ack(
    expires_at_ms: Option<u64>,
    capture_id: &str,
    registry: &Registry,
) -> TerminalCaptureWorkerAck {
    TerminalCaptureWorkerAck {
        status: TerminalCaptureStatus::Recording,
        path: None,
        byte_length: None,
        error: None,
        expires_at_ms,
        recent_worker_capture: registry
            .recent
            .clone()
            .filter(|last| last.capture_id != capture_id),
    }
}

pub(super) fn stopped_ack(capture_id: &str, registry: &Registry) -> TerminalCaptureWorkerAck {
    TerminalCaptureWorkerAck {
        status: TerminalCaptureStatus::Stopped,
        path: None,
        byte_length: None,
        error: None,
        expires_at_ms: None,
        recent_worker_capture: registry
            .recent
            .clone()
            .filter(|last| last.capture_id != capture_id),
    }
}

/// The two evidence strings, admitted under the protocol's own caps.
///
/// Both are checked HERE, while the caller is still waiting, rather than at the
/// write: a capture that cannot hold its evidence is refused before a bundle
/// exists, instead of producing a file nobody downloads.
pub(super) fn admit_evidence(
    command: &CaptureCommand,
) -> Result<(String, String), TerminalCaptureErrorCode> {
    let browser = admit_one(
        &command.browser_evidence_json,
        TERMINAL_CAPTURE_LIMITS.browser_evidence_bytes,
    )?;
    let coordinator = admit_one(
        &command.coordinator_evidence_json,
        TERMINAL_CAPTURE_LIMITS.coordinator_evidence_bytes,
    )?;
    Ok((browser, coordinator))
}

fn admit_one(value: &str, cap: usize) -> Result<String, TerminalCaptureErrorCode> {
    if value.len() > cap {
        return Err(TerminalCaptureErrorCode::EvidenceTooLarge);
    }
    if value.is_empty() {
        return Ok(String::new());
    }
    serde_json::from_str::<serde_json::Value>(value)
        .map_err(|_| TerminalCaptureErrorCode::EvidenceMalformed)
        .map(|_| value.to_owned())
}

/// A held registry, or the poison a panicking writer left behind.
pub(super) type Held<'a> = Result<MutexGuard<'a, Registry>, PoisonError<MutexGuard<'a, Registry>>>;
