//! The pane-local find controller: the query, the debounce, the step cursor and
//! the reveal. `chain` owns the search that is running; `find::hits` owns what
//! the grid is told; the host performs every returned `FindCommand` and answers
//! pages and pulls back here. The terminal pane component holds one per pane.
//! Ports `apps/web/src/renderer/terminalFindController.ts`.

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
    /// Whether this chain slides onto rows a match cap left unscanned.
    resumed: bool,
    /// Matches published before a resumed chain, kept under its page.
    carried: Vec<FindMatch>,
    epoch_retry_budget: u32,
}

/// Bounded find-in-scrollback state for one terminal pane.
///
/// A cancellable page chain accumulates newest-first matches within one grid
/// epoch; deep matches are pulled in before reveal. The epoch retry budget covers
/// ONE grid renumbering through reveal, and a pane that keeps moving reports
/// failure rather than searching forever.
#[derive(Debug, Clone, Default)]
pub struct TerminalFind {
    session_id: String,
    search_id_seed: String,
    open: bool,
    query: String,
    case_sensitive: bool,
    regex: bool,
    disposed: bool,
    /// Monotonic token: only the newest search may publish or scroll.
    token: u64,
    searches_minted: u64,
    debounce_at_ms: Option<u64>,
    active: Option<ActiveSearch>,
    publication: FindPublication,
}

impl TerminalFind {
    /// A closed find bar for one session. `search_id_seed` is a random id (a
    /// UUID) the host mints once per controller: every search id is the seed plus
    /// a counter, so two panes on one session never share a coordinator search.
    pub fn new(session_id: &str, search_id_seed: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            search_id_seed: search_id_seed.to_string(),
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
        tracing::debug!(target: "find", session_id = %self.session_id, "find bar opened");
    }

    /// Hide the find bar and end the search. Dismissal ends the find reading
    /// interval LAST and never moves the view: the park survives as a scroll
    /// park, so live output resumes the way any other park does.
    pub fn close_find(&mut self, host: &mut dyn FindHost) -> Vec<FindCommand> {
        self.open = false;
        let mut commands = self.cancel_debounce();
        commands.extend(self.stop_active());
        self.query.clear();
        self.publication.prefer(None);
        self.publication.clear(host);
        host.end_find_read();
        tracing::info!(target: "find", session_id = %self.session_id, "find bar closed; find reading ended");
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
            let mut commands = self.cancel_debounce();
            commands.extend(self.stop_active());
            self.publication.clear(host);
            return commands;
        }
        self.schedule(now_ms)
    }

    /// Flip case sensitivity and re-search a live query; the coordinator
    /// coordinate goes with the flag, since the new flags may not reproduce it.
    pub fn toggle_case_sensitive(&mut self, now_ms: u64) -> Vec<FindCommand> {
        self.case_sensitive = !self.case_sensitive;
        self.reschedule(now_ms)
    }

    /// Flip regex mode and re-search a live query.
    pub fn toggle_regex(&mut self, now_ms: u64) -> Vec<FindCommand> {
        self.regex = !self.regex;
        self.reschedule(now_ms)
    }

    /// Move the active match by `delta`, wrapping at both ends. The published
    /// window ends at the oldest match a capped page reached, so a step back past
    /// it pages older rows instead of wrapping.
    pub fn step(&mut self, delta: i64, host: &mut dyn FindHost) -> Vec<FindCommand> {
        if self.publication.matches().is_empty() {
            return Vec::new();
        }
        if delta < 0 && self.publication.index() == 1 && self.publication.has_older_page() {
            return self.extend_older_matches(host);
        }
        match self.publication.step(delta, host) {
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

    /// Absorb the answer to an `EnsureRowPainted` a reveal was waiting on.
    /// `painted` false means the pull was refused or evicted: at most the
    /// remaining retry is spent, and NOTHING scrolls.
    pub fn on_row_painted(
        &mut self,
        reveal: u64,
        painted: bool,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let Some(pending) = self.publication.take_pending_reveal(reveal) else {
            return Vec::new();
        };
        let budget = pending.epoch_retry_budget;
        if self.disposed || pending.token != self.token {
            return Vec::new();
        }
        if !painted {
            tracing::info!(target: "find", session_id = %self.session_id, row = pending.row, budget, "find reveal pull refused");
            return if budget > 0 {
                self.search_now(budget - 1, None, host)
            } else {
                Vec::new()
            };
        }
        if host.pane_epoch() != pending.epoch {
            return self.invalidate(budget, host);
        }
        tracing::debug!(target: "find", session_id = %self.session_id, row = pending.row, "find reveal scrolls to row");
        host.reveal_row(pending.row);
        Vec::new()
    }

    /// Retire the controller: nothing it armed may fire afterwards.
    pub fn dispose(&mut self) -> Vec<FindCommand> {
        self.disposed = true;
        self.publication.prefer(None);
        let mut commands = self.cancel_debounce();
        commands.extend(self.stop_active());
        tracing::debug!(target: "find", session_id = %self.session_id, "find controller disposed");
        commands
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

    /// Replace the armed debounce and stop whatever search holds the pane.
    fn schedule(&mut self, now_ms: u64) -> Vec<FindCommand> {
        let mut commands = self.cancel_debounce();
        commands.extend(self.stop_active());
        let at_ms = now_ms + FIND_DEBOUNCE_MS;
        self.debounce_at_ms = Some(at_ms);
        commands.push(FindCommand::ArmDebounce { at_ms });
        commands
    }

    /// Disarm the debounce, telling the host only when one was armed.
    fn cancel_debounce(&mut self) -> Vec<FindCommand> {
        self.debounce_at_ms
            .take()
            .map_or_else(Vec::new, |_| vec![FindCommand::CancelDebounce])
    }

    /// Abort the active search and fence every future answer from it.
    fn stop_active(&mut self) -> Vec<FindCommand> {
        self.token += 1;
        self.active.take().map_or_else(Vec::new, |active| {
            vec![FindCommand::CancelSearch {
                search_id: active.search_id,
            }]
        })
    }

    /// Reveal one match inside its own epoch. A viewport row needs no jump; a
    /// history row is pulled first, and the answer rechecks token and epoch.
    fn reveal(
        &mut self,
        row: u32,
        epoch: &str,
        budget: u32,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let Some(anchor) = host.anchor() else {
            return Vec::new();
        };
        match reveal_decision(&anchor, row, epoch) {
            RevealDecision::StaleEpoch => self.invalidate(budget, host),
            RevealDecision::Viewport => Vec::new(),
            RevealDecision::EnsurePainted { row } => {
                let reveal = self.publication.arm_reveal(row, epoch, self.token, budget);
                vec![FindCommand::EnsureRowPainted { row, reveal }]
            }
        }
    }

    /// Spend the older-rows cursor a match cap handed back, so a needle with
    /// more hits than one page holds stays fully navigable.
    fn extend_older_matches(&mut self, host: &mut dyn FindHost) -> Vec<FindCommand> {
        // Consumed up front so a second keypress cannot start the same page twice.
        let Some(page) = self.publication.take_older_page() else {
            return Vec::new();
        };
        tracing::info!(
            target: "find",
            session_id = %self.session_id,
            before_row = page.before_row,
            pages_used = page.pages_used,
            held_matches = self.publication.matches().len(),
            "find slides onto older rows"
        );
        self.search_now(1, Some(page), host)
    }
}
