//! Browser-side diagnostic event capture and bounded coordinator upload queue.
//!
//! The tracing layer is always installed; only the authenticated pump owns the
//! wasm flush loop, while native builds retain a testable in-memory queue.

#[cfg(target_arch = "wasm32")]
pub(crate) mod flush;
mod layer;
mod queue;
pub use queue::DiagUploadQueue;

pub(crate) use layer::{DiagUploadLayer, filter};
