//! Browser→worker command dispatch: the envelope, the table naming each
//! command's owner, and the refusal every caller is owed. `runtime::link_drain`
//! is the only caller, and it is the only thing that puts a reply on the link.
//!
//! The split below is by concept — which capability executes a command — and
//! not by the TypeScript files it replaces. v2 kept one `switch` and four
//! handler files, and the boundaries that survived are the ones that name a
//! capability: sessions, presence, the filesystem, a retained grid read, a
//! search, the attachment store, and diagnostics.
//!
//! ONE RULE: every command that asks for an answer gets one, correlated by the
//! envelope's `request_id`. A command that is neither executed nor refused
//! leaves the coordinator's pending-RPC entry to expire, and the browser sees a
//! deadline with no cause in any log — which is why the routing is a TABLE
//! rather than a `match`. A kind with no row is a refusal a test can observe;
//! a missing `match` arm is a compile error nobody reads.

use std::pin::Pin;
use std::sync::Arc;

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

pub mod attachments;
pub mod diagnostics;
pub mod file_commands;
pub mod presence;
pub mod replies;
pub mod scrollback_page;
pub mod search;
pub mod search_cancellation;
pub mod session_lifecycle;

pub use replies::Reply;

/// A future a capability trait returns, boxed so the trait stays object-safe
/// while the work behind it is asynchronous.
///
/// Owned rather than borrowed: a command's inputs move into the future, so
/// nothing the caller holds is kept alive across the await and a capability
/// can be dropped while one of its commands is still in flight.
pub type Boxed<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// One browser command as the coordinator relayed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    /// Opaque to the worker: it does not learn who is watching.
    pub browser_id: String,
    pub viewer_id: String,
    /// The coordinator's own correlation id. The reply echoes THIS one and not
    /// the frame's inner `request_id`: the coordinator keys its pending-RPC
    /// table on the envelope, so answering the inner id would answer a request
    /// nobody is waiting on.
    pub request_id: String,
    pub frame: ClientControlFrame,
}

impl Command {
    pub fn new(
        browser_id: impl Into<String>,
        viewer_id: impl Into<String>,
        request_id: impl Into<String>,
        frame: ClientControlFrame,
    ) -> Self {
        Self {
            browser_id: browser_id.into(),
            viewer_id: viewer_id.into(),
            request_id: request_id.into(),
            frame,
        }
    }

    /// Decode a command off the wire, or say why the frame is not one.
    ///
    /// This is the worker's front door, and the only place a frame's own
    /// admission runs. A kind this build does not know, a field out of range,
    /// and a key a strict frame does not define are all refused HERE: a frame
    /// that decoded and then had no owner would otherwise reach a handler that
    /// cannot honour it.
    pub fn decode(
        browser_id: impl Into<String>,
        viewer_id: impl Into<String>,
        request_id: impl Into<String>,
        value: serde_json::Value,
    ) -> Result<Self, String> {
        let frame = ClientControlFrame::parse(value).map_err(|error| error.to_string())?;
        Ok(Self::new(browser_id, viewer_id, request_id, frame))
    }

    /// The session this command names, for the commands that name one.
    ///
    /// A file or a directory command names none, and asking it for a session is
    /// a bug in the dispatch rather than something to answer.
    pub fn session_id(&self) -> Option<&SessionId> {
        match &self.frame {
            ClientControlFrame::Kill { session_id, .. }
            | ClientControlFrame::Attach { session_id, .. }
            | ClientControlFrame::Detach { session_id, .. }
            | ClientControlFrame::SetTitle { session_id, .. }
            | ClientControlFrame::CursorPos { session_id, .. }
            | ClientControlFrame::GitDiff { session_id, .. }
            | ClientControlFrame::RespawnIfMissing { session_id, .. } => Some(session_id),
            _ => None,
        }
    }
}

