//! Portable execution environment (port of `packages/durable/src/env/*`
//! @ `abe508e1b`).
//!
//! The module carries the upstream `ExecutionEnv` surface: the result-shaped
//! [`FileSystem`] and [`Shell`] capability traits, the snapshot-scanning file
//! watcher, the streaming UTF-8 decoders and the line scanner, and the local
//! Tokio/std implementation [`node::NodeExecutionEnv`].
//!
//! Rust shape notes (registered V17-15 §7.3): upstream's `Result<T, E>` union
//! maps to `std::result::Result`; option objects map to option structs; the
//! `onOutput` callback is an `Arc<dyn Fn>`.

pub mod decode;
pub mod line_scan;
pub mod node;
pub mod node_watch;
pub mod types;

pub use decode::{StreamDecoder, Utf8StreamDecoder, range_decoder, starts_with_bom};
pub use line_scan::{InvalidLineRange, LineScanError, LineScanner};
pub use node::{NodeExecutionEnv, NodeExecutionEnvOptions};
pub use node_watch::{NodeFileWatcher, NodeWatchMode, NodeWatchOptions};
pub use types::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirPage, DirReader, ExcludeOptions,
    ExecutionEnv, ExecutionError, ExecutionErrorCode, FileContent, FileInfo, FileKind, FileSystem,
    FileWatcher, LineScan, OpenBinaryReaderOptions, ReadTextLinesOptions, RemoveOptions,
    ScanLinesOptions, Shell, ShellCommand, ShellExecOptions, ShellExecResult, ShellOutputInfo,
    ShellOutputSkip, ShellOutputWindow, ShellSpillOptions, ShellStream, TextLine, TextLineReader,
    WatchChange, WatchMode, WatchTarget,
};

pub use crate::errors::{FileError, FileErrorCode};

use crate::chord::context::Context;

/// Change callback installed by [`FileSystem::watch`].
pub type WatchCallback = std::sync::Arc<dyn Fn(WatchChange) + Send + Sync>;

/// Abort marker embedded in environment callbacks (upstream `AbortError`).
pub const ABORTED_MESSAGE: &str = "aborted";

/// A context-derived abort check shared by the local implementation.
pub(crate) fn context_aborted(context: &Context) -> bool {
    context
        .abort_signal()
        .map(|signal| signal.is_cancelled())
        .unwrap_or(false)
}
