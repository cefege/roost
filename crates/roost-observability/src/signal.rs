//! The Tier-1 signal channel: always on, independent of the diag firehose,
//! low-volume, and the daily-review channel `roost doctor` reads. Context
//! detail stays on [`crate::diag::emit`]. Repeats of one anomaly coalesce
//! behind a per-(kind, scope) cooldown so a flapping source cannot flood the
//! channel, while distinct sessions stay independent.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::fields::{LogFields, RecordSink};
use crate::log;
use crate::runtime::{self, EVENT_FIELD, MONO_NS_FIELD, Process};
use crate::signal_kind::SignalKind;

/// The log target signal records carry.
pub const SIGNAL_TARGET: &str = "signal";

/// How long one (kind, scope) stays quiet after it fires.
pub const SIGNAL_COOLDOWN_MS: i64 = 10_000;

/// The scope count past which cold entries are swept. Distinct per-session
/// anomalies would otherwise accumulate forever on a long-lived coordinator;
/// once the map is clearly larger than any realistic flapping set, entries
/// past the window go.
pub const SIGNAL_COOLDOWN_MAX_KEYS: usize = 512;

/// The field that scopes a signal's cooldown, deleted before the record ships.
pub const COOLDOWN_KEY_FIELD: &str = "cooldownKey";

/// The field that scopes a signal's cooldown when the caller named no
/// `cooldownKey`.
pub const SID_FIELD: &str = "sid";

const SINK_THREW: &str = "sink_threw";
const SINK_PANICKED: &str = "signal sink panicked";

/// When each (kind, scope) last fired. A `BTreeMap` so a sweep and a test see
/// the same order every time.
#[derive(Debug, Default)]
pub(crate) struct CooldownMap {
    last_fire_ms: BTreeMap<String, i64>,
}

impl CooldownMap {
    /// Whether the signal is admitted, stamping it if it is. The check, the
    /// cold sweep and the stamp happen together so two threads cannot both
    /// fire one key inside one window.
    pub(crate) fn admit(&mut self, key: &str, now_ms: i64) -> bool {
        if let Some(last_fire_ms) = self.last_fire_ms.get(key)
            && now_ms.saturating_sub(*last_fire_ms) < SIGNAL_COOLDOWN_MS
        {
            return false;
        }
        if self.last_fire_ms.len() > SIGNAL_COOLDOWN_MAX_KEYS {
            let cutoff = now_ms.saturating_sub(SIGNAL_COOLDOWN_MS);
            self.last_fire_ms.retain(|_, fired_at| *fired_at > cutoff);
        }
        self.last_fire_ms.insert(key.to_owned(), now_ms);
        true
    }

    /// Only the cap-guard tests read this; production has no reason to.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.last_fire_ms.len()
    }
}

/// Emit ONE always-on Tier-1 signal, or nothing if the same kind and scope
/// fired inside the window. Anomaly reporting is never load-bearing, so a
/// record the sink or the logger cannot carry costs this ONE event and is
/// reported as `sink_threw`; it is never raised into the failing path that
/// noticed the anomaly.
pub fn emit(kind: SignalKind, kv: LogFields) {
    let process = runtime::process();
    let now_ms = process.map_or(0, |process| process.clock.now_epoch_ms());
    let host = runtime::host();
    let sink = host.signal_sink();
    if !host.cooldowns().admit(&cooldown_key(kind, &kv), now_ms) {
        return;
    }
    emit_record(process, kind, kv, sink.as_deref());
}

/// The key is the kind joined with the caller's `cooldownKey`, else its `sid`,
/// else nothing — so a signal with no scope coalesces with every other
/// scope-less signal of that kind, which is the intent for a fleet-wide fault.
pub(crate) fn cooldown_key(kind: SignalKind, kv: &LogFields) -> String {
    let mut key = String::with_capacity(kind.as_str().len() + 1);
    key.push_str(kind.as_str());
    key.push('|');
    key.push_str(&cooldown_scope(kv));
    key
}

fn cooldown_scope(kv: &LogFields) -> String {
    for field in [COOLDOWN_KEY_FIELD, SID_FIELD] {
        match kv.get(field) {
            Some(Value::String(text)) => return text.clone(),
            // A null scope falls through to the next field, as `??` did.
            Some(Value::Null) | None => continue,
            Some(other) => return other.to_string(),
        }
    }
    String::new()
}

