//! Scrollback search, over one session or a page-budget's worth. Owned by the
//! worker.
//!
//! The scan belongs to the terminal slice, which owns the grid and the matcher.
//! What is here is the part a browser COMMAND owns and the scanner does not:
//! which identity a search runs under, whether there is room for it, and what
//! happens to one that arrives after it was cancelled.
//!
//! THE TWO FAMILIES ARE OWNED BY DIFFERENT IDENTITIES, and the difference is
//! load-bearing rather than incidental. A single-session search is owned by
//! the browser DOCUMENT that asked, over the one session it named — so a
//! second tab in the same document asking again REPLACES the first tab's
//! search rather than running beside it, which is what keeps a document from
//! stacking one scan per open tab. A fleet-wide search is owned by the VIEWER
//! over the whole worker, because it is a navigation rather than a read of one
//! terminal, and one viewer navigates once.
//!
//! A CANCEL IS OWNED BY WHATEVER CAN ABANDON IT, and a tombstone a different
//! identity cannot consume is a tombstone that never fires. That is why the
//! cancel arm reads the browser id for a single search and the viewer id for a
//! batch: it has to be the identity the search will be admitted under, or the
//! search runs against a cancel its own caller made.
//!
//! CANCEL IS RECORDED BEFORE A SEARCH IS ADMITTED. A browser that scrolls away
//! abandons a search, and the abandon routinely overtakes the search on the
//! wire; the ledger in [`super::search_cancellation`] is what makes the second
//! one stop instead of scanning a grid nobody is watching.

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;

use super::search_cancellation::{Cancellations, search_owner_key};
use super::{Answered, Boxed, Command, Deps, Refusal, Reply};

/// How many searches may run at once on one worker.
///
/// Every scan walks rows while holding every other session's PTY output, so an
/// unbounded number is one browser asking for enough of them to make the whole
/// machine's terminals stutter. A search that arrives at the bound is refused
/// with a reason rather than queued behind work the caller cannot see.
pub const MAX_ACTIVE_SEARCHES: usize = 8;

/// The scope a fleet-wide search runs over: the whole worker, as opposed to
/// one session. Named rather than left as an empty string so a diagnostic
/// report reads as what it is.
const WORKER_SCOPE: &str = "worker";

/// One search this worker is running.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    /// `search_owner_key` of the viewer's search on one channel, or the
    /// viewer's fleet-wide cursor.
    owner_key: String,
    search_id: String,

    /// Whether this is a fleet-wide cursor rather than a single-session scan.
    batch: bool,
}

/// What a caller needs in order to release the slot it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTicket {
    owner_key: String,
}

/// The searches this worker is running, and the cancels waiting to meet one.
#[derive(Debug, Default)]
pub struct Searches {
    running: Vec<Running>,
    cancellations: Cancellations,
}

impl Searches {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many searches are running.
    pub fn active(&self) -> usize {
        self.running.len()
    }

    /// The identity one viewer's search over one thing runs under.
    pub fn owner_key(&self, scope: &str, owner: &str) -> String {
        search_owner_key(scope, owner)
    }

    /// A cancel arrived: record it, so the search it names stops when it
    /// arrives.
    ///
    /// Recording is unconditional, even when no search is running, because
    /// the search this cancel is for has not arrived yet. That inversion is
    /// the whole reason the ledger exists.
    pub fn cancel(&mut self, owner: &str, session_id: &SessionId, search_id: &str, now_ms: u64) {
        self.cancellations
            .record(owner, session_id, search_id, now_ms);
    }

    /// Whether a search was cancelled before it was admitted.
    pub fn consume_cancel(
        &mut self,
        owner: &str,
        session_id: &SessionId,
        search_id: &str,
        now_ms: u64,
    ) -> bool {
        self.cancellations
            .consume(owner, session_id, search_id, now_ms)
    }

    /// Admit a search, or refuse it: cancelled, or past the bound.
    pub fn admit(
        &mut self,
        owner_key: &str,
        search_id: &str,
        batch: bool,
    ) -> Result<SearchTicket, Refusal> {
        // A restarted search REPLACES the one under the same key rather than
        // competing with it for the bound: the caller has already decided it
        // wants this one, and holding the old one only delays it.
        self.running.retain(|held| held.owner_key != owner_key);
        if self.running.len() >= MAX_ACTIVE_SEARCHES {
            return Err(Refusal::failed(
                "search",
                "too many active scrollback searches",
            ));
        }
        self.running.push(Running {
            owner_key: owner_key.to_owned(),
            search_id: search_id.to_owned(),
            batch,
        });
        Ok(SearchTicket {
            owner_key: owner_key.to_owned(),
        })
    }

    /// A search finished. The slot is released only when the entry still names
    /// it: a restarted search reuses the key, and a late completion for the
    /// previous one must not release the new one's slot.
    pub fn finish(&mut self, ticket: &SearchTicket) {
        self.running
            .retain(|held| held.owner_key != ticket.owner_key);
    }

    /// The searches running, for a diagnostic report.
    pub fn running(&self) -> Vec<(&str, &str, bool)> {
        self.running
            .iter()
            .map(|held| (held.owner_key.as_str(), held.search_id.as_str(), held.batch))
            .collect()
    }
}

