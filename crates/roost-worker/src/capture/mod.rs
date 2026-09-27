//! The terminal capture recorder: what this worker retains when a recording is
//! armed, and the evidence bundle a capture request freezes. The capture WIRE —
//! the command, the acknowledgement and every bound — is
//! `roost_protocol::terminal_capture`, and the browser-command shape is
//! `browser_commands::diagnostics`; neither is restated here. Depends on
//! `roost_protocol` — and on nothing here.
//!
//! WHY THIS MODULE EXISTS AT ALL, given the above. The recorder is the only
//! thing in the worker that writes PTY bytes to disk while nothing is wrong, so
//! its bounds and its always-on byte window are decisions somebody has to own.
//! They are owned here rather than beside the protocol because they are
//! decisions about this worker's disk, not about the message two peers exchange.
//!
//! The three files are split by what they own and by nothing else:
//! [`byte_window`] holds the always-on tail, [`bundle`] is the only writer of
//! the capture directory, [`leases`] owns the armed state and the rules that
//! move it, and [`recorder`] is the capability that joins them to the
//! protocol's vocabulary.

pub mod bundle;
pub mod byte_window;
pub mod leases;
pub mod recorder;

pub use recorder::CaptureRecorder;

/// How many bytes of raw PTY output are retained per session for the incident
/// stream, whether or not a recording is armed.
///
/// ALWAYS ON, deliberately. An anomaly fires when the diagnostic gate was off,
/// and a recorder that only retained bytes while armed would find an empty
/// window at exactly the moment it is needed. One integer compare is the whole
/// cost when nothing is armed.
pub const BYTE_CAPTURE_WINDOW_BYTES: usize = 256 * 1024;

/// Where a capture recorder writes its working files, under this worker's own
/// state directory rather than a session's attachment directory.
///
/// Separate from the attachment tree on purpose: an attachment directory is
/// synced and offered to peers, and evidence bundles are neither. It sits
/// inside the worker's log directory, which is the one directory the worker
/// already owns exclusively — v2's `capture-storage.ts` puts it there for the
/// same reason, and a second top-level directory would be a second thing to
/// make owner-only.
pub const CAPTURE_DIR_NAME: &str = "captures";
