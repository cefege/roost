//! The container: one browse view per machine, and every move a viewer can make
//! to one of them.
//!
//! Split from `browse_machine.rs` only for the line cap — the two files are one
//! type's two halves, and a reader who wants "where is this machine" wants the
//! first and a reader who wants "move it" wants this one.

use std::collections::BTreeMap;

use roost_protocol::wire::WorkerFp;

use crate::store::browse_entries::BrowseEntry;
use crate::store::browse_machine::{
    BrowseHistory, BrowseListing, BrowseListingRequest, MachineBrowse,
};
use crate::store::browse_paths::{BROWSE_HOME, BrowsePathOps};

pub mod intent;

/// Every machine's browse view, keyed by machine fingerprint.
///
/// One container rather than a map a host owns, for the reason
/// `store::root.rs` gives: "what does leaving this machine clear?" then has one
/// answer, written once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowseState {
    machines: BTreeMap<WorkerFp, MachineBrowse>,
}

impl BrowseState {
    /// A state that has browsed nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a machine's browser on `home`, or return the one already open.
    ///
    /// `home` is used only the FIRST time: a machine's browser that reset to `~`
    /// because its route was re-entered would lose the directory the viewer was
    /// standing in.
    pub fn open(&mut self, worker_fp: &WorkerFp, home: Option<&str>) -> &MachineBrowse {
        self.machines.entry(worker_fp.clone()).or_insert_with(|| {
            let home = home.unwrap_or(BROWSE_HOME).to_owned();
            MachineBrowse {
                worker_fp: worker_fp.clone(),
                history: BrowseHistory::starting_at(home.clone()),
                home,
                filter: String::new(),
                filter_open: false,
                listing: BrowseListing::Idle,
                generation: 0,
            }
        })
    }

    /// One machine's view, if it is open.
    #[must_use]
    pub fn get(&self, worker_fp: &WorkerFp) -> Option<&MachineBrowse> {
        self.machines.get(worker_fp)
    }

    /// One machine's view, mutable, if it is open.
    pub fn get_mut(&mut self, worker_fp: &WorkerFp) -> Option<&mut MachineBrowse> {
        self.machines.get_mut(worker_fp)
    }

    /// How many machines have a browser open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.machines.len()
    }

    /// Whether no machine has a browser open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.machines.is_empty()
    }

    /// The open machines, in fingerprint order.
    pub fn machines(&self) -> impl Iterator<Item = (&WorkerFp, &MachineBrowse)> {
        self.machines.iter()
    }

    /// Forget a machine's browser, for a machine that has left the registry.
    pub fn forget(&mut self, worker_fp: &WorkerFp) -> bool {
        self.machines.remove(worker_fp).is_some()
    }

    /// Navigate one machine to a path, returning whether anything moved.
    pub fn set_cwd(&mut self, worker_fp: &WorkerFp, path: &str) -> bool {
        self.get_mut(worker_fp)
            .is_some_and(|machine| machine.history.push(path.to_owned()))
    }

    /// Navigate one machine into a listed child, through the host's codec.
    pub fn open_child(
        &mut self,
        worker_fp: &WorkerFp,
        name: &str,
        paths: &dyn BrowsePathOps,
    ) -> bool {
        let Some(machine) = self.get_mut(worker_fp) else {
            return false;
        };
        let child = paths.child(machine.cwd(), name);
        machine.history.push(child)
    }

    /// Navigate one machine up, through the host's codec.
    pub fn go_up(&mut self, worker_fp: &WorkerFp, paths: &dyn BrowsePathOps) -> bool {
        let Some(machine) = self.get_mut(worker_fp) else {
            return false;
        };
        let parent = paths.parent(machine.cwd());
        if parent == machine.cwd() {
            return false;
        }
        machine.history.push(parent)
    }

    /// Go back on one machine, returning whether the cursor moved.
    pub fn go_back(&mut self, worker_fp: &WorkerFp) -> bool {
        self.get_mut(worker_fp)
            .is_some_and(|machine| machine.history.go_back())
    }

    /// Go forward on one machine, returning whether the cursor moved.
    pub fn go_forward(&mut self, worker_fp: &WorkerFp) -> bool {
        self.get_mut(worker_fp)
            .is_some_and(|machine| machine.history.go_forward())
    }

    /// Filter one machine's list.
    pub fn set_filter(&mut self, worker_fp: &WorkerFp, filter: &str) {
        if let Some(machine) = self.get_mut(worker_fp) {
            machine.filter = filter.to_owned();
        }
    }

    /// Show or hide one machine's filter box.
    pub fn set_filter_open(&mut self, worker_fp: &WorkerFp, open: bool) {
        if let Some(machine) = self.get_mut(worker_fp) {
            machine.filter_open = open;
        }
    }

    /// Hide one machine's filter box and clear its text.
    ///
    /// One call rather than the pair, because a hidden filter that still hides
    /// entries is the "where did my folders go" report and a caller that writes
    /// the two separately has a paint in between.
    pub fn close_filter(&mut self, worker_fp: &WorkerFp) {
        if let Some(machine) = self.get_mut(worker_fp) {
            machine.filter_open = false;
            machine.filter.clear();
        }
    }

    /// Ask one machine for its current directory, returning the request to
    /// issue.
    ///
    /// The generation moves on EVERY request, including a reload of the same
    /// path: a retry is a new attempt, and the reply to the abandoned one is
    /// still in flight.
    pub fn begin_listing(&mut self, worker_fp: &WorkerFp) -> Option<BrowseListingRequest> {
        let machine = self.get_mut(worker_fp)?;
        machine.generation = machine.generation.wrapping_add(1);
        let path = machine.cwd().to_owned();
        machine.listing = BrowseListing::Loading {
            generation: machine.generation,
            path: path.clone(),
        };
        Some(BrowseListingRequest {
            worker_fp: worker_fp.clone(),
            path,
            generation: machine.generation,
        })
    }

    /// Fold one answered listing in, returning whether it published.
    ///
    /// A reply for a generation this machine has moved past is dropped whole: it
    /// lists a directory the viewer has left, and its rows would replace the
    /// rows of the one they are standing in.
    pub fn apply_listing(
        &mut self,
        request: &BrowseListingRequest,
        resolved_path: String,
        entries: Vec<BrowseEntry>,
    ) -> bool {
        let Some(machine) = self.get_mut(&request.worker_fp) else {
            return false;
        };
        if machine.generation != request.generation || machine.cwd() != request.path {
            return false;
        }
        machine.listing = BrowseListing::Ready {
            generation: request.generation,
            path: request.path.clone(),
            resolved_path,
            entries,
        };
        true
    }

    /// Fold one failed listing in, returning whether it published.
    pub fn fail_listing(&mut self, request: &BrowseListingRequest, message: String) -> bool {
        let Some(machine) = self.get_mut(&request.worker_fp) else {
            return false;
        };
        if machine.generation != request.generation || machine.cwd() != request.path {
            return false;
        }
        machine.listing = BrowseListing::Failed {
            generation: request.generation,
            path: request.path.clone(),
            message,
        };
        true
    }
}
