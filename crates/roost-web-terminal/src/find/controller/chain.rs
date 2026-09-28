//! The search that is running: is it still the pane's, and what did it conclude?
//!
//! A file split, not a type split: the state lives on `TerminalFind`, which
//! `controller` owns. `controller` decides the query, the debounce and the
//! reveal; this decides the cancellable page chain and the epoch fence that
//! turns a match found under retired grid numbering into no match at all.

use crate::find::controller::{ActiveSearch, TerminalFind};
use crate::find::{
    ChainOutcome, ChainStep, FindChain, FindCommand, FindHost, OlderMatchPage, SearchReply,
};

impl TerminalFind {
    /// Absorb one page of the active search.
    pub fn on_page(&mut self, reply: &SearchReply, host: &mut dyn FindHost) -> Vec<FindCommand> {
        let Some(mut active) = self.take_current() else {
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

    /// Absorb a page that never arrived. A grid that moved under a failed request
    /// is epoch-changed; only that is retried.
    pub fn on_search_error(&mut self, host: &mut dyn FindHost) -> Vec<FindCommand> {
        let Some(active) = self.take_current() else {
            return Vec::new();
        };
        let moved = !active.chain.pane_accepts_epoch(&host.pane_epoch());
        let outcome = if moved {
            ChainOutcome::EpochChanged
        } else {
            active.chain.failed_partial()
        };
        self.install_chain(active, outcome, host)
    }

    /// Start one chain for the current query.
    pub(super) fn search_now(
        &mut self,
        epoch_retry_budget: u32,
        resume: Option<OlderMatchPage>,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        let carried = if resume.is_some() {
            self.publication.matches().to_vec()
        } else {
            Vec::new()
        };
        let mut commands = self.stop_active();
        if self.query.is_empty() {
            self.publication.clear(host);
            return commands;
        }
        self.searches_minted += 1;
        let search_id = format!("find-{}", self.searches_minted);
        // A resumed chain keeps its cursor's epoch; a fresh one pins the pane's.
        let epoch = resume
            .as_ref()
            .map_or_else(|| host.pane_epoch(), |page| page.epoch.clone());
        let flags = (self.case_sensitive, self.regex);
        let chain = FindChain::new(
            &self.session_id,
            &search_id,
            &self.query,
            flags,
            &epoch,
            resume.as_ref(),
        );
        self.active = Some(ActiveSearch {
            search_id,
            token: self.token,
            chain,
            carried,
            epoch_retry_budget,
        });
        // A chain minted one line above is current, within budget and pinned to
        // the pane's own epoch, so its only possible first step is a request.
        let pane_epoch = host.pane_epoch();
        let fresh = self.active.take();
        let step = fresh
            .as_ref()
            .map_or(ChainStep::Finish(ChainOutcome::Abandoned), |s| {
                s.chain.next_request(&pane_epoch, !self.disposed)
            });
        match step {
            ChainStep::Issue(request) => commands.push(FindCommand::Search(request)),
            ChainStep::Finish(outcome) => {
                if let Some(finished) = fresh {
                    commands.extend(self.install_chain(finished, outcome, host));
                }
            }
        }
        commands
    }

    /// Publish what one chain concluded, with the chain already retired.
    fn install_chain(
        &mut self,
        active: ActiveSearch,
        outcome: ChainOutcome,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        match outcome {
            ChainOutcome::Abandoned => Vec::new(),
            ChainOutcome::EpochChanged => self.invalidate(active.epoch_retry_budget, host),
            ChainOutcome::Matches {
                matches,
                truncated,
                failed,
                older,
            } => {
                let mut list = active.carried;
                list.extend(matches);
                // A partial that found nothing is a plain failure, not a truncated
                // result: there is no older window to page into.
                let partial = truncated || (failed && !list.is_empty());
                let pane_epoch = host.pane_epoch();
                let budget = active.epoch_retry_budget;
                match self
                    .publication
                    .install(list, partial, failed, older, &pane_epoch, host)
                {
                    Some((row, epoch)) => self.reveal(row, &epoch, budget, host),
                    None => Vec::new(),
                }
            }
        }
    }

    /// Drop stale numbering, then spend one retry. A chain reporting
    /// `epoch-changed` and a reveal finding the pane renumbered are one rule.
    pub(super) fn invalidate(
        &mut self,
        epoch_retry_budget: u32,
        host: &mut dyn FindHost,
    ) -> Vec<FindCommand> {
        self.publication.clear(host);
        if epoch_retry_budget > 0 && !self.query.is_empty() {
            self.search_now(epoch_retry_budget - 1, None, host)
        } else {
            self.publication.mark_failed();
            Vec::new()
        }
    }
}
