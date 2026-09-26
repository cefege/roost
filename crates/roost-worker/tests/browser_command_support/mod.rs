// A support module that cannot say what it expected is not a support
// module. `expect` is denied outside `#[cfg(test)]`, and an
// integration-test module is its own crate, so the exemption is here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The browser-command dispatch surface: which command reaches which
//! capability, what a command that cannot run is answered with, and what each
//! command actually does once it is running.
//!
//! The table test is the one that matters most. It walks every kind the
//! `ClientControlFrame` union can produce and asserts the dispatch owns it, so
//! deleting a row from `OWNERS` fails here rather than turning a command into
//! one nothing answers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use roost_host::{HostPlatform, MapEnv};
use roost_protocol::wire::brand::SessionId;
use roost_worker::browser_commands::attachments::SessionAttachments;
pub use roost_worker::browser_commands::file_commands::LocalFiles;
use roost_worker::browser_commands::search::Searches;
use roost_worker::browser_commands::Deps;

pub mod dispatch;
pub mod fakes;
pub mod frames;

// `every_kind` is the dispatch table's own enumeration; the test binaries reach
// it through this module rather than naming the submodule.
pub use frames::every_kind;
pub mod scratch;

pub use scratch::scratch_root;

pub use dispatch::{command, dispatch, frame_of, only};
pub use fakes::{FakeDiagnostics, FakeGrid, FakePresence, FakeSearch, FakeSessions};


// Re-exported so a test binary can name the collaborators it drives without
// importing four modules to reach them.
pub const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
pub const OTHER_SESSION: &str = "00000000-0000-4000-8000-00000000000b";
pub const EPOCH: &str = "epoch:1";
pub const FINGERPRINT: &str = "ababababababababababababababababababababababababab";
pub const DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

pub fn session(raw: &str) -> SessionId {
    SessionId::try_from(raw.to_owned()).expect("a session id is a uuid")
}
/// The fakes, and what they recorded.
pub struct Harness {
    pub deps: Deps,
    pub root: PathBuf,
    pub sessions: Arc<FakeSessions>,
    pub presence: Arc<FakePresence>,
    pub search: Arc<FakeSearch>,
    pub diagnostics: Arc<FakeDiagnostics>,
}

pub fn harness() -> Harness {
    let root = scratch_root();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch root");
    let environment = Arc::new(MapEnv::new().with("HOME", root.to_string_lossy().as_ref()));
    let files = Arc::new(LocalFiles::new(environment, HostPlatform::Linux));
    let attachments = Arc::new(SessionAttachments::new(
        root.join("attachments"),
        HostPlatform::Linux,
    ));
    let sessions = Arc::new(FakeSessions::default());
    let presence = Arc::new(FakePresence::default());
    let search = Arc::new(FakeSearch::default());
    let diagnostics = Arc::new(FakeDiagnostics::default());
    Harness {
        deps: Deps {
            sessions: sessions.clone(),
            presence: presence.clone(),
            files,
            grid: Arc::new(FakeGrid::default()),
            search: search.clone(),
            searches: Arc::new(Mutex::new(Searches::new())),
            attachments,
            diagnostics: diagnostics.clone(),
        },
        root,
        sessions,
        presence,
        search,
        diagnostics,
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
