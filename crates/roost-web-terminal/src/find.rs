//! Pane-local find in terminal scrollback: what one search answers with, the
//! bounded page chain that walks those answers, and the host conversation that
//! paints them. `hits` owns what the grid is told; `controller` owns the query
//! and the cursor; `intent` is the one-shot handoff from global search.
//!
//! The match types and the three page guards are `roost_client_core::search`,
//! reused rather than restated, and `roost_protocol::terminal_search` owns every
//! bound. Ported from `apps/web/src/client/search/terminalFindPageChain.ts` and
//! `terminalFindPaging.ts`.

pub mod controller;
pub mod hits;
pub mod intent;

pub use controller::{FIND_DEBOUNCE_MS, TerminalFind};
pub use hits::{
    ActiveHit, FindCommand, FindHost, FindPublication, FindQueryOptions, HitRows, PendingReveal,
    PreferredMatch, RevealDecision, active_hit, hit_rows, matches_belong_to_pane,
    preferred_find_index, reveal_decision,
};
pub use intent::{FindIntentRegistry, FindIntentSink, TerminalFindIntent, TerminalFindIntentOptions};

use crate::presentation::BackfillAnchor;
use roost_client_core::search::{
    FindMatch, RawMatch, SearchPage, continuation_is_valid, fence_page, window_is_valid,
};
use roost_protocol::terminal_search::{
    TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_PAGES, TERMINAL_SEARCH_MAX_ROWS,
};

/// Why a coordinator stopped scanning a page.
///
/// A local copy of the wire enum, because the crate that decodes it
/// (`roost-coord`) is a server and the client needs the same five words to read
/// a page. It is a stop REASON, not a match: every match on a page is
/// `roost_client_core::search::RawMatch` and nothing here restates one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStop {
    /// Every row was scanned.
    Complete,
    /// The row ceiling stopped the scan; older rows remain.
    RowLimit,
    /// The match ceiling stopped the scan; older rows were never read.
    MatchLimit,
    /// The page deadline stopped the scan.
    Deadline,
    /// The grid renumbered under the scan, so every row it read is stale.
    EpochChanged,
}

/// One page, as the coordinator answered it: the matches, the window they came
/// from, the grid numbering, and why the scan stopped.
///
/// The window and the matches travel side by side rather than nested, because
/// the window is what the guards judge and a match read out of an unvalidated
/// window is a row that points at the wrong line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchReply {
    /// The rows the scan found, unfenced.
    pub matches: Vec<RawMatch>,
    /// The window the scan read and where it would resume.
    pub page: SearchPage,
    /// The grid numbering those rows belong to.
    pub grid_epoch: String,
    /// Why the scan stopped.
    pub stop: SearchStop,
}

/// One scrollback-search request, exactly as the page chain issues it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindRequest {
    /// The session whose history is searched.
    pub session_id: String,
    /// The correlation id every page of one search echoes, which is what lets a
    /// cancel name the search it is cancelling.
    pub search_id: String,
    /// The grid numbering to pin; empty until the first page names one.
    pub grid_epoch: String,
    /// The literal the reader typed, or the pattern they toggled regex on.
    pub query: String,
    /// Whether the scan is case-sensitive.
    pub case_sensitive: bool,
    /// Whether `query` is a pattern rather than a literal.
    pub regex: bool,
    /// The row the next page starts at; absent on the first page.
    pub before_row: Option<u32>,
    /// The most rows one page may scan.
    pub max_rows: u32,
    /// The most matches one page may return.
    pub max_matches: u32,
}

/// The page of older rows a match cap left unscanned, plus the page budget
/// already spent — the ceiling spans the query, not one chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlderMatchPage {
    /// The grid numbering the cursor was taken on.
    pub epoch: String,
    /// The row the older page starts at.
    pub before_row: u32,
    /// How many pages the query has already spent.
    pub pages_used: u32,
}

/// What one page chain concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainOutcome {
    /// A newer search owns the pane, so this chain ends without publishing.
    Abandoned,
    /// The grid renumbered mid-chain, so every row it read is stale.
    EpochChanged,
    /// The chain read what it could.
    Matches {
        /// Every match the chain found, in scan order.
        matches: Vec<FindMatch>,
        /// Older matches are known to exist beyond `matches`.
        truncated: bool,
        /// The chain stopped on something the reader must be told about.
        failed: bool,
        /// The cursor onto those older matches, absent when none is usable.
        older: Option<OlderMatchPage>,
    },
}

