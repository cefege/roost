//! Creating, ending, re-creating and attaching to the worker's sessions.
//! Owned by the worker.
//!
//! Four commands, and the differences between them are the whole reason this
//! is one module rather than four files.
//!
//! KILLING AN UNKNOWN SESSION IS NOT AN ERROR. A browser that closes a pane
//! whose worker link dropped a moment earlier is asking for something that is
//! already true, and the answer is the tombstone that tells the coordinator
//! the session ended — not a refusal the SPA has to interpret. A session that
//! is gone and a session that was never here are the same state to the caller.
//!
//! RESPAWN IS IDEMPOTENT AND SAYS SO. A protocol bump wipes the worker's
//! in-memory sessions while the coordinator's rows and the browser's terminal
//! are still there, so a reattach has to re-create what is missing and
//! nothing else. The reply names whether it re-created or adopted, because a
//! browser that adopted must not paint a "started" moment it did not have.
//!
//! A DURABILITY FAILURE IS NOT A COMMAND FAILURE. Losing the ability to record
//! a session's end means the coordinator will believe a dead session is alive
//! forever, and that is a condition no per-request answer can fix. It is
//! reported as its own outcome so the caller can stop the worker rather than
//! retry.

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};

/// The geometry a session falls back to when the client names none.
pub const DEFAULT_COLS: u16 = 80;
/// The geometry a session falls back to when the client names none.
pub const DEFAULT_ROWS: u16 = 24;

/// What a session command did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    /// The session was ended, or was already ended. Both are the same state to
    /// the caller, and the tombstone below is what the coordinator reads.
    Killed,
    /// A new PTY was opened.
    Spawned { channel_id: u16 },
    /// A keeper survivor already held this session, so nothing was opened.
    AlreadyLive,
    /// A viewer claimed an existing session.
    Attached { replay_offset: u64 },
    /// The worker could not record the outcome, so the caller must stop it.
    ///
    /// Distinct from every other failure because retrying does not help and
    /// answering the browser does not either.
    DurabilityLost,
}

/// The worker's own sessions, as a browser command acts on them.
pub trait SessionLifecycle: Send + Sync {
    fn kill(&self, session_id: SessionId) -> Boxed<Result<SessionOutcome, Refusal>>;

    fn spawn_shell(
        &self,
        folder: String,
        cols: Option<u16>,
        rows: Option<u16>,
        requested_session_id: Option<SessionId>,
    ) -> Boxed<Result<SessionOutcome, Refusal>>;

    /// Re-create a session only if the worker does not already hold it.
    fn respawn_if_missing(
        &self,
        session_id: SessionId,
        cwd: String,
        cols: u16,
        rows: u16,
    ) -> Boxed<Result<SessionOutcome, Refusal>>;
    fn attach(
        &self,
        session_id: SessionId,
        from_offset: Option<u64>,
    ) -> Boxed<Result<SessionOutcome, Refusal>>;
}

/// Run whichever session command arrived.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let request_id = command.request_id.as_str();
    match &command.frame {
        ClientControlFrame::Kill { session_id, .. } => {
            answer(
                deps.sessions.kill(session_id.clone()).await,
                request_id,
                |outcome| {
                    // Nothing. A kill's whole answer is the tombstone the session
                    // layer writes, and a browser that closed a pane is not waiting
                    // for one. A DURABILITY failure still answers, because a
                    // request that is neither answered nor refused times out
                    // against a cause nobody wrote down.
                    match outcome {
                        SessionOutcome::DurabilityLost => {
                            Reply::error(request_id, "the session could not be recorded as ended")
                        }
                        SessionOutcome::Killed
                        | SessionOutcome::Spawned { .. }
                        | SessionOutcome::AlreadyLive
                        | SessionOutcome::Attached { .. } => {
                            Reply::ok(request_id, serde_json::json!({}))
                        }
                    }
                },
            )
        }
        ClientControlFrame::SpawnShell {
            folder,
            cols,
            rows,
            session_id,
            ..
        } => {
            let spawned = deps
                .sessions
                .spawn_shell(
                    folder.clone(),
                    cols.map(|value| value as u16),
                    rows.map(|value| value as u16),
                    session_id.clone(),
                )
                .await?;
            Ok(Answered::Reply(Reply::ok(
                request_id,
                serde_json::json!({
                    "session_id": session_id.as_ref().map(SessionId::as_str),
                    "channel_id": channel_of(&spawned),
                }),
            )))
        }
        ClientControlFrame::RespawnIfMissing {
            session_id,
            cwd,
            cols,
            rows,
            ..
        } => {
            let outcome = deps
                .sessions
                .respawn_if_missing(
                    session_id.clone(),
                    cwd.clone(),
                    geometry(*cols),
                    geometry(*rows),
                )
                .await?;
            Ok(Answered::Reply(Reply::ok(
                request_id,
                serde_json::json!({
                    "session_id": session_id.as_str(),
                    "channel_id": channel_of(&outcome),
                    "already_live": outcome == SessionOutcome::AlreadyLive,
                }),
            )))
        }
        ClientControlFrame::Attach {
            session_id,
            from_offset,
            ..
        } => {
            let offset = from_offset.and_then(|offset| u64::try_from(offset).ok());
            let outcome = deps.sessions.attach(session_id.clone(), offset).await?;
            let SessionOutcome::Attached { replay_offset } = outcome else {
                return Err(Refusal::failed(
                    "attach",
                    "the session layer attached a session it did not claim",
                ));
            };
            Ok(Answered::Reply(Reply::ok(
                request_id,
                serde_json::json!({ "replay_offset": replay_offset }),
            )))
        }
        other => Err(Refusal::failed(
            "sessions",
            format!("{} is not a session command", other.kind()),
        )),
    }
}

/// The answer for a command whose reply shape depends on what happened.
fn answer(
    outcome: Result<SessionOutcome, Refusal>,
    _request_id: &str,
    shape: impl FnOnce(SessionOutcome) -> Reply,
) -> Result<Answered, Refusal> {
    outcome.map(|outcome| Answered::Reply(shape(outcome)))
}

/// A geometry the wire carried, narrowed to what a PTY can be sized to.
///
/// A client that named none gets the fallback the frame already defaulted to,
/// and a client that named an absurd one is clamped rather than refused: the
/// terminal is wrong either way and a PTY cannot be opened at a size the
/// kernel does not have.
fn geometry(value: i64) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// The channel a session landed on, when one was opened.
fn channel_of(outcome: &SessionOutcome) -> Option<u16> {
    match outcome {
        SessionOutcome::Spawned { channel_id } => Some(*channel_id),
        SessionOutcome::AlreadyLive => None,
        SessionOutcome::Killed
        | SessionOutcome::Attached { .. }
        | SessionOutcome::DurabilityLost => None,
    }
}
