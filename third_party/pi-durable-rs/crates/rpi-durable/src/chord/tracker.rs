//! Document draft tracker (port of the consumed surface of
//! `chord/src/delta/tracker.ts` + `draft.ts` @ `abe508e1b`).
//!
//! Upstream records fine-grained proxy mutations and replays the recorded
//! operation list. The Rust port keeps the same transaction contract
//! (base/revision checks, prepare/adopt/abort) but derives the operation batch
//! with [`crate::chord::delta::diff_json`] at prepare time; the final value is
//! identical and the operations replay to the same value. Operation
//! granularity is a registered shape difference (V17-14 §7.3).
//!
//! The tracker is intentionally small: it carries one immutable value and one
//! revision. A [`Change`] owns a shared mutable cell; callers mutate it through
//! [`DocCell::write`] and settle it with [`Change::prepare`] or
//! [`Change::abort`]. Adopting a prepared change is an infallible pointer swap
//! so a failed storage admission can discard the candidate without touching the
//! committed value.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::chord::delta::{DeltaError, Op, diff_json};
use crate::chord::json::JsonValue;

/// Shared mutable staging cell behind one [`Change`] or [`crate::session::transaction::DocDraft`].
#[derive(Debug)]
pub struct DocCell {
    value: Mutex<JsonValue>,
    revoked: AtomicBool,
}

impl DocCell {
    /// Wrap a candidate value.
    pub fn new(value: JsonValue) -> Self {
        Self {
            value: Mutex::new(value),
            revoked: AtomicBool::new(false),
        }
    }

    /// Whether the owning transaction already settled (drafts are revoked).
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }

    /// Revoke read and write access; every later access returns [`DeltaError`].
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
    }

    /// Read the candidate value (a clone at the caller boundary).
    pub fn read(&self) -> Result<JsonValue, DeltaError> {
        if self.is_revoked() {
            return Err(DeltaError::Invalid(
                "Cannot use a settled document draft".to_string(),
            ));
        }
        let guard = self.lock()?;
        Ok(guard.clone())
    }

    /// Lock the candidate for in-place mutation.
    pub fn write(&self) -> Result<MutexGuard<'_, JsonValue>, DeltaError> {
        if self.is_revoked() {
            return Err(DeltaError::Invalid(
                "Cannot use a settled document draft".to_string(),
            ));
        }
        self.lock()
    }

    fn lock(&self) -> Result<MutexGuard<'_, JsonValue>, DeltaError> {
        self.value
            .lock()
            .map_err(|_| DeltaError::Invalid("document draft lock poisoned".to_string()))
    }
}

/// One prepared candidate revision of a document value.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// Value the change started from.
    pub base: JsonValue,
    /// Candidate final value.
    pub value: JsonValue,
    /// Replayable structural diff from `base` to `value`; a root replacement for
    /// [`Tracker::prepare_replace`].
    pub ops: Vec<Op>,
    base_revision: u64,
    cell: Arc<DocCell>,
}

impl Prepared {
    /// The tracker revision this candidate was prepared against.
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }

    /// Candidate value.
    pub fn value(&self) -> &JsonValue {
        &self.value
    }

    /// Candidate operations.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Discard the candidate and revoke its draft cell.
    pub fn abort(&self) {
        self.cell.revoke();
    }
}

/// One open change returned by [`Tracker::begin_change`].
#[derive(Debug)]
pub struct Change {
    base: JsonValue,
    base_revision: u64,
    cell: Arc<DocCell>,
    settled: bool,
}

impl Change {
    /// Shared staging cell; callers mutate it through [`DocCell::write`].
    pub fn state(&self) -> Arc<DocCell> {
        Arc::clone(&self.cell)
    }

    /// Snapshot the current candidate value.
    pub fn value(&self) -> Result<JsonValue, DeltaError> {
        self.cell.read()
    }

    /// Settle the change and produce its candidate revision.
    pub fn prepare(&mut self) -> Result<Prepared, DeltaError> {
        if self.settled {
            return Err(DeltaError::Invalid(
                "Change has already been settled".to_string(),
            ));
        }
        self.settled = true;
        let value = self.cell.read()?;
        let ops = diff_json(&self.base, &value);
        Ok(Prepared {
            base: self.base.clone(),
            value,
            ops,
            base_revision: self.base_revision,
            cell: Arc::clone(&self.cell),
        })
    }

    /// Discard the change; every later draft access is rejected.
    pub fn abort(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        self.cell.revoke();
    }
}

/// Committed document value plus its revision (upstream `Tracker`).
#[derive(Debug)]
pub struct Tracker {
    inner: Mutex<TrackerInner>,
}

