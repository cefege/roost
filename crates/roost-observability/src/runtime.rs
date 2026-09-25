//! Process-wide wiring. Two published values, each with exactly one writer:
//! `init` owns the diag gate and the clock, and the host owns the optional
//! sinks and the Tier-1 cooldown history. Neither is mutable after it is
//! published — a `OnceLock`/`LazyLock`, never a `static mut` — and neither is
//! read from a place the caller cannot see.

use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};

use crate::clock::EventClock;
use crate::fields::{LogFields, RecordSink};
use crate::signal::CooldownMap;

/// The `evt` key every diag and signal record carries.
pub(crate) const EVENT_FIELD: &str = "evt";

/// The `mono_ns` tiebreak key, on every diag and signal record.
pub(crate) const MONO_NS_FIELD: &str = "mono_ns";

/// What `init` decided once: whether the firehose is on, and whose clock
/// stamps the lines. Absent means `init` never ran, which reads as "firehose
/// off" and "no clock" rather than as a default the caller did not choose.
#[derive(Clone)]
pub(crate) struct Process {
    pub(crate) diag_enabled: bool,
    pub(crate) clock: Arc<dyn EventClock>,
}

impl Process {
    pub(crate) fn new(diag_enabled: bool, clock: Arc<dyn EventClock>) -> Self {
        Self {
            diag_enabled,
            clock,
        }
    }
}

impl fmt::Debug for Process {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Process")
            .field("diag_enabled", &self.diag_enabled)
            .field("clock", &self.clock)
            .finish()
    }
}

static PROCESS: OnceLock<Process> = OnceLock::new();

/// The installed process, or `None` before `init`.
pub(crate) fn process() -> Option<&'static Process> {
    PROCESS.get()
}

/// Publish the process. Called by `init` only; a second call is ignored
/// because the gate and the clock are read-once decisions.
pub(crate) fn install_process(process: Process) {
    let _already_installed = PROCESS.set(process);
}

/// What a host can install on top of the facade: a destination for whole
/// records, and the memory of which Tier-1 signals already fired.
#[derive(Default)]
pub(crate) struct Host {
    diag_sink: Mutex<Option<Arc<dyn RecordSink>>>,
    signal_sink: Mutex<Option<Arc<dyn RecordSink>>>,
    cooldowns: Mutex<CooldownMap>,
}

impl Host {
    pub(crate) fn diag_sink(&self) -> Option<Arc<dyn RecordSink>> {
        read_slot(&self.diag_sink)
    }

    pub(crate) fn signal_sink(&self) -> Option<Arc<dyn RecordSink>> {
        read_slot(&self.signal_sink)
    }

    pub(crate) fn cooldowns(&self) -> MutexGuard<'_, CooldownMap> {
        lock(&self.cooldowns)
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Host")
            .field("diag_sink_installed", &self.diag_sink().is_some())
            .field("signal_sink_installed", &self.signal_sink().is_some())
            .finish()
    }
}

static HOST: LazyLock<Host> = LazyLock::new(Host::default);

/// The process-wide host state, created on first use by whichever of the
/// setters or the emitters runs first.
pub(crate) fn host() -> &'static Host {
    &HOST
}

/// Route diag records to `sink` instead of to the log line. The browser front
/// end installs a batched coordinator-shipping sink here; coord and worker
/// leave it unset and records go to stdout.
pub fn set_diag_sink(sink: Option<Arc<dyn RecordSink>>) {
    *lock(&HOST.diag_sink) = sink;
}

/// Route Tier-1 signal records to `sink` instead of to the stderr log line.
/// The browser front end installs an always-on sink that ships to the
/// coordinator.
pub fn set_signal_sink(sink: Option<Arc<dyn RecordSink>>) {
    *lock(&HOST.signal_sink) = sink;
}

/// Hand one record to a sink. An observer must never break what it observes
/// — terminal input emits one diagnostic per keystroke — so a sink that panics
/// costs this ONE event and is reported as `sink_threw`. `false` means the
/// sink panicked; the caller reports it with strings only, so the failure line
/// cannot trip on the same value that broke the sink.
pub(crate) fn call_sink(sink: &dyn RecordSink, record: &LogFields) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.emit(record))).is_ok()
}

/// Take a poisoned lock rather than propagating another thread's panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_slot<T: Clone>(mutex: &Mutex<Option<T>>) -> Option<T> {
    lock(mutex).clone()
}
