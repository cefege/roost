//! Opt-in terminal incident capture: the always-on byte window, the armed
//! recorder's segment chain, emitted-frame fold and core samples, and the one
//! gzip bundle a capture freezes. Ports `apps/worker/src/diag/
//! {terminal-capture*,byte-capture,capture-storage}.ts`. `runtime` builds ONE
//! [`CaptureRecorder`]; the session data path feeds it through [`CaptureTap`];
//! `browser_commands::diagnostics` dispatches the wire steps to it.
//!
//! The capture WIRE — the command, the acknowledgement and every bound — and
//! the bundle's JSON shape and validator are `roost_protocol::terminal_capture`
//! and are not restated here. The files below are split by what they own:
//! [`byte_window`] the always-on tail, [`storage`] the only writer of capture
//! files, [`registry`] the armed recorders, [`recorder_state`] and [`pools`]
//! one recording's bounded state, [`emission`] the accepted-frame fold,
//! [`worker_section`], [`section_grid`] and [`section_coverage`] the freeze,
//! [`evidence`] the remote layers, [`bundle_writer`] the budget fit and gate,
//! [`write`] and [`finish`] one capture's flow, [`ack`] its answers,
//! [`recorder`] the lease façade and [`tap`] the data-path handle.

pub mod ack;
pub mod bundle_writer;
pub mod byte_window;
pub mod emission;
pub mod evidence;
pub mod finish;
pub mod pools;
pub mod recorder;
pub mod recorder_state;
pub mod registry;
pub mod section_coverage;
pub mod section_grid;
pub mod storage;
pub mod tap;
pub mod worker_section;
pub mod write;

pub use recorder::{CaptureRecorder, CaptureRecorderDeps};
pub use tap::{CaptureTap, ResizeBoundaryNote};

/// How many bytes of raw PTY output are retained per session for the incident
/// stream, whether or not a recording is armed (v2 `RING_CAP_BYTES`).
///
/// ALWAYS ON, deliberately. An anomaly fires when the diagnostic gate was off,
/// and a recorder that only retained bytes while armed would find an empty
/// window at exactly the moment it is needed.
pub const BYTE_CAPTURE_WINDOW_BYTES: usize = 256 * 1024;

/// Wall-clock milliseconds, the clock v2's recorder dates every record and
/// lease with (`Date.now()`).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}