/// What executing a command produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Answered {
    /// One frame to put on the link.
    Reply(Reply),
    /// Nothing to put on the link.
    ///
    /// For the commands that ask for no answer. A browser that walked away from
    /// a search is not waiting for the cancel to be acknowledged, and a frame
    /// the coordinator never registered a pending entry for has nobody to
    /// answer. Silence here is a property of the wire, not an omission.
    Silent,
}

/// Which capability executes a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// Creating, ending and re-creating the worker's sessions.
    Sessions,
    /// What a viewer is doing right now, and the title it gave a session.
    Presence,
    /// Reading files, listing directories, making directories, naming a home.
    Files,
    /// One bounded page out of a session's authoritative grid.
    RetainedGrid,
    /// A bounded cursor search over one session or a page-budget's worth.
    Search,
    /// The on-disk store behind a session's attachment directory.
    Attachments,
    /// What the worker is willing to say about itself.
    Diagnostics,
    /// The capability this build does not have, and the reason a caller is
    /// given instead of silence. Answering is the point: a dropped command
    /// leaves a request to time out rather than fail.
    Absent(&'static str),
}

/// One row of the dispatch table: a wire kind and the concept that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandOwner {
    pub kind: &'static str,
    pub owner: Owner,
}

/// Every command kind the worker answers, and who runs it.
///
/// The table IS the dispatch: [`owner_of`] resolves a frame through it and
/// [`dispatch`] refuses a kind it does not name. `tests/browser_commands.rs`
/// walks the whole `ClientControlFrame` union against it, so deleting a row
/// fails a test instead of quietly turning a command into one that is never
/// answered.
pub const OWNERS: &[CommandOwner] = &[
    cmd("attach", Owner::Sessions),
    cmd("cursor-pos", Owner::Presence),
    cmd("spawn-shell", Owner::Sessions),
    cmd("kill", Owner::Sessions),
    cmd("read-file", Owner::Files),
    cmd("read-file-chunk", Owner::Files),
    cmd("attachment-probe", Owner::Attachments),
    cmd("list-dir", Owner::Files),
    cmd("mkdir", Owner::Files),
    cmd(
        "list-skills",
        Owner::Absent("this worker has no skills directory to list"),
    ),
    cmd(
        "git-diff",
        Owner::Absent("this worker does not compute session diffs"),
    ),
    cmd("set-title", Owner::Presence),
    cmd("get-home", Owner::Files),
    cmd("get-scrollback-cells", Owner::RetainedGrid),
    cmd("search-scrollback", Owner::Search),
    cmd("cancel-scrollback-search", Owner::Search),
    cmd("search-scrollback-batch", Owner::Search),
    cmd("cancel-scrollback-search-batch", Owner::Search),
    cmd("list-attachments", Owner::Attachments),
    cmd("delete-attachment", Owner::Attachments),
    cmd("diag-terminal-capture", Owner::Diagnostics),
    cmd("diag-snapshot", Owner::Diagnostics),
    cmd("respawn-if-missing", Owner::Sessions),
    cmd("detach", Owner::Presence),
];

const fn cmd(kind: &'static str, owner: Owner) -> CommandOwner {
    CommandOwner { kind, owner }
}

/// The owner of a command kind, or the refusal for a kind nothing owns.
pub fn owner_of(kind: &str) -> Result<Owner, Refusal> {
    OWNERS
        .iter()
        .find(|row| row.kind == kind)
        .map(|row| row.owner)
        .ok_or_else(|| Refusal::NoRoute {
            kind: kind.to_owned(),
        })
}

/// Why a command was not executed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// Nothing in [`OWNERS`] names this kind.
    ///
    /// Reachable for a kind a NEWER build added: the frame decodes, and this
    /// build has no owner for it. Refusing is the only answer that leaves the
    /// caller a cause.
    #[error("no worker capability owns the command `{kind}`")]
    NoRoute { kind: String },
    /// The capability that would run this command is not installed.
    #[error("`{kind}`: {reason}")]
    Unavailable {
        kind: &'static str,
        reason: &'static str,
    },
    /// The capability ran and refused, permanently.
    #[error("`{kind}`: {message}")]
    Failed { kind: &'static str, message: String },
}