/// One single-session search, as the scanner is asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleSearch {
    pub session_id: String,
    pub grid_epoch: String,
    pub search_id: String,
    pub query: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// Exclusive: a page resumes at this row and older.
    pub before_row: Option<u32>,
    pub max_rows: u32,
    pub max_matches: u32,
}

/// One fleet-wide search, as the scanner is asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSearch {
    pub search_id: String,
    pub query: String,
    pub case_sensitive: bool,
    pub max_rows_per_session: u32,
    pub max_matches: u32,
    pub deadline_ms: u32,
    /// The sessions this page spans, each with the epoch the caller holds.
    pub sessions: Vec<(String, String, Option<u32>)>,
}

/// A scan over one session's grid, or over a page budget's worth of them.
pub trait ScrollbackSearch: Send + Sync {
    /// One page of matches from one session.
    fn search(&self, request: SingleSearch) -> Boxed<Result<serde_json::Value, Refusal>>;

    /// One page of matches from a page budget's worth of sessions.
    fn search_batch(&self, request: BatchSearch) -> Boxed<Result<serde_json::Value, Refusal>>;
}

/// Run whichever search command arrived.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    match &command.frame {
        ClientControlFrame::SearchScrollback {
            session_id,
            search_id,
            grid_epoch,
            query,
            case_sensitive,
            regex,
            before_row,
            max_rows,
            max_matches,
            ..
        } => {
            let now = now_ms();
            if held(deps)?.consume_cancel(&command.browser_id, session_id, search_id.as_str(), now)
            {
                return Err(Refusal::failed("search-scrollback", "search superseded"));
            }
            let ticket = held(deps)?.admit(
                &search_owner_key(session_id.as_str(), &command.browser_id),
                search_id.as_str(),
                false,
            )?;
            let outcome = deps
                .search
                .search(SingleSearch {
                    session_id: session_id.as_str().to_owned(),
                    grid_epoch: grid_epoch.as_str().to_owned(),
                    search_id: search_id.as_str().to_owned(),
                    query: query.as_str().to_owned(),
                    regex: *regex,
                    case_sensitive: *case_sensitive,
                    before_row: before_row.map(|row| row.as_i64() as u32),
                    max_rows: rows(*max_rows),
                    max_matches: rows(*max_matches),
                })
                .await;
            held(deps)?.finish(&ticket);
            Ok(Answered::Reply(Reply::ok(&command.request_id, outcome?)))
        }
        ClientControlFrame::SearchScrollbackBatch {
            search_id,
            query,
            case_sensitive,
            sessions,
            max_rows_per_session,
            max_matches,
            deadline_ms,
            ..
        } => {
            let now = now_ms();
            // A batch cancel names the sessions the batch would have spanned,
            // so one that arrived first is consumed here, per session. One
            // consumed is enough to stop the page: the scan is bounded by the
            // sessions list, and a caller that cancelled any of them has
            // abandoned the navigation they were in the middle of.
            // The ledger is held only for the admission decision and the
            // shape it hands back; the scan below runs with it released, so a
            // slow fleet-wide page never blocks another command's admission.
            let ticket = {
                let mut searches = held(deps)?;
                let cancelled = sessions.iter().any(|session| {
                    searches.consume_cancel(
                        &command.viewer_id,
                        &session.session_id,
                        search_id.as_str(),
                        now,
                    )
                });
                if cancelled {
                    return Err(Refusal::failed(
                        "search-scrollback-batch",
                        "search superseded",
                    ));
                }
                searches.admit(
                    &search_owner_key(WORKER_SCOPE, &command.viewer_id),
                    search_id.as_str(),
                    true,
                )?
            };
            let request = BatchSearch {
                search_id: search_id.as_str().to_owned(),
                query: query.as_str().to_owned(),
                case_sensitive: *case_sensitive,
                max_rows_per_session: rows(*max_rows_per_session),
                max_matches: rows(*max_matches),
                deadline_ms: rows(*deadline_ms),
                sessions: sessions
                    .iter()
                    .map(|session| {
                        (
                            session.session_id.as_str().to_owned(),
                            session.grid_epoch.as_str().to_owned(),
                            session.before_row.map(|row| row.as_i64() as u32),
                        )
                    })
                    .collect(),
            };
            let outcome = deps.search.search_batch(request).await;
            held(deps)?.finish(&ticket);
            Ok(Answered::Reply(Reply::ok(&command.request_id, outcome?)))
        }
        ClientControlFrame::CancelScrollbackSearch {
            session_id,
            search_request_id,
            ..
        } => {
            held(deps)?.cancel(
                &command.browser_id,
                session_id,
                search_request_id.as_str(),
                now_ms(),
            );
            Ok(Answered::Silent)
        }
        ClientControlFrame::CancelScrollbackSearchBatch {
            search_id,
            session_ids,
            ..
        } => {
            let now = now_ms();
            let mut searches = held(deps)?;
            for session_id in session_ids.iter() {
                searches.cancel(&command.viewer_id, session_id, search_id.as_str(), now);
            }
            Ok(Answered::Silent)
        }
        other => Err(Refusal::failed(
            "search",
            format!("{} is not a search command", other.kind()),
        )),
    }
}

fn held(deps: &Deps) -> Result<std::sync::MutexGuard<'_, Searches>, Refusal> {
    deps.searches
        .lock()
        .map_err(|_| Refusal::failed("search", "the search ledger is unusable"))
}

fn rows(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}
