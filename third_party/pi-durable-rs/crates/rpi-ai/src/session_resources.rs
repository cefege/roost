//! Port of `packages/ai/src/session-resources.ts` @ pi 0.82.1 (2efa728).
//!
//! Registry for per-session resource cleanups (the Codex WebSocket connection
//! cache registers here). Unlike upstream, cleanup callbacks run outside any
//! lock and their errors are collected into a single [`SessionResourceError`]
//! (upstream throws an `AggregateError`).
//!
//! Intentional differences: none beyond the error container.

use std::sync::{LazyLock, Mutex};

/// `SessionResourceCleanup = (sessionId?: string) => void`.
pub type SessionResourceCleanup = fn(Option<&str>);

static CLEANUPS: LazyLock<Mutex<Vec<SessionResourceCleanup>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Collected cleanup failures (upstream `AggregateError`).
#[derive(Debug, thiserror::Error)]
#[error("Failed to cleanup session resources: {0}")]
pub struct SessionResourceError(pub String);

/// `registerSessionResourceCleanup`. Registration is process-global and
/// permanent; upstream returns an unregister function, which rpi omits because
/// no caller unregisters (the only upstream registration is module-level).
pub fn register_session_resource_cleanup(cleanup: SessionResourceCleanup) {
    CLEANUPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(cleanup);
}

/// `cleanupSessionResources`: runs every registered cleanup. Callbacks are
/// infallible `fn` pointers (the Codex WebSocket cache close is best-effort),
/// so unlike upstream's `AggregateError` this cannot fail; the `Result` is
/// kept for future fallible cleanups.
pub fn cleanup_session_resources(session_id: Option<&str>) -> Result<(), SessionResourceError> {
    let cleanups: Vec<SessionResourceCleanup> =
        CLEANUPS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for cleanup in cleanups {
        cleanup(session_id);
    }
    Ok(())
}