/// What the chain wants next: a request to issue, or the outcome to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainStep {
    /// Issue this page request, then hand the answer back to `absorb`.
    Issue(FindRequest),
    /// The chain is over.
    Finish(ChainOutcome),
}

/// One bounded page chain over the coordinator scrollback-search RPC.
///
/// The chain holds no state past its own return, and every step re-reads the
/// pane's live grid numbering: a chain never publishes across a renumbering, so
/// the epoch is tested on the way INTO a request as well as on the way out of
/// an answer.
#[derive(Debug, Clone)]
pub struct FindChain {
    session_id: String,
    search_id: String,
    query: String,
    case_sensitive: bool,
    regex: bool,
    initial_epoch: String,
    requested_epoch: String,
    before_row: Option<u32>,
    pages: u32,
    found: Vec<FindMatch>,
}

impl FindChain {
    /// A chain for one query, starting at the newest row.
    ///
    /// `flags` is `(case_sensitive, regex)`, taken as a pair because the two are
    /// one setting the reader toggles together and never one value's meaning.
    ///
    /// `resume` slides the window onto older rows: it carries the cursor AND the
    /// page budget already spent, because the ceiling bounds the query rather
    /// than one run of it.
    pub fn new(
        session_id: &str,
        search_id: &str,
        query: &str,
        flags: (bool, bool),
        epoch: &str,
        resume: Option<&OlderMatchPage>,
    ) -> Self {
        let (requested_epoch, before_row, pages) = match resume {
            Some(page) => (page.epoch.clone(), Some(page.before_row), page.pages_used),
            None => (epoch.to_string(), None, 0),
        };
        Self {
            session_id: session_id.to_string(),
            search_id: search_id.to_string(),
            query: query.to_string(),
            case_sensitive: flags.0,
            regex: flags.1,
            initial_epoch: epoch.to_string(),
            requested_epoch,
            before_row,
            pages,
            found: Vec::new(),
        }
    }

    /// The matches this chain has read, in scan order.
    pub fn found(&self) -> &[FindMatch] {
        &self.found
    }

    /// What the chain wants next, before a request goes out.
    pub fn next_request(&self, pane_epoch: &str, current: bool) -> ChainStep {
        if !current {
            return ChainStep::Finish(ChainOutcome::Abandoned);
        }
        if self.pages >= TERMINAL_SEARCH_MAX_PAGES {
            return ChainStep::Finish(self.failed_partial());
        }
        if !self.pane_accepts_epoch(pane_epoch) {
            return ChainStep::Finish(ChainOutcome::EpochChanged);
        }
        let max_matches = self.remaining_matches();
        if max_matches == 0 {
            return ChainStep::Finish(ChainOutcome::Matches {
                matches: self.found.clone(),
                truncated: true,
                failed: false,
                older: None,
            });
        }
        ChainStep::Issue(FindRequest {
            session_id: self.session_id.clone(),
            search_id: self.search_id.clone(),
            grid_epoch: self.requested_epoch.clone(),
            query: self.query.clone(),
            case_sensitive: self.case_sensitive,
            regex: self.regex,
            before_row: self.before_row,
            max_rows: TERMINAL_SEARCH_MAX_ROWS,
            max_matches,
        })
    }

