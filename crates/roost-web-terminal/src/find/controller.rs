//! The pane-local find controller: the query, the debounce, the reveal, and the
//! cursor the reader steps. `chain` owns the search that is running.
//!
//! What the controller DECIDES is here; what the grid is TOLD is
//! `find::hits::FindPublication` and what the host must DO is a `FindCommand`.
//! That split is what lets the fence, the wrap, the paging cursor and the reveal
//! ORDER be decided in a test with no browser and no RPC.
//!
//! Ported from `apps/web/src/renderer/terminalFindController.ts`; the chain, the
//! reply, the reveal decision and the host contract are the module root.

mod chain;

use roost_client_core::search::FindMatch;

use crate::find::hits::FindPublication;
use crate::find::{
    FindChain, FindCommand, FindHost, FindQueryOptions, RevealDecision, reveal_decision,
};

/// How long typing settles before a query is searched, in milliseconds.
pub const FIND_DEBOUNCE_MS: u64 = 300;

/// The search currently owning the pane.
#[derive(Debug, Clone)]
struct ActiveSearch {
    search_id: String,
    token: u64,
    chain: FindChain,
    /// Matches published under a RESUMED chain, so a slide keeps the page.
    carried: Vec<FindMatch>,
    epoch_retry_budget: u32,
}

/// Bounded find-in-scrollback state for one terminal pane.
///
/// A cancellable page chain accumulates newest-first matches within one grid
/// epoch; deep matches backfill before reveal. The epoch retry budget covers ONE
/// grid renumbering through reveal, and a pane that keeps moving reports failure
/// rather than searching forever.
#[derive(Debug, Clone, Default)]
pub struct TerminalFind {
    session_id: String,
    open: bool,
    query: String,
    case_sensitive: bool,
    regex: bool,
    disposed: bool,
    /// Monotonic token: only the newest search may publish.
    token: u64,
    searches_minted: u64,
    debounce_at_ms: Option<u64>,
    active: Option<ActiveSearch>,
    publication: FindPublication,
}

