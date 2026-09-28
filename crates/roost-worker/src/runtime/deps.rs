//! The one production [`Deps`]: every capability a browser command routes to,
//! built from the values the worker already owns. `runtime::serve` constructs
//! one and hands it to the command pump; nothing else in the worker builds a
//! `Deps`.
//!
//! IT LIVES IN `runtime/` AND NOT IN `browser_commands/`, and that is a
//! dependency-direction fact rather than a preference. `browser_commands` names
//! its capabilities as traits precisely so it does not have to know the session
//! layer, the search engine or the recorder; a module that BUILT them would
//! have to depend on all three, and `session` and `capture` would then depend
//! on the dispatcher that routes to them. `runtime` already depends on both
//! directions, so it is the only place the wiring fits.
//!
//! THE ACCEPTANCE TEST FOR THIS FILE IS A GREP: every trait named in [`Deps`]
//! must have an implementation somewhere under `src/`, and a trait with only a
//! test fake is a capability that answers in tests and refuses in production.
//! `browser_commands::mod.rs` says the same thing from the other side, and both
//! are there because the failure is silent — a dispatcher with a missing
//! capability still compiles, and a browser still waits.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_host::HostPlatform;
use roost_host::env::ProcessEnv;

use crate::attachments::store_paths::AttachmentBase;
use crate::browser_commands::Deps;
use crate::browser_commands::attachments::SessionAttachments;
use crate::browser_commands::file_commands::LocalFiles;
use crate::browser_commands::presence::WorkerPresence;
use crate::browser_commands::search;
use crate::browser_commands::search_scan::GridScanner;
use crate::capture::CaptureRecorder;
use crate::session::lifecycle::{SessionManager, SessionTable};
use crate::session::retained_grid::SessionGrid;

/// Everything the builder needs that is NOT a capability.
///
/// A struct rather than six arguments: this is what makes "which of these did
/// the composition root forget to pass" a question the compiler asks.
#[derive(Debug)]
pub struct WorkerCapabilities {
    /// The sessions every session-shaped capability reads.
    pub sessions: Arc<SessionTable>,
    /// The manager that owns those sessions' lifecycle. It is both the table's
    /// owner and the `SessionLifecycle` capability, so it appears once here
    /// rather than as two arguments that could disagree.
    pub manager: Arc<SessionManager>,
    /// The root every session's attachment directory hangs from.
    pub attachment_root: PathBuf,
    /// The one terminal incident recorder the session data path feeds; the
    /// diagnostics command reaches it rather than a second one built here.
    pub capture: Arc<CaptureRecorder>,
    /// Which host this is, for the two capabilities that branch on it.
    pub platform: HostPlatform,
    /// The admission ledger, ALREADY OPENED by the caller.
    ///
    /// IT IS TAKEN RATHER THAN BUILT, and that is the whole fix. This used to
    /// construct `Arc::new(Mutex::new(Searches::default()))` here, which made
    /// the ledger once per CALL to `into_deps` while `deps.rs`'s own header
    /// and `session_stack.rs`'s both documented it as "FRESH HERE, ONCE PER
    /// PROCESS". Nothing enforced that; only one call site upheld it, and
    /// `SessionStack::deps` is `pub`.
    pub searches: Arc<Mutex<search::Searches>>,
}

impl WorkerCapabilities {
    /// Build the one `Deps` these inputs imply.
    ///
    /// The search ledger is NOT BUILT HERE. It is opened once by the owner of
    /// the session layer and passed in, because it is the admission decision
    /// for every search on this worker and a second ledger would let two
    /// callers each believe they hold all
    /// [`MAX_ACTIVE_SEARCHES`](crate::browser_commands::search::MAX_ACTIVE_SEARCHES)
    /// slots — which is the bound the ledger exists to enforce. A ledger built
    /// per call makes that bound per call, and the type is what stops it: two
    /// `Deps` from one owner share one `Arc`, so the second cannot admit past
    /// what the first took.
    pub fn into_deps(self) -> Deps {
        let Self {
            sessions,
            manager,
            attachment_root,
            capture,
            platform,
            searches,
        } = self;
        Deps {
            sessions: manager,
            presence: Arc::new(WorkerPresence),
            files: Arc::new(LocalFiles::new(Arc::new(ProcessEnv::new()), platform)),
            grid: Arc::new(SessionGrid::new(Arc::clone(&sessions))),
            search: Arc::new(GridScanner::new(Arc::clone(&sessions))),
            searches: Arc::clone(&searches),
            attachments: Arc::new(SessionAttachments::new(AttachmentBase::new(attachment_root))),
            diagnostics: capture,
        }
    }
}
