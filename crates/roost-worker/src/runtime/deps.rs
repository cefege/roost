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

use crate::browser_commands::Deps;
use crate::browser_commands::attachments::SessionAttachments;
use crate::browser_commands::file_commands::LocalFiles;
use crate::browser_commands::presence::WorkerPresence;
use crate::browser_commands::search::Searches;
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
    /// The worker's log directory, which is where a capture bundle is written.
    pub log_dir: PathBuf,
    /// This worker's fingerprint, as a diagnostic report records it.
    pub worker_fp: String,
    /// Which host this is, for the two capabilities that branch on it.
    pub platform: HostPlatform,
}

impl WorkerCapabilities {
    /// Build the one `Deps` these inputs imply.
    ///
    /// The search ledger is FRESH HERE, ONCE PER PROCESS, because it is the
    /// admission decision for every search on this worker: a second ledger
    /// would let two callers each believe they hold all eight slots, which is
    /// the bound the ledger exists to enforce.
    pub fn into_deps(self) -> Deps {
        let Self {
            sessions,
            manager,
            attachment_root,
            log_dir,
            worker_fp,
            platform,
        } = self;
        Deps {
            sessions: manager,
            presence: Arc::new(WorkerPresence),
            files: Arc::new(LocalFiles::new(Arc::new(ProcessEnv::new()), platform)),
            grid: Arc::new(SessionGrid::new(Arc::clone(&sessions))),
            search: Arc::new(GridScanner::new(Arc::clone(&sessions))),
            searches: Arc::new(Mutex::new(Searches::default())),
            attachments: Arc::new(SessionAttachments::new(attachment_root, platform)),
            diagnostics: Arc::new(CaptureRecorder::new(sessions, log_dir, worker_fp)),
        }
    }
}
