//! The search that is running: starting one chain, fencing its answers to the
//! search id and token that own the pane, and publishing what it concluded. A
//! file split of `TerminalFind`, whose state `controller` owns. Ports
//! `searchNow`, `installResult`, `retryAfterEpochChange` and `invalidate` of
//! `apps/web/src/renderer/terminalFindController.ts`.

use crate::find::controller::{ActiveSearch, TerminalFind};
use crate::find::{
    ChainOutcome, ChainStep, FindChain, FindCommand, FindHost, OlderMatchPage, SearchReply,
};

impl TerminalFind {
    /// Absorb one page answered for `search_id`. An answer for any search but
    /// the one owning the pane is dropped: it may neither publish nor continue.
    pub fn on_page(
        &mut self,
        search_id: &str,
        reply: &SearchReply,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let Some(mut active) = self.take_current(search_id) else {
            return Vec::new();
        };
        match active.chain.absorb(reply, &host.pane_epoch(), true) {
            ChainStep::Issue(request) => {
                self.active = Some(active);
                vec![FindCommand::Search(request)]
            }
            ChainStep::Finish(outcome) => self.install_chain(active, outcome, host),
        }
    }

    /// Absorb a page request for `search_id` that failed. A pane that has
    /// renumbered away from the epoch a page was PINNED to is epoch-changed,
    /// and only that is retried; a first page pinned to nothing is a plain
    /// failure, because re-issuing it asks the identical question.
    pub fn on_search_error(
        &mut self,
        search_id: &str,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let Some(active) = self.take_current(search_id) else {
            return Vec::new();
        };
        let outcome = if active.chain.pane_accepts_epoch(&host.pane_epoch()) {
            active.chain.failed_partial()
        } else {
            ChainOutcome::EpochChanged
        };
        self.install_chain(active, outcome, host)
    }

    /// Start one chain for the current query. `resume` slides onto rows a match
    /// cap left unscanned, keeping the matches already published under it.
    pub(super) fn search_now(
        &mut self,
        epoch_retry_budget: u32,
        resume: Option<OlderMatchPage>,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let carried = match resume {
            Some(_) => self.publication.matches().to_vec(),
            None => Vec::new(),
        };
        let mut commands = self.stop_active();
        if self.query.is_empty() {
            self.publication.clear(host);
            return commands;
        }
        self.searches_minted += 1;
        let search_id = format!("{}-{}", self.search_id_seed, self.searches_minted);
        // A fresh chain pins NO epoch and lets its first answer name one: the
        // worker refuses a request addressed to a grid it has already
        // renumbered BEFORE it scans, so seeding the pane's numbering spends
        // the whole epoch retry on a pane a frame behind and returns no page
        // at all. A resumed chain keeps the epoch its cursor was taken on,
        // because that page continues a read the pane already proved.
        let chain = FindChain::new(
            &self.session_id,
            &search_id,
            &self.query,
            (self.case_sensitive, self.regex),
            resume.as_ref(),
        );
        let first = chain.next_request(&host.pane_epoch(), !self.disposed);
        tracing::info!(
            target: "find",
            session_id = %self.session_id,
            search_id = %search_id,
            resumed = resume.is_some(),
            epoch_retry_budget,
            "find search started"
        );
        let active = ActiveSearch {
            search_id,
            token: self.token,
            chain,
            resumed: resume.is_some(),
            carried,
            epoch_retry_budget,
        };
        match first {
            ChainStep::Issue(request) => {
                self.active = Some(active);
                commands.push(FindCommand::Search(request));
            }
            ChainStep::Finish(outcome) => {
                commands.extend(self.install_chain(active, outcome, host))
            }
        }
        commands
    }

    /// The active search if `search_id` names it and it still owns the pane,
    /// taken by value; a mismatched id leaves the active search in place.
    fn take_current(&mut self, search_id: &str) -> Option<ActiveSearch> {
        if self.active.as_ref()?.search_id != search_id {
            return None;
        }
        let active = self.active.take()?;
        if self.disposed || active.token != self.token {
            return None;
        }
        Some(active)
    }

    /// Publish what one chain concluded, with the chain already retired.
    fn install_chain(
        &mut self,
        active: ActiveSearch,
        outcome: ChainOutcome,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let (matches, truncated, failed, older) = match outcome {
            ChainOutcome::Abandoned => return Vec::new(),
            ChainOutcome::EpochChanged => {
                tracing::info!(target: "find", session_id = %self.session_id, search_id = %active.search_id, pane_epoch = %host.pane_epoch(), "find search saw the grid renumber");
                return self.invalidate(active.epoch_retry_budget, host);
            }
            ChainOutcome::Matches {
                matches,
                truncated,
                failed,
                older,
            } => (matches, truncated, failed, older),
        };
        if active.resumed {
            self.publication.prefer_newest_of(&matches, &active.carried);
        }
        let mut list = active.carried;
        list.extend(matches);
        // A failure that found nothing is a plain failure, not a truncated result.
        let partial = truncated || (failed && !list.is_empty());
        // Read before the conclusion is logged: the epoch the matches publish
        // under is what says whether they belong to the painted grid.
        let pane_epoch = host.pane_epoch();

        tracing::info!(
            target: "find",
            session_id = %self.session_id,
            search_id = %active.search_id,
            matches = list.len(),
            truncated = partial,
            failed,
            older = older.is_some(),
            pane_epoch = %pane_epoch,
            "find search concluded"
        );
        let budget = active.epoch_retry_budget;
        match self
            .publication
            .install(list, partial, failed, older, &pane_epoch, host)
        {
            Some((row, epoch)) => self.reveal(row, &epoch, budget, host),
            None => Vec::new(),
        }
    }

    /// Drop stale numbering, then spend one retry against the live pane. A
    /// chain reporting epoch-changed and a reveal finding the pane renumbered
    /// are one rule.
    pub(super) fn invalidate(
        &mut self,
        epoch_retry_budget: u32,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        self.publication.clear(host);
        if epoch_retry_budget > 0 && !self.query.is_empty() {
            self.search_now(epoch_retry_budget - 1, None, host)
        } else {
            tracing::info!(target: "find", session_id = %self.session_id, "find gave up after the grid renumbered");
            self.publication.mark_failed();
            Vec::new()
        }
    }
}
