//! Browse state, keyed BY MACHINE.
//!
//! **A PATH IS NOT AN IDENTITY.** Two machines can both have `~/src`, and a
//! browse cache keyed by path is a cache that shows machine A's folders under
//! machine B's name — the two listings share a key, the second overwrites the
//! first, and the reader cannot tell whose files they are looking at. Every map
//! in this file is keyed by [`WorkerFp`], and a machine's recents list is
//! filtered by machine BEFORE it is built rather than after.
//!
//! One machine's view: where it is, where it has been, what it is filtered to,
//! and which listing is outstanding. The listing is fenced by a per-machine
//! generation, so a reply for a path the viewer has already left cannot repaint
//! the directory they are now in — the same shape the rest of the core fences
//! its asynchronous answers with.
//!
//! Ported from `apps/web/src/lib/browseHistory.ts` and the state half of
//! `apps/web/src/components/browse/browseDirectoryListing.ts`. Depends on
//! `browse_entries`, `browse_paths` and `roost_protocol::wire`.

use std::collections::BTreeMap;
use roost_protocol::wire::{Session, SessionId, WorkerFp};

use crate::store::browse_entries::BrowseEntry;
use crate::store::browse_paths::{BROWSE_HOME, BrowsePathOps};

/// How many folders the recents list offers.
pub const BROWSE_RECENTS_MAX: usize = 5;

/// A browser-style navigation history over canonical directory paths.
///
/// Entries carry no trailing slash, and `~` is a path like any other here. The
/// history IS the answer to "where is this machine": a second `cwd` field
/// beside it is a second answer, and the two drift the moment a Back is
/// followed by a listing that resolves to something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseHistory {
    entries: Vec<String>,
    cursor: usize,
}

impl BrowseHistory {
    /// A history holding exactly one entry.
    #[must_use]
    pub fn starting_at(path: impl Into<String>) -> Self {
        Self {
            entries: vec![path.into()],
            cursor: 0,
        }
    }

    /// The path at the cursor, or `~` for an empty history.
    #[must_use]
    pub fn current(&self) -> &str {
        self.entries
            .get(self.cursor)
            .map_or(BROWSE_HOME, String::as_str)
    }

    /// Whether Back is available.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    /// Whether Forward is available.
    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// Go back, returning whether the cursor moved.
    pub fn go_back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        self.cursor -= 1;
        true
    }

    /// Go forward, returning whether the cursor moved.
    pub fn go_forward(&mut self) -> bool {
        if !self.can_go_forward() {
            return false;
        }
        self.cursor += 1;
        true
    }

    /// The whole stack, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Record a move to `path`, truncating anything ahead of the cursor.
    ///
    /// A move to the path already at the cursor records nothing: re-entering the
    /// directory you are standing in is not navigation, and recording it makes
    /// Back land where you already were.
    /// `pub(crate)` because the container in `browse_state.rs` is the only
    /// writer, and the line cap put it in a sibling file. `pub` here would let a
    /// host grow a history without the listing generation moving with it.
    pub(crate) fn push(&mut self, path: String) -> bool {
        if self.current() == path {
            return false;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(path);
        self.cursor = self.entries.len() - 1;
        true
    }
}

/// One directory listing request, fenced by the generation that issued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseListingRequest {
    /// Which machine to ask.
    pub worker_fp: WorkerFp,
    /// Which path on it.
    pub path: String,
    /// The per-machine generation this request belongs to.
    pub generation: u64,
}

/// What one machine's listing is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowseListing {
    /// Nothing has been asked yet.
    #[default]
    Idle,
    /// A listing is outstanding.
    Loading {
        /// The generation that issued it.
        generation: u64,
        /// The path it was issued for.
        path: String,
    },
    /// A listing stands.
    Ready {
        /// The generation that produced it.
        generation: u64,
        /// The path that was asked for.
        path: String,
        /// What the machine resolved that path to.
        resolved_path: String,
        /// The rows it returned.
        entries: Vec<BrowseEntry>,
    },
    /// A listing failed and no listing stands.
    Failed {
        /// The generation that failed.
        generation: u64,
        /// The path that failed.
        path: String,
        /// Reader-facing copy. The machine's own words stay in the host's log.
        message: String,
    },
}

impl BrowseListing {
    /// The rows that stand, or empty.
    #[must_use]
    pub fn entries(&self) -> &[BrowseEntry] {
        match self {
            Self::Ready { entries, .. } => entries,
            _ => &[],
        }
    }

    /// What the machine resolved the asked path to, when a listing stands.
    #[must_use]
    pub fn resolved_path(&self) -> Option<&str> {
        match self {
            Self::Ready { resolved_path, .. } => Some(resolved_path.as_str()),
            _ => None,
        }
    }

    /// Whether a listing is outstanding.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }
}

/// One machine's browse view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineBrowse {
    /// Which machine this view belongs to. Present on the view rather than only
    /// in its map key so a copied or moved view cannot lose its owner.
    pub(crate) worker_fp: WorkerFp,
    /// The directory this machine's browser opened on.
    pub(crate) home: String,
    /// Where it is, and where it has been.
    pub(crate) history: BrowseHistory,
    /// The in-list filter.
    pub(crate) filter: String,
    /// Whether the filter box is showing.
    pub(crate) filter_open: bool,
    /// The outstanding or standing listing.
    pub(crate) listing: BrowseListing,
    /// The next listing generation. Per machine, because two machines listing
    /// at once must not fence each other.
    ///
    /// `pub(crate)` and not `pub`: the container in `browse_state.rs` is this
    /// view's only writer, and the line cap put it in a sibling file. `pub`
    /// would let any host write a machine's generation directly, which is the
    /// one write that can unfence every listing answer this crate sends.
    pub(crate) generation: u64,
}

impl MachineBrowse {
    /// The machine this view belongs to.
    #[must_use]
    pub fn worker_fp(&self) -> &WorkerFp {
        &self.worker_fp
    }

    /// Where this machine's browser opened.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Where it is now.
    #[must_use]
    pub fn cwd(&self) -> &str {
        self.history.current()
    }

    /// The navigation history.
    #[must_use]
    pub fn history(&self) -> &BrowseHistory {
        &self.history
    }

    /// The in-list filter.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Whether the filter box is showing.
    #[must_use]
    pub fn is_filter_open(&self) -> bool {
        self.filter_open
    }

    /// The outstanding or standing listing.
    #[must_use]
    pub fn listing(&self) -> &BrowseListing {
        &self.listing
    }

    /// Whether Up is available: there is somewhere above this directory.
    #[must_use]
    pub fn can_go_up(&self, paths: &dyn BrowsePathOps) -> bool {
        paths.parent(self.cwd()) != self.cwd()
    }

    /// The newest session directories THIS machine has, most recent first.
    ///
    /// Filtered by machine, and the filter is the first thing that happens. The
    /// paths on two machines are the same strings, so a recents list built from
    /// every session this browser knows is machine A's folders under machine B's
    /// name, with nothing downstream able to tell them apart.
    #[must_use]
    pub fn recents(&self, sessions: &BTreeMap<SessionId, Session>) -> Vec<String> {
        let mut owned: Vec<&Session> = sessions
            .values()
            .filter(|session| session.worker_fp == self.worker_fp)
            .collect();
        owned.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut seen: Vec<String> = Vec::new();
        for session in owned {
            if session.cwd.is_empty() || seen.iter().any(|path| path == &session.cwd) {
                continue;
            }
            seen.push(session.cwd.clone());
            if seen.len() >= BROWSE_RECENTS_MAX {
                break;
            }
        }
        seen
    }
}
