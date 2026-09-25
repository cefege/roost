//! The opt-in diagnostic firehose: ONE call, one JSON record, off by default.
//! Left on it is a documented trap (a coordinator spends a fifth of its CPU
//! and writes gigabytes a day of `*.out.log`), and `roost doctor` reads
//! `*.err.log`, not this. Enabled when `ROOST_DIAG=1`, or when the browser
//! front end passes `localStorage.roostDiag === "1"` into
//! [`crate::InitOptions::with_diag_enabled`] — the browser half cannot be read
//! from a crate that builds for native and wasm, so the host reads it and
//! hands it over. It is read once at startup and is deliberately not
//! runtime-togglable, so the hot path stays one boolean test.

use crate::fields::{LogFields, RecordSink};
use crate::log;
use crate::runtime::{self, EVENT_FIELD, MONO_NS_FIELD, Process};

/// The log target diag records carry, so the operational facade keeps its own
/// namespace: `rg '"target":"diag"' main.out.log`.
pub const DIAG_TARGET: &str = "diag";

/// The environment variable that turns the firehose on.
pub const DIAG_ENABLED_ENV: &str = "ROOST_DIAG";

/// The only value [`DIAG_ENABLED_ENV`] accepts. Anything else, including `0`,
/// leaves the firehose off.
pub const DIAG_ENABLED_VALUE: &str = "1";

/// The browser storage key the front end reads and passes to
/// [`crate::InitOptions::with_diag_enabled`]. Named here so the key lives in
/// one place on both sides of the port.
pub const DIAG_STORAGE_KEY: &str = "roostDiag";

/// The message a panicking sink is reported under. One event, then the loss.
const SINK_THREW: &str = "sink_threw";

/// Why the sink failed, as a string: a panic payload is not JSON and must not
/// be the thing the reporting line itself trips over.
const SINK_PANICKED: &str = "diagnostic sink panicked";

/// Resolve [`DIAG_ENABLED_ENV`]. The one reader is `init`.
pub fn enabled_from_env_value(raw: Option<&str>) -> bool {
    raw == Some(DIAG_ENABLED_VALUE)
}

/// Whether the firehose is on in this process. `false` before `init`.
pub fn is_diag_enabled() -> bool {
    runtime::process().is_some_and(|process| process.diag_enabled)
}

/// Emit ONE diagnostic event, or nothing at all. The record is
/// `{evt, mono_ns, ...kv}`: the caller's `kv` is spliced in last, so a `kv`
/// naming `evt` or `mono_ns` wins, exactly as the object spread did.
/// `mono_ns` is a per-process tiebreak for when the wall clock collides at
/// sub-millisecond resolution, and it comes from the clock `init` installed
/// rather than from a clock read behind the caller's back.
pub fn emit(evt: &str, kv: LogFields) {
    let Some(process) = runtime::process() else {
        return;
    };
    if !process.diag_enabled {
        return;
    }
    let sink = runtime::host().diag_sink();
    emit_record(process, sink.as_deref(), evt, kv);
}

/// The gate and the sink are the caller's; this half is the record.
fn emit_record(process: &Process, sink: Option<&dyn RecordSink>, evt: &str, kv: LogFields) {
    let mut record = LogFields::new().set(EVENT_FIELD, evt);
    record.put(MONO_NS_FIELD, process.clock.mono_ns());
    record.absorb(kv);
    match sink {
        Some(sink) => {
            if !runtime::call_sink(sink, &record) {
                log::warn(
                    DIAG_TARGET,
                    SINK_THREW,
                    LogFields::new()
                        .set(EVENT_FIELD, evt)
                        .set("error", SINK_PANICKED),
                );
            }
        }
        None => log::info(DIAG_TARGET, evt, record),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::{DIAG_STORAGE_KEY, EVENT_FIELD, emit_record, enabled_from_env_value};
    use crate::clock::FixedClock;
    use crate::fields::{LogFields, RecordSink};
    use crate::runtime::Process;

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

    struct Panicking;

    impl RecordSink for Panicking {
        fn emit(&self, _record: &LogFields) {
            panic!("a sink that throws into the path it observes");
        }
    }

    fn process(enabled: bool) -> Process {
        Process::new(enabled, Arc::new(FixedClock::new(1_700_000_000_000, 4_242)))
    }

    #[test]
    fn only_the_exact_string_one_turns_the_firehose_on() {
        assert!(enabled_from_env_value(Some("1")));
        assert!(!enabled_from_env_value(Some("0")));
        assert!(!enabled_from_env_value(Some("true")));
        assert!(!enabled_from_env_value(Some("")));
        assert!(!enabled_from_env_value(None));
    }

    #[test]
    fn the_browser_half_is_named_so_the_front_end_reads_the_right_key() {
        assert_eq!(DIAG_STORAGE_KEY, "roostDiag");
    }

    #[test]
    fn a_record_is_evt_mono_ns_then_the_callers_kv_verbatim() {
        let sink = Capturing::default();
        emit_record(
            &process(true),
            Some(&sink),
            "viewport.claim",
            LogFields::new()
                .set("sid", "abc")
                .set("viewer_key", "vk1")
                .set("cols", 80)
                .set("client_seq", 3),
        );
        let records = sink.take();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].to_value(),
            json!({
                "evt": "viewport.claim",
                "mono_ns": 4_242,
                "sid": "abc",
                "viewer_key": "vk1",
                "cols": 80,
                "client_seq": 3,
            })
        );
    }

    #[test]
    fn a_caller_kv_that_names_a_fixed_key_wins_it() {
        let sink = Capturing::default();
        emit_record(
            &process(true),
            Some(&sink),
            "bytes.chunk",
            LogFields::new().set(EVENT_FIELD, "caller.evt"),
        );
        assert_eq!(sink.take()[0].get(EVENT_FIELD), Some(&json!("caller.evt")));
    }

    #[test]
    fn a_sink_that_panics_costs_one_event_not_the_callers_path() {
        // The gate passed and the sink blew up; the call still returns.
        emit_record(
            &process(true),
            Some(&Panicking),
            "bytes.up_send",
            LogFields::new().set("input_seq", 9),
        );
    }
}