impl Refusal {
    /// The refusal as the message the browser is answered with.
    ///
    /// Bounded, because a refusal crosses a trust boundary and an error text
    /// can quote the bytes it failed on — which, for a capability backed by a
    /// terminal, is the user's screen.
    pub fn message(&self) -> String {
        const MAX_CHARS: usize = 200;
        self.to_string().chars().take(MAX_CHARS).collect()
    }

    pub fn unavailable(kind: &'static str, reason: &'static str) -> Self {
        Self::Unavailable { kind, reason }
    }

    pub fn failed(kind: &'static str, message: impl Into<String>) -> Self {
        Self::Failed {
            kind,
            message: message.into(),
        }
    }
}

/// The capabilities a dispatch runs against.
///
/// One field per concept, each a trait rather than a concrete type: the session
/// layer, the search engine and the terminal-capture recorder belong to other
/// slices of this crate, and a dispatch that named their concrete types would
/// be a second copy of a decision those slices own.
#[derive(Clone)]
pub struct Deps {
    pub sessions: Arc<dyn session_lifecycle::SessionLifecycle>,
    pub presence: Arc<dyn presence::PresenceReports>,
    pub files: Arc<dyn file_commands::FileCommands>,
    pub grid: Arc<dyn scrollback_page::RetainedGrid>,
    pub search: Arc<dyn search::ScrollbackSearch>,
    /// The searches this worker is running and the cancels waiting to meet
    /// one. Separate from the scanner because admission is a decision about
    /// this worker and the scan is work on a grid.
    pub searches: Arc<std::sync::Mutex<search::Searches>>,
    pub attachments: Arc<dyn attachments::AttachmentStore>,
    pub diagnostics: Arc<dyn diagnostics::DiagnosticReports>,
}

impl std::fmt::Debug for Deps {
    /// The collaborators are interfaces and their debug output is not the
    /// dispatch's to choose.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Deps")
            .field("commands_owned", &OWNERS.len())
            .finish_non_exhaustive()
    }
}

/// Run one command and return the frames to put on the link.
///
/// The frames are returned rather than sent so the dispatch can be exercised
/// without a socket, and so the link keeps the only place that writes bytes.
pub async fn dispatch(command: &Command, deps: &Deps) -> Vec<CoordWorkerUpstream> {
    let kind = command.frame.kind();
    tracing::debug!(kind, request_id = %command.request_id, "browser command arrived");
    let row = match OWNERS.iter().find(|row| row.kind == kind) {
        Some(row) => *row,
        None => {
            return refuse(
                command,
                Refusal::NoRoute {
                    kind: kind.to_owned(),
                },
            );
        }
    };
    let answered = match row.owner {
        Owner::Sessions => session_lifecycle::execute(command, deps).await,
        Owner::Presence => presence::execute(command, deps).await,
        Owner::Files => file_commands::execute(command, deps).await,
        Owner::RetainedGrid => scrollback_page::execute(command, deps).await,
        Owner::Search => search::execute(command, deps).await,
        Owner::Attachments => attachments::execute(command, deps).await,
        Owner::Diagnostics => diagnostics::execute(command, deps).await,
        Owner::Absent(reason) => Err(Refusal::unavailable(row.kind, reason)),
    };
    match answered {
        Ok(Answered::Reply(reply)) => {
            vec![reply.into_upstream(replies::trace_id(&command.frame))]
        }
        Ok(Answered::Silent) => Vec::new(),
        Err(refusal) => refuse(command, refusal),
    }
}

/// The refusal, correlated so the caller can act on it.
fn refuse(command: &Command, refusal: Refusal) -> Vec<CoordWorkerUpstream> {
    tracing::warn!(
        kind = command.frame.kind(),
        request_id = %command.request_id,
        %refusal,
        "browser command refused"
    );
    vec![
        Reply::error(&command.request_id, refusal.message())
            .into_upstream(replies::trace_id(&command.frame)),
    ]
}
