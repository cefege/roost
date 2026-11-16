//! The four browser commands, as this manager answers them.
//!
//! `session::respawn` owns what a RESPAWN does; this owns how the manager
//! answers `kill`, `spawn_shell`, `respawn_if_missing` and `attach`. Split by
//! concept rather than by line count: the four are one contract — a browser can
//! kill a session, open one, replace a lost one, or claim one, and what each
//! answers turns on whether this worker already holds it — and the work each arm
//! does lives in the file that owns it.
//!
//! THE ONE THING ALL FOUR SHARE is the owned handle. `Boxed<T>` is
//! `Pin<Box<dyn Future + Send + 'static>>`, and a `&self` receiver cannot put
//! itself into a `'static` future, so each arm clones the manager's own `Weak`
//! self-handle, upgrades it, and builds its future from the OWNED `Arc` — which
//! is what makes those futures `Send + 'static` while the trait keeps `&self`
//! and stays dyn compatible. A handle that cannot be upgraded is answered with
//! a refusal, not a panic: nothing owns the manager, so a claim taken through it
//! could not be tied to anything durable, which is the condition a reservation
//! exists to refuse.
//!
//! `Arc<Self>` on the receiver was the other answer and the compiler refused it —
//! an undispatchable receiver cannot back a `dyn`. Depends on
//! `super::lifecycle`, `super::respawn` and `roost_worker::browser_commands` — and
//! on nothing that depends on it back.

use crate::browser_commands::Refusal;
use crate::browser_commands::session_lifecycle::{SessionLifecycle, SessionOutcome};
use roost_protocol::wire::brand::SessionId;

use super::lifecycle::SessionManager;
use crate::browser_commands::Boxed;

/// The four browser commands, as this manager answers them.
///
/// All four here, in one place, because they are one contract: a browser can
/// kill a session, open one, replace a lost one, or claim one, and what each
/// answers turns on whether this worker already holds the session. The work each
/// arm does lives in the file that owns it.
impl SessionLifecycle for SessionManager {
    fn kill(&self, session_id: SessionId) -> Boxed<Result<SessionOutcome, Refusal>> {
        // THE OWNED HANDLE, AND WHY THIS IS NOT OPTIONAL. The return type is
        // `Boxed<T>` = `Pin<Box<dyn Future<Output = T> + Send + 'static>>`, and
        // `&self` cannot go into a `'static` future. So the future is built from
        // an UPGRADED `Arc<SessionManager>`, and a manager that cannot be
        // upgraded is answered with a refusal rather than a panic: nothing owns
        // it, so nothing could have tied a claim taken through it to disk.
        match self.owned() {
            Some(owned) => Box::pin(async move { owned.kill_held_session(&session_id).await }),
            None => Box::pin(std::future::ready(Err(Refusal::failed(
                "sessions",
                "this session manager is not owned by anything, so a claim taken \
                 through it could not be recorded; refusing rather than opening \
                 a PTY nobody could close",
            )))),
        }
    }

    fn spawn_shell(
        &self,
        folder: String,
        cols: Option<u16>,
        rows: Option<u16>,
        requested_session_id: Option<SessionId>,
    ) -> Boxed<Result<SessionOutcome, Refusal>> {
        match self.owned() {
            Some(owned) => Box::pin(async move {
                owned
                    .open_shell(folder, cols, rows, requested_session_id)
                    .await
            }),
            None => Box::pin(std::future::ready(Err(Refusal::failed(
                "sessions",
                "this session manager is not owned by anything, so a claim taken \
                 through it could not be recorded; refusing rather than opening \
                 a PTY nobody could close",
            )))),
        }
    }

    fn respawn_if_missing(
        &self,
        session_id: SessionId,
        cwd: String,
        cols: u16,
        rows: u16,
    ) -> Boxed<Result<SessionOutcome, Refusal>> {
        match self.owned() {
            Some(owned) => Box::pin(async move {
                owned
                    .respawn_lost_child(&session_id, &cwd, cols, rows)
                    .await
            }),
            None => Box::pin(std::future::ready(Err(Refusal::failed(
                "sessions",
                "this session manager is not owned by anything, so a claim taken \
                 through it could not be recorded; refusing rather than opening \
                 a PTY nobody could close",
            )))),
        }
    }

    fn attach(
        &self,
        session_id: SessionId,
        from_offset: Option<u64>,
    ) -> Boxed<Result<SessionOutcome, Refusal>> {
        match self.owned() {
            Some(owned) => {
                Box::pin(async move { owned.claim_viewer(&session_id, from_offset).await })
            }
            None => Box::pin(std::future::ready(Err(Refusal::failed(
                "sessions",
                "this session manager is not owned by anything, so a claim taken \
                 through it could not be recorded; refusing rather than opening \
                 a PTY nobody could close",
            )))),
        }
    }
}
