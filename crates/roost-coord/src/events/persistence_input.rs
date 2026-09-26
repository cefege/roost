//! The bounds a worker-controlled string gets before it is stored, and the one
//! normalization every durable consumer shares.
//!
//! Ported from `apps/coord/src/events/persistence-input.ts`. Called by the
//! append transaction before the event is serialized, so the durable JSON, the
//! projection fold, the channel-index publication and the live Sync publication
//! all see the same bytes (`event-transaction.ts:85-95`): "Use this one
//! normalized value for durable JSON, projection folding, channel-index
//! publication, and live Sync publication. Normalizing a row later would make
//! replay disagree with the sessions projection."
//!
//! THE SIX FIELDS. `cwd`, `spawn_cwd`, `custom_title`, `git_branch`, `git_remote`
//! and `pr_url` are display strings, paths and URLs a worker supplies. None of
//! them is a protocol fact, all of them are attacker-influenced in the sense
//! that matters here -- a worker is a peer, not a trusted source of text -- and
//! all of them are stored in the event log forever. Each is bounded to
//! [`MAX_PERSISTED_UTF8_BYTES`] **per field**, so a session cannot smuggle an
//! unbounded payload into the log by splitting it across fields.
//!
//! WHY THE TRUNCATION IS BYTE COUNT AND NOT CHARACTER COUNT. The stored value is
//! UTF-8 and the column is bytes, so the bound is bytes, and it lands on a whole
//! code point: a stored event must never contain half a character.
//!
//! WHAT THE JAVASCRIPT VERSION HANDLES THAT A `&str` CANNOT. `truncatePersistedUtf8`
//! walks UTF-16 code units and has a branch for an unpaired surrogate, which
//! `TextEncoder` encodes as U+FFFD (`persistence-input.ts:39-50`). A Rust `&str`
//! is already whole code points, so that branch is unreachable here and the
//! equivalent walk is over `char_indices`. The bound and the result for every
//! representable input are identical.

use roost_protocol::wire::{Session, SessionEvent};

/// The bound on one persisted string, in UTF-8 bytes.
pub const MAX_PERSISTED_UTF8_BYTES: usize = 4_096;

/// The most sessions one worker snapshot may announce.
///
/// A snapshot is a whole machine's live set on reconnect. Past this the append
/// is refused with an error rather than a truncated list, because a truncated
/// snapshot would read as "these are all my sessions" and quietly orphan the
/// rest (`event-transaction.ts:87-92`).
pub const MAX_WORKER_SNAPSHOT_SESSIONS: usize = 1_024;

/// The longest whole-code-point prefix of `value` that fits in `max_bytes`.
///
/// A zero bound yields the empty string, which is v2's answer for
/// `maxBytes === 0` (`persistence-input.ts:25`). A negative bound was a
/// `RangeError` in v2 and is unrepresentable here, so there is no failure to
/// return.
#[must_use]
pub fn truncate_persisted_utf8(value: &str, max_bytes: usize) -> &str {
    let mut bytes = 0;
    for (index, character) in value.char_indices() {
        let width = character.len_utf8();
        if bytes + width > max_bytes {
            return &value[..index];
        }
        bytes += width;
    }
    value
}

/// Bound every worker-controlled string in an event, in place.
///
/// The shape of this function is v2's: one arm per kind that carries a bounded
/// field, and no arm for the kinds that carry none. A field that is already
/// within its bound is left exactly as it is -- no reallocation, and no way for
/// the stored value to differ from the value the worker sent by accident.
#[must_use]
pub fn normalize_persisted_worker_event(event: SessionEvent) -> SessionEvent {
    match event {
        SessionEvent::Opened {
            session_id,
            worker_fp,
            channel,
            session_kind,
            mut cwd,
            ts,
            trace_id,
        } => {
            bound_field(&mut cwd);
            SessionEvent::Opened {
                session_id,
                worker_fp,
                channel,
                session_kind,
                cwd,
                ts,
                trace_id,
            }
        }
        SessionEvent::Cwd {
            session_id,
            mut cwd,
            ts,
            trace_id,
        } => {
            bound_field(&mut cwd);
            SessionEvent::Cwd {
                session_id,
                cwd,
                ts,
                trace_id,
            }
        }
        SessionEvent::Renamed {
            session_id,
            mut custom_title,
            ts,
            trace_id,
        } => {
            bound_field(&mut custom_title);
            SessionEvent::Renamed {
                session_id,
                custom_title,
                ts,
                trace_id,
            }
        }
        SessionEvent::Git {
            session_id,
            mut branch,
            mut remote,
            ts,
            trace_id,
        } => {
            bound_optional(&mut branch);
            bound_optional(&mut remote);
            SessionEvent::Git {
                session_id,
                branch,
                remote,
                ts,
                trace_id,
            }
        }
        SessionEvent::Pr {
            session_id,
            number,
            state,
            checks,
            mut url,
            ts,
            trace_id,
        } => {
            bound_optional(&mut url);
            SessionEvent::Pr {
                session_id,
                number,
                state,
                checks,
                url,
                ts,
                trace_id,
            }
        }
        SessionEvent::Snapshot {
            worker_fp,
            mut sessions,
            ts,
            trace_id,
        } => {
            for session in &mut sessions {
                bound_session(session);
            }
            SessionEvent::Snapshot {
                worker_fp,
                sessions,
                ts,
                trace_id,
            }
        }
        other => other,
    }
}

/// Bound the six bounded fields of one announced session row.
fn bound_session(session: &mut Session) {
    bound_field(&mut session.cwd);
    bound_optional(&mut session.spawn_cwd);
    bound_optional(&mut session.custom_title);
    bound_optional(&mut session.git_branch);
    bound_inner_optional(&mut session.git_remote);
    bound_optional(&mut session.pr_url);
}

fn bound_field(value: &mut String) {
    let truncated = truncate_persisted_utf8(value, MAX_PERSISTED_UTF8_BYTES);
    if truncated.len() < value.len() {
        *value = truncated.to_owned();
    }
}

fn bound_optional(value: &mut Option<String>) {
    if let Some(inner) = value.as_mut() {
        bound_field(inner);
    }
}

/// The inner half of a two-state optional. `git_remote` is absent, present and
/// null, or present and set, and only the last of those carries text.
fn bound_inner_optional(value: &mut Option<Option<String>>) {
    if let Some(Some(inner)) = value.as_mut() {
        bound_field(inner);
    }
}
