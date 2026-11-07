//! Port of `packages/agent/src/stream-fn.ts` @ pi 0.82.1 (2efa728), with the
//! function shape pinned by design doc §4.4.
//!
//! Intentional differences:
//! - Upstream `StreamFn` takes `SimpleStreamOptions` and may return the stream
//!   or a promise of it; design doc §4.4 pins the simplified shape
//!   `Fn(Model, Context, StreamOptions) -> BoxStream<'static, StreamEvent>`.
//!
//! Contract (unchanged from upstream):
//! - Must not panic for request/model/runtime failures.
//! - Failures must be encoded in the returned stream via protocol events and a
//!   final assistant message with stopReason "error" or "aborted" and
//!   errorMessage.

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use rpi_ai::types::{Model, StreamEvent, StreamOptions, TranscriptContext};

/// Boxed, sendable stream of [`StreamEvent`]s.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// Stream function injected into the agent loop. `rpi-ai`'s
/// `Models::stream_simple` (adapted to this shape by the assembly layer)
/// satisfies it in production; tests inject faux streams (coding-standards
/// §4.2).
pub type StreamFn = Arc<
    dyn Fn(Model, TranscriptContext, StreamOptions) -> BoxStream<'static, StreamEvent>
        + Send
        + Sync,
>;