#[derive(Debug)]
struct TrackerInner {
    value: JsonValue,
    revision: u64,
}

impl Tracker {
    /// Track one committed value at revision zero.
    pub fn new(value: JsonValue) -> Self {
        Self {
            inner: Mutex::new(TrackerInner { value, revision: 0 }),
        }
    }

    /// Current committed value.
    pub fn value(&self) -> JsonValue {
        self.inner
            .lock()
            .map(|inner| inner.value.clone())
            .unwrap_or(JsonValue::Null)
    }

    /// Current revision; every adopted change bumps it.
    pub fn revision(&self) -> u64 {
        self.inner.lock().map(|inner| inner.revision).unwrap_or(0)
    }

    /// Open a change over the current value.
    pub fn begin_change(&self) -> Change {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Change {
            base: inner.value.clone(),
            base_revision: inner.revision,
            cell: Arc::new(DocCell::new(inner.value.clone())),
            settled: false,
        }
    }

    /// Prepare a root replacement (upstream `prepareReplace`).
    pub fn prepare_replace(&self, value: JsonValue) -> Prepared {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Prepared {
            base: inner.value.clone(),
            value: value.clone(),
            ops: vec![Op::Replace(value)],
            base_revision: inner.revision,
            cell: Arc::new(DocCell::new(inner.value.clone())),
        }
    }

    /// Adopt one prepared change. Fails when the change is stale or belongs to
    /// a revision the tracker has already moved past.
    pub fn adopt(&self, prepared: Prepared) -> Result<(), DeltaError> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if prepared.base_revision != inner.revision || prepared.base != inner.value {
            prepared.cell.revoke();
            return Err(DeltaError::Invalid("Prepared change is stale".to_string()));
        }
        inner.value = prepared.value.clone();
        inner.revision += 1;
        prepared.cell.revoke();
        Ok(())
    }
}

/// Take ownership of an alias-free strict-JSON root (upstream `track`).
pub fn track(initial: JsonValue) -> Tracker {
    Tracker::new(initial)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::delta::apply_immutable;
    use serde_json::json;

    #[test]
    fn change_prepare_adopt_round_trip() {
        let tracker = track(json!({"items": [], "name": "a"}));
        let mut change = tracker.begin_change();
        {
            let cell = change.state();
            let mut guard = cell.write().unwrap();
            guard["items"].as_array_mut().unwrap().push(json!(1));
            guard["name"] = json!("b");
        }
        let _ = &change;
        let prepared = change.prepare().unwrap();
        assert_eq!(prepared.base_revision(), 0);
        assert_eq!(
            apply_immutable(Some(&prepared.base), &prepared.ops).unwrap(),
            prepared.value
        );
        tracker.adopt(prepared).unwrap();
        assert_eq!(tracker.revision(), 1);
        assert_eq!(tracker.value(), json!({"items": [1], "name": "b"}));
    }

    #[test]
    fn prepared_is_single_use_and_stale_after_adoption() {
        let tracker = track(json!({"n": 0}));
        let mut first = tracker.begin_change();
        let first_cell = first.state();
        {
            let mut guard = first_cell.write().unwrap();
            guard["n"] = json!(1);
        }
        let mut second = tracker.begin_change();
        let second_cell = second.state();
        {
            let mut guard = second_cell.write().unwrap();
            guard["n"] = json!(2);
        }
        let first = first.prepare().unwrap();
        let second = second.prepare().unwrap();
        tracker.adopt(first).unwrap();
        assert!(tracker.adopt(second).is_err());
        assert_eq!(tracker.value(), json!({"n": 1}));
    }

    #[test]
    fn aborted_draft_is_revoked() {
        let tracker = track(json!({"n": 0}));
        let mut change = tracker.begin_change();
        change.abort();
        assert!(change.state().read().is_err());
        assert_eq!(tracker.value(), json!({"n": 0}));
    }

    #[test]
    fn empty_change_prepares_no_operations() {
        let tracker = track(json!({"n": 0}));
        let mut change = tracker.begin_change();
        let prepared = change.prepare().unwrap();
        assert!(prepared.ops().is_empty());
        tracker.adopt(prepared).unwrap();
    }

    #[test]
    fn prepare_replace_emits_a_root_replacement() {
        let tracker = track(json!({"n": 0}));
        let prepared = tracker.prepare_replace(json!({"m": 1}));
        assert_eq!(prepared.ops(), &[Op::Replace(json!({"m": 1}))]);
        tracker.adopt(prepared).unwrap();
        assert_eq!(tracker.value(), json!({"m": 1}));
    }
}