impl TerminalFind {
    /// A closed find bar. `Default` names no session.
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            ..Self::default()
        }
    }

    /// What the painted grid is told: matches, active index, truncated, failed.
    pub fn publication(&self) -> &FindPublication {
        &self.publication
    }
    /// Whether the find bar is open.
    pub fn is_open(&self) -> bool {
        self.open
    }
    /// The current query.
    pub fn query(&self) -> &str {
        &self.query
    }
    /// Whether the scan is case-sensitive.
    pub fn is_case_sensitive(&self) -> bool {
        self.case_sensitive
    }
    /// Whether the query is a pattern rather than a literal.
    pub fn is_regex(&self) -> bool {
        self.regex
    }

    /// Show the find bar.
    pub fn open_find(&mut self) {
        self.open = true;
    }

    /// Hide the find bar, end the search, and leave the view where it is.
    pub fn close_find(&mut self, host: &mut dyn FindHost) -> Vec<FindCommand> {
        self.open = false;
        self.debounce_at_ms = None;
        let commands = self.stop_active();
        self.query.clear();
        self.publication.clear(host);
        host.end_find_read();
        commands
    }

    /// Replace the query and schedule a search; an empty query clears at once.
    pub fn set_query(
        &mut self,
        next: &str,
        options: FindQueryOptions,
        now_ms: u64,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        self.publication.prefer(options.preferred_match);
        if options.literal {
            self.regex = false;
            self.case_sensitive = options.case_sensitive.unwrap_or(false);
        }
        self.query.clear();
        self.query.push_str(next);
        if next.is_empty() {
            self.debounce_at_ms = None;
            let commands = self.stop_active();
            self.publication.clear(host);
            return commands;
        }
        self.schedule(now_ms)
    }

    /// Flip case sensitivity and re-search; the coordinator coordinate goes with
    /// the flag, since it names a row the new flags will not reproduce.
    pub fn toggle_case_sensitive(&mut self, now_ms: u64) -> Vec<FindCommand> {
        self.case_sensitive = !self.case_sensitive;
        self.reschedule(now_ms)
    }

    /// Flip regex mode and re-search a live query.
    pub fn toggle_regex(&mut self, now_ms: u64) -> Vec<FindCommand> {
        self.regex = !self.regex;
        self.reschedule(now_ms)
    }

    /// Drop the coordinator coordinate and re-arm for a query already live.
    fn reschedule(&mut self, now_ms: u64) -> Vec<FindCommand> {
        self.publication.prefer(None);
        if self.query.is_empty() {
            Vec::new()
        } else {
            self.schedule(now_ms)
        }
    }

    /// Move the active match by `delta`, wrapping at both ends.
    ///
    /// The published window ends at the oldest match a capped page reached, so a
    /// step back past it pages older rows instead of wrapping.
    pub fn step(&mut self, delta: i64, host: &mut dyn FindHost) -> Vec<FindCommand> {
        if delta < 0 && self.publication.index() == 1 && self.publication.has_older_page() {
            return self.extend_older_matches(host);
        }
        match self.publication.step(delta) {
            Some((row, epoch)) => self.reveal(row, &epoch, 1, host),
            None => Vec::new(),
        }
    }

    /// Fire an armed debounce whose time has come.
    pub fn on_debounce(&mut self, now_ms: u64, host: &mut dyn FindHost) -> Vec<FindCommand> {
        let due = self.debounce_at_ms.is_some_and(|at_ms| now_ms >= at_ms);
        if !due {
            return Vec::new();
        }
        self.debounce_at_ms = None;
        self.search_now(1, None, host)
    }

    /// Absorb the answer to a `FetchRow` a reveal was waiting on. `painted` false
    /// means the pager refused or evicted the pull: at most the remaining retry is
    /// spent, and NOTHING scrolls.
    pub fn on_row_painted(
        &mut self,
        row: u32,
        painted: bool,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let Some(pending) = self.publication.take_pending_reveal(row) else {
            return Vec::new();
        };
        let budget = pending.epoch_retry_budget;
        if self.disposed || pending.token != self.token {
            return Vec::new();
        }
        if !painted {
            return if budget > 0 {
                self.search_now(budget - 1, None, host)
            } else {
                Vec::new()
            };
        }
        if host.pane_epoch() != pending.epoch {
            return self.invalidate(budget, host);
        }
        host.reveal_row(pending.row);
        Vec::new()
    }

    /// Retire the controller: nothing it armed may fire afterwards.
    pub fn dispose(&mut self) -> Vec<FindCommand> {
        self.disposed = true;
        let armed = self.debounce_at_ms.take().is_some();
        let mut commands = self.stop_active();
        commands.extend(armed.then_some(FindCommand::CancelDebounce));
        commands
    }

    /// Replace the armed debounce and stop whatever search holds the pane.
    fn schedule(&mut self, now_ms: u64) -> Vec<FindCommand> {
        let at_ms = now_ms + FIND_DEBOUNCE_MS;
        self.debounce_at_ms = Some(at_ms);
        let mut commands = vec![FindCommand::CancelDebounce];
        commands.extend(self.stop_active());
        commands.push(FindCommand::ArmDebounce { at_ms });
        commands
    }

    /// Abort the active search and fence every future answer from it.
    fn stop_active(&mut self) -> Vec<FindCommand> {
        self.token += 1;
        let aborted = self.active.take().map(|active| active.search_id);
        aborted.map_or_else(Vec::new, |id| {
            vec![FindCommand::CancelSearch { search_id: id }]
        })
    }

    /// The active search if it still owns the pane, taken by value.
    fn take_current(&mut self) -> Option<ActiveSearch> {
        let active = self.active.take()?;
        if self.disposed || active.token != self.token {
            return None;
        }
        Some(active)
    }

    /// Reveal one match, fetching its row first when that row is unpainted.
    fn reveal(
        &mut self,
        row: u32,
        epoch: &str,
        budget: u32,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let decision = host
            .anchor()
            .as_ref()
            .map_or(RevealDecision::AlreadyVisible, |a| {
                reveal_decision(a, row, epoch)
            });
        match decision {
            RevealDecision::StaleEpoch => self.invalidate(budget, host),
            RevealDecision::AlreadyVisible => Vec::new(),
            RevealDecision::FetchFirst { row } => {
                self.publication.arm_reveal(row, epoch, self.token, budget);
                vec![FindCommand::FetchRow { row }]
            }
        }
    }

    /// Spend the older-rows cursor a match cap handed back.
    fn extend_older_matches(&mut self, host: &mut dyn FindHost) -> Vec<FindCommand> {
        // Consumed up front so a second keypress cannot start the same page twice.
        let Some(page) = self.publication.take_older_page() else {
            return Vec::new();
        };
        let parked = self.publication.matches().to_vec();
        let commands = self.search_now(1, Some(page), host);
        let slid = self.active.as_ref().map(|live| live.chain.found());
        if let Some(slid) = slid {
            self.publication.prefer_newest_of(slid, &parked);
        }
        commands
    }
}
