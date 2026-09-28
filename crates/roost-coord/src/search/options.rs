//! The page limits one global search binds to, normalized once before the
//! cursor binding, and the split of a page's match budget across sessions.
//!
//! Ports `apps/coord/src/search/global-search-options.ts` and v2's
//! `allocateGlobalSearchMatchLimits` (`packages/protocol/src/terminal-search.ts`).
//! Called by `search::rpc_search` (authorization breadth, worker work, cursor
//! identity) and `search::fanout` / `search::worker_result` (budgets), so the
//! page semantics cannot drift between the request and its validation.

use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
};

/// The effective limits of one global search page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalSearchPageLimits {
    /// How many authorized sessions one page may select.
    pub max_sessions: usize,
    /// How many rows the worker may scan per session.
    pub max_rows_per_session: u32,
    /// How many matches the whole page may carry.
    pub max_matches: u32,
}

/// Zero means "the contract maximum"; anything else is capped at it.
fn requested_or_maximum(requested: u32, maximum: u32) -> u32 {
    if requested == 0 {
        maximum
    } else {
        requested.min(maximum)
    }
}

/// Normalize the three caller-requested limits of a `SessionsSearchGlobal`.
#[must_use]
pub fn normalize_global_search_page_limits(
    max_sessions: u32,
    max_rows_per_session: u32,
    max_matches: u32,
) -> GlobalSearchPageLimits {
    let session_cap = u32::try_from(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS).unwrap_or(u32::MAX);
    GlobalSearchPageLimits {
        max_sessions: requested_or_maximum(max_sessions, session_cap) as usize,
        max_rows_per_session: requested_or_maximum(
            max_rows_per_session,
            GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
        ),
        max_matches: requested_or_maximum(max_matches, GLOBAL_TERMINAL_SEARCH_MAX_MATCHES),
    }
}

/// Divide a page's match cap across `session_count` sessions so no selected
/// session is starved: every session gets the floor, the first `remainder`
/// sessions one more. Empty for zero sessions (v2 throws; no caller asks).
#[must_use]
pub fn allocate_global_search_match_limits(total_matches: u32, session_count: usize) -> Vec<u32> {
    let Ok(count) = u32::try_from(session_count) else {
        return Vec::new();
    };
    if count == 0 {
        return Vec::new();
    }
    let base = total_matches / count;
    let remainder = total_matches % count;
    (0..count)
        .map(|index| base + u32::from(index < remainder))
        .collect()
}
