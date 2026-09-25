//! The logging facade every crate in the fleet depends on, and nothing else
//! depends on it: the log line shape `roost status` and `roost doctor` parse,
//! the opt-in diag firehose, the always-on Tier-1 signal channel, and the
//! trace id that correlates a request across every log.
//!
//! One JSON object per line, `{ts, level, target, msg, ...fields}`, written
//! by a `tracing` formatter the host installs with [`init::init`]. Nothing in
//! this crate reads the environment, a clock or the filesystem outside that
//! one function, and no emitter reaches for global state of its own.

#![forbid(unsafe_code)]

pub mod clock;
pub mod diag;
pub mod fields;
pub mod init;
pub mod level;
mod line;
pub mod log;
mod runtime;
pub mod signal;
pub mod signal_kind;
pub mod trace;

pub use clock::{EventClock, SystemClock};
pub use diag::DIAG_ENABLED_ENV;
pub use fields::{LogFields, RecordSink};
pub use init::{DEFAULT_LOG_LEVEL, InitOptions, LOG_LEVEL_ENV, OutputStream, init, init_with};
pub use level::LogLevel;
pub use runtime::{set_diag_sink, set_signal_sink};
pub use signal_kind::SignalKind;
pub use trace::{
    TRACE_HEADER, TRACE_ID_BYTES, TRACE_ID_HEX_LEN, TRACE_ID_MIN_LEN, TraceIdError, as_trace_id,
    is_trace_id, trace_id_from_bytes,
};