    /// Absorb one answer and decide what the chain wants next.
    ///
    /// The page budget counts an answer before it is judged, because a page the
    /// chain refused still cost a round trip and a budget that did not count it
    /// would let a malformed worker answer be fetched forever.
    pub fn absorb(&mut self, reply: &SearchReply, pane_epoch: &str, current: bool) -> ChainStep {
        self.pages += 1;
        if !current {
            return ChainStep::Finish(ChainOutcome::Abandoned);
        }
        if reply.stop == SearchStop::EpochChanged {
            return ChainStep::Finish(ChainOutcome::EpochChanged);
        }
        if self.requested_epoch.is_empty() {
            if reply.grid_epoch.is_empty() {
                return ChainStep::Finish(self.failed_partial());
            }
            self.requested_epoch.clone_from(&reply.grid_epoch);
        } else if reply.grid_epoch != self.requested_epoch {
            return ChainStep::Finish(ChainOutcome::EpochChanged);
        }
        if !self.pane_accepts_epoch(pane_epoch) {
            return ChainStep::Finish(ChainOutcome::EpochChanged);
        }
        if !window_is_valid(&reply.page, &reply.matches, self.before_row) {
            return ChainStep::Finish(self.failed_partial());
        }
        if reply.matches.len() as u32 > self.remaining_matches() {
            return ChainStep::Finish(self.failed_partial());
        }
        let Some(page_matches) =
            fence_page(&reply.matches, &reply.page, self.before_row, &self.requested_epoch)
        else {
            return ChainStep::Finish(self.failed_partial());
        };
        self.found.extend(page_matches);

        let may_continue = matches!(reply.stop, SearchStop::RowLimit | SearchStop::MatchLimit);
        if !may_continue && reply.page.next_before_row.is_some() {
            return ChainStep::Finish(self.failed_partial());
        }
        match reply.stop {
            SearchStop::Complete => ChainStep::Finish(self.read_all()),
            SearchStop::MatchLimit => ChainStep::Finish(ChainOutcome::Matches {
                matches: self.found.clone(),
                truncated: true,
                failed: false,
                older: self.older_match_page(reply),
            }),
            SearchStop::Deadline => ChainStep::Finish(ChainOutcome::Matches {
                matches: self.found.clone(),
                truncated: true,
                failed: true,
                older: None,
            }),
            SearchStop::RowLimit => {
                if !continuation_is_valid(&reply.page, self.before_row) {
                    return ChainStep::Finish(self.failed_partial());
                }
                if self.found.len() as u32 >= TERMINAL_SEARCH_MAX_MATCHES {
                    return ChainStep::Finish(ChainOutcome::Matches {
                        matches: self.found.clone(),
                        truncated: true,
                        failed: false,
                        older: self.older_match_page(reply),
                    });
                }
                self.before_row = reply.page.next_before_row;
                self.next_request(pane_epoch, current)
            }
            SearchStop::EpochChanged => ChainStep::Finish(ChainOutcome::EpochChanged),
        }
    }

    /// The outcome published when a page is refused or a request never lands.
    pub fn failed_partial(&self) -> ChainOutcome {
        ChainOutcome::Matches {
            matches: self.found.clone(),
            truncated: false,
            failed: true,
            older: None,
        }
    }

    /// Whether a pane numbering is one this chain may read and publish under.
    ///
    /// The empty-epoch case is the FIRST page: a pane that has not painted a
    /// frame names nothing, so the chain may adopt whatever epoch the first
    /// answer carries. A pane that has numbered itself must match exactly.
    pub fn pane_accepts_epoch(&self, pane_epoch: &str) -> bool {
        pane_epoch == self.requested_epoch
            || (self.initial_epoch.is_empty()
                && pane_epoch.is_empty()
                && !self.requested_epoch.is_empty())
    }

    /// A chain that read every row it asked for, and found nothing older.
    fn read_all(&self) -> ChainOutcome {
        ChainOutcome::Matches {
            matches: self.found.clone(),
            truncated: false,
            failed: false,
            older: None,
        }
    }

    /// A cursor is only worth keeping when it is valid, strictly older, and the
    /// query still has page budget left to spend on it.
    fn older_match_page(&self, reply: &SearchReply) -> Option<OlderMatchPage> {
        if self.pages >= TERMINAL_SEARCH_MAX_PAGES {
            return None;
        }
        let before_row = reply.page.next_before_row?;
        continuation_is_valid(&reply.page, self.before_row).then_some(OlderMatchPage {
            epoch: self.requested_epoch.clone(),
            before_row,
            pages_used: self.pages,
        })
    }

    fn remaining_matches(&self) -> u32 {
        TERMINAL_SEARCH_MAX_MATCHES.saturating_sub(self.found.len() as u32)
    }
}

/// What must happen before one match can be revealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealDecision {
    /// The pane renumbered under this match, so the controller re-fences.
    StaleEpoch,
    /// The match is already in the live grid: no fetch, no scroll.
    AlreadyVisible,
    /// The match is history, so its row is fetched FIRST and only a painted row
    /// may then be revealed.
    FetchFirst {
        /// The row the match sits in.
        row: u32,
    },
}

/// What one match needs from the pane before it can be revealed.
#[must_use]
pub fn reveal_decision(anchor: &BackfillAnchor, row: u32, epoch: &str) -> RevealDecision {
    if anchor.grid_epoch != epoch {
        return RevealDecision::StaleEpoch;
    }
    if row >= anchor.total {
        return RevealDecision::AlreadyVisible;
    }
    RevealDecision::FetchFirst { row }
}
