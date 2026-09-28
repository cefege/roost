//! The user-felt terminal timing clocks: a bounded ledger of begun timings,
//! the trusted-keydown start of a `trusted_key` clock, and the result a finished
//! timing reports on top of its paint proof. Native; `smoke::paint_wait` owns
//! the keydown listener and the proof. Ports
//! `apps/web/src/smoke/smokeHarness.ts:71-83,441-522`.

use std::collections::VecDeque;

use serde_json::{Value, json};

use super::call::TimingKind;

/// Timings held before the oldest is evicted.
pub const TIMING_CAPACITY: usize = 64;

/// One begun timing.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTiming {
    pub id: String,
    pub kind: TimingKind,
    pub session_id: Option<String>,
    pub started_monotonic_ms: Option<f64>,
    pub started_epoch_ms: Option<f64>,
    pub trusted_key: bool,
}

/// The begun timings, oldest first.
#[derive(Debug, Default)]
pub struct TimingLedger {
    pending: VecDeque<PendingTiming>,
}

impl TimingLedger {
    /// Begin `kind` under `id` at `now_ms` (monotonic) with the page's time
    /// origin. A `trusted_key` clock needs a session and starts only at its
    /// first trusted keydown. Answers the id it evicted to make room, if any.
    pub fn begin(
        &mut self,
        id: &str,
        kind: TimingKind,
        session_id: Option<String>,
        now_ms: f64,
        time_origin_ms: f64,
    ) -> Result<Option<String>, String> {
        if kind == TimingKind::TrustedKey && session_id.is_none() {
            return Err("trusted_key timing requires a session id".to_owned());
        }
        let evicted = if self.pending.len() >= TIMING_CAPACITY {
            self.pending.pop_front().map(|oldest| oldest.id)
        } else {
            None
        };
        let starts_now = kind != TimingKind::TrustedKey;
        self.pending.push_back(PendingTiming {
            id: id.to_owned(),
            kind,
            session_id,
            started_monotonic_ms: starts_now.then_some(now_ms),
            started_epoch_ms: starts_now.then_some(time_origin_ms + now_ms),
            trusted_key: false,
        });
        tracing::debug!(target: "smoke", id, kind = kind.as_str(), "terminal timing begun");
        Ok(evicted)
    }

    /// A trusted keydown inside the timing's slot started its clock. Answers
    /// whether a waiting `trusted_key` timing took it.
    pub fn note_trusted_key(&mut self, id: &str, now_ms: f64, time_origin_ms: f64) -> bool {
        let Some(timing) = self.pending.iter_mut().find(|timing| timing.id == id) else {
            return false;
        };
        if timing.trusted_key {
            return false;
        }
        timing.started_monotonic_ms = Some(now_ms);
        timing.started_epoch_ms = Some(time_origin_ms + now_ms);
        timing.trusted_key = true;
        true
    }

    /// A copy of a pending timing.
    pub fn get(&self, id: &str) -> Option<PendingTiming> {
        self.pending.iter().find(|timing| timing.id == id).cloned()
    }

    /// Remove a timing, answering what it held.
    pub fn take(&mut self, id: &str) -> Option<PendingTiming> {
        let position = self.pending.iter().position(|timing| timing.id == id)?;
        self.pending.remove(position)
    }
}

/// The error an unknown or evicted timing id answers.
pub fn unknown_timing(id: &str) -> String {
    format!("unknown or expired terminal timing: {id}")
}

/// A finished timing: the marker proof plus the clock it closed.
pub fn timing_result(
    timing: &PendingTiming,
    session_id: &str,
    proof: Value,
    proof_monotonic_ms: f64,
) -> Result<Value, String> {
    if let Some(started_for) = &timing.session_id
        && started_for != session_id
    {
        return Err(format!("terminal timing session changed: {started_for} -> {session_id}"));
    }
    let (Some(started), Some(started_epoch)) = (timing.started_monotonic_ms, timing.started_epoch_ms)
    else {
        return Err(format!("terminal timing {} never observed a trusted keydown", timing.id));
    };
    let Value::Object(mut fields) = proof else {
        return Err("terminal timing proof was not an object".to_owned());
    };
    let extra = json!({
        "timingId": timing.id,
        "kind": timing.kind.as_str(),
        "startedMonotonicMs": started,
        "startedEpochMs": started_epoch,
        "durationMs": proof_monotonic_ms - started,
        "trustedKey": timing.trusted_key,
    });
    if let Value::Object(extra) = extra {
        fields.extend(extra);
    }
    Ok(Value::Object(fields))
}
