//! Port of `packages/agent/src/harness/tools/tool-context.ts` @ pi 0.82.1
//! (2efa728) — the context handed to the built-in execution tools.
//!
//! Upstream is a structural interface (`{ env: ExecutionEnv }`, tool-context.ts
//! :4-6). Rust has no structural typing, so the `env` property becomes the
//! [`ToolContext`] trait (the `TContext extends ExecutionToolContext` bound of
//! the tool factories, e.g. read.ts:45), and the concrete default context keeps
//! the upstream name [`ExecutionToolContext`].
//!
//! Intentional differences:
//! - `env()` returns `Arc<dyn ExecutionEnv>` instead of a bare reference: the
//!   bash tool must hand the whole `TContext` to its `prepare` hook by value,
//!   so the env handle has to be independent of the context's lifetime.

use std::sync::Arc;

use crate::harness::types::ExecutionEnv;

/// `TContext extends ExecutionToolContext` (tool-context.ts:4-6) — anything the
/// built-in execution tools can run against must expose an execution env.
///
/// The `'static` bound mirrors the bash tool's `prepare` hook, whose stored
/// closure type (`Arc<dyn Fn(..., TContext, ...) -> BoxFuture<'static, ()>>`)
/// requires `TContext: 'static` to be well-formed.
pub trait ToolContext: Send + Sync + 'static {
    /// The filesystem and shell environment of the current turn snapshot.
    fn env(&self) -> Arc<dyn ExecutionEnv>;
}

/// `ExecutionToolContext` (tool-context.ts:4-6) — the default concrete context:
/// an execution env handle.
#[derive(Clone)]
pub struct ExecutionToolContext {
    pub env: Arc<dyn ExecutionEnv>,
}

impl ExecutionToolContext {
    pub fn new(env: Arc<dyn ExecutionEnv>) -> Self {
        ExecutionToolContext { env }
    }
}

impl ToolContext for ExecutionToolContext {
    fn env(&self) -> Arc<dyn ExecutionEnv> {
        Arc::clone(&self.env)
    }
}