fn emit_record(
    process: Option<&Process>,
    kind: SignalKind,
    kv: LogFields,
    sink: Option<&dyn RecordSink>,
) {
    let mut record = LogFields::new().set(EVENT_FIELD, kind.as_str());
    if let Some(process) = process {
        record.put(MONO_NS_FIELD, process.clock.mono_ns());
    }
    record.absorb(kv);
    record.remove(COOLDOWN_KEY_FIELD);
    match sink {
        Some(sink) => {
            if !runtime::call_sink(sink, &record) {
                log::warn(
                    SIGNAL_TARGET,
                    SINK_THREW,
                    LogFields::new()
                        .set(EVENT_FIELD, kind.as_str())
                        .set("error", SINK_PANICKED),
                );
            }
        }
        None => log::warn(SIGNAL_TARGET, kind.as_str(), record),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::{
        CooldownMap, SIGNAL_COOLDOWN_MAX_KEYS, SIGNAL_COOLDOWN_MS, cooldown_key, emit_record,
    };
    use crate::clock::FixedClock;
    use crate::fields::{LogFields, RecordSink};
    use crate::runtime::Process;
    use crate::signal_kind::SignalKind;

    #[derive(Default)]
    struct Capturing {
        records: Mutex<Vec<LogFields>>,
    }

    impl Capturing {
        fn take(&self) -> Vec<LogFields> {
            let mut records = self
                .records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *records)
        }
    }

    impl RecordSink for Capturing {
        fn emit(&self, record: &LogFields) {
            let mut records = self
                .records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            records.push(record.clone());
        }
    }

    fn process() -> Process {
        Process::new(false, Arc::new(FixedClock::new(1_700_000_000_000, 4_242)))
    }

    #[test]
    fn a_second_fire_inside_the_window_is_dropped_and_another_key_is_not() {
        let mut cooldowns = CooldownMap::default();
        let now = 1_700_000_000_000;
        assert!(cooldowns.admit("keeper.died|sid-a", now));
        assert!(
            !cooldowns.admit("keeper.died|sid-a", now + SIGNAL_COOLDOWN_MS - 1),
            "the same kind and scope inside the window is one line"
        );
        assert!(
            cooldowns.admit("keeper.died|sid-b", now + 1),
            "a distinct scope stays independent"
        );
        assert!(
            cooldowns.admit("keeper.degraded|sid-a", now + 1),
            "a distinct kind stays independent"
        );
        assert!(
            cooldowns.admit("keeper.died|sid-a", now + SIGNAL_COOLDOWN_MS),
            "the window is exactly the cooldown, not cooldown plus one"
        );
    }

    #[test]
    fn past_the_cap_cold_keys_are_swept_and_hot_ones_are_kept() {
        let mut cooldowns = CooldownMap::default();
        let start = 1_700_000_000_000;
        for index in 0..=SIGNAL_COOLDOWN_MAX_KEYS {
            cooldowns.admit(&format!("keeper.died|sid-{index}"), start);
        }
        assert_eq!(cooldowns.len(), SIGNAL_COOLDOWN_MAX_KEYS + 1);
        // Inside the window: the map is over the cap but nothing is cold yet,
        // so the sweep drops nothing and the newcomer joins them.
        cooldowns.admit("keeper.died|sid-fresh", start + 1);
        assert_eq!(cooldowns.len(), SIGNAL_COOLDOWN_MAX_KEYS + 2);
        // Past the window: the next admission sweeps every entry that went
        // cold, so the map cannot grow without bound. The key admitted one
        // millisecond after the start is still HOT at this instant — it is
        // one millisecond short of its own window — so it survives, and only
        // it plus the newcomer remain.
        cooldowns.admit("keeper.died|sid-later", start + SIGNAL_COOLDOWN_MS);
        assert_eq!(cooldowns.len(), 2);
        // The sweep is a cap guard, not a per-admission collection: once the
        // map is back under the cap, later admissions leave the stale keys
        // alone and simply append. The bound this protects is memory, and
        // memory is bounded.
        cooldowns.admit("keeper.died|sid-last", start + 2 * SIGNAL_COOLDOWN_MS);
        assert_eq!(cooldowns.len(), 3);
    }

    #[test]
    fn the_key_is_the_kind_then_the_scope_cooldown_key_first() {
        let explicit = LogFields::new()
            .set("cooldownKey", "sidA|resize")
            .set("sid", "sidA");
        assert_eq!(
            cooldown_key(SignalKind::DiagCorruptionSignal, &explicit),
            "diag.corruption_signal|sidA|resize"
        );
        let from_sid = LogFields::new().set("sid", "sidB");
        assert_eq!(
            cooldown_key(SignalKind::DiagCorruptionSignal, &from_sid),
            "diag.corruption_signal|sidB"
        );
        let null_scope = LogFields::new().set("cooldownKey", json!(null));
        assert_eq!(
            cooldown_key(SignalKind::AuthKeyEvicted, &null_scope),
            "auth.key_evicted|"
        );
    }

    #[test]
    fn a_record_is_evt_mono_ns_then_the_callers_kv_without_the_cooldown_key() {
        let sink = Capturing::default();
        let process = process();
        emit_record(
            Some(&process),
            SignalKind::SpaUncaught,
            LogFields::new()
                .set("kind", "error")
                .set("msg", "boom")
                .set("cooldownKey", "sig-a"),
            Some(&sink),
        );
        let records = sink.take();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].to_value(),
            json!({ "evt": "spa.uncaught", "mono_ns": 4_242, "kind": "error", "msg": "boom" })
        );
        assert_eq!(records[0].get("cooldownKey"), None);
    }

    #[test]
    fn a_sink_that_panics_costs_one_signal_not_the_callers_path() {
        struct Panicking;
        impl RecordSink for Panicking {
            fn emit(&self, _record: &LogFields) {
                panic!("a signal sink that throws into the path it observes");
            }
        }
        emit_record(
            Some(&process()),
            SignalKind::EventAppendFailed,
            LogFields::new().set("error", "x"),
            Some(&Panicking),
        );
    }
}
