//! The per-RPC, per-caller request budget: 100 mutations a minute, 600 for
//! `PairPoll` alone. Reached as `core.services.rate_limit`; the mount in
//! `http::listener` hands it the exact Connect method name and the caller
//! address `middleware::caller_origin` resolved. It depends on nothing else.
//!
//! WHY PER PROCESS AND PER CALLER. A bucket set that is per-listener, or keyed
//! on a connection, is a limit every socket can spend again on the next
//! connection: the caller reconnects, the budget it already burned is gone, and
//! the limit never fired. The table is one process-wide runtime keyed on
//! `(rpc, caller address)` — never on a socket.
//!
//! WHY THE CLOCK IS MONOTONIC, NOT WALL CLOCK, where v2 reads `Date.now()`
//! (`rate-limit.ts:108`). A wall-clock refill stops limiting the moment NTP
//! steps the clock forward, and a step backward pins a spent budget shut for as
//! long as the skew lasts; neither is a decision a rate limiter gets to make
//! twice a year. `admit_at` takes the instant as an argument, so the window
//! arithmetic is testable without sleeping and no wall-clock value can be
//! substituted for the monotonic one.
//!
//! WHY EXACT RPC NAMES AND NOT PREFIXES, so a `*List` sibling never shares a
//! mutation's budget — the entry "rate-limit buckets matched by path prefix" in
//! `docs/FAILURE-INDEX.md`. Names rather than paths, because the listener owns
//! the `/roost.v1.CoordinatorService/` prefix.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The window every budget is spent over. 60 s (`rate-limit.ts:68`).
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// Requests a minute a [`RateLimitBucket::Sensitive`] RPC allows.
pub const DEFAULT_TOKENS_PER_WINDOW: u32 = 100;

/// Requests a minute `PairPoll` allows: a device polling for its own approval
/// is expected to, and throttling it throttles the user out of their pairing.
pub const PAIR_POLL_TOKENS_PER_WINDOW: u32 = 600;

/// The process-wide ceiling on live buckets (`rate-limit.ts:69`). Reached, the
/// limiter fails closed rather than evicting a live limit and handing a
/// churning caller a fresh budget.
pub const RATE_LIMIT_MAX_BUCKETS: usize = 10_000;

/// The RPC that polls for the outcome of a pairing request.
pub const PAIR_POLL_METHOD: &str = "PairPoll";

/// The RPCs that spend a budget, by exact Connect method name. Mutations only:
/// SPA bootstrap and tab-focus refresh call the reads constantly, and a budget
/// shared with a create or a revoke is spent before the user acts.
pub const RATE_LIMITED_METHODS: &[&str] = &[
    "AuthMintBootstrap",
    "AuthRedeemWorker",
    "AuthRedeemBrowser",
    "AuthLogout",
    "PairCreate",
    PAIR_POLL_METHOD,
    "PairApprove",
    "PairConfirm",
    "PairDeny",
    "DevicesRevoke",
    "DevicesRotateCurrent",
    "WorkspacesCreate",
    "WorkspacesUpdate",
    "WorkspacesDelete",
    "WorkspacesSetSessions",
    // Task mutations. `TasksNextPending` is absent because workers poll it on a
    // backoff schedule; the three here are the user surfaces.
    "TasksEnqueue",
    "TasksSetState",
    "TasksCancel",
    "McpCreate",
    "McpDelete",
    "McpPublish",
    // Worker control-plane mutations. Register and Heartbeat are absent: the
    // workers send them on a fixed cadence, and throttling a worker throttles
    // the terminal it is carrying.
    "WorkersRename",
    "WorkersDelete",
    "WorkersDeployStart",
    // Transcription: key write, stored-key handoff, and the test that spends
    // it. `TranscriptionGetConfig` is a read.
    "TranscriptionSetConfig",
    "TranscriptionGrantToken",
    "TranscriptionTest",
    // ui-cc mutations. `UiReportState` is absent so an existing tab's bounded
    // heartbeats stay admitted; `UiListStates` is a read.
    "UiDispatch",
    "UiApplyLayout",
    // Install-global search allocates cursors, worker queues and cancel
    // tombstones; the owner caps bound an account, this bounds a caller.
    "SessionsSearchGlobal",
    "SessionsCancelGlobalSearch",
    // Status-fenced prompting mutates a live PTY and may retain a waiter.
    "SessionsPrompt",
];

/// The budget class an RPC spends, which is also the bucket's identity.
///
/// The two rates exist because one default cannot be right for both: every
/// mutation gets 100 a minute, and the one RPC a paired device is expected to
/// hammer while it waits gets 600.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitBucket {
    /// Every mutation surface: [`DEFAULT_TOKENS_PER_WINDOW`] a minute.
    Sensitive,
    /// [`PAIR_POLL_METHOD`]: [`PAIR_POLL_TOKENS_PER_WINDOW`] a minute.
    PairPoll,
}

impl RateLimitBucket {
    /// The requests this bucket allows per window.
    #[must_use]
    pub const fn tokens_per_window(self) -> u32 {
        match self {
            Self::Sensitive => DEFAULT_TOKENS_PER_WINDOW,
            Self::PairPoll => PAIR_POLL_TOKENS_PER_WINDOW,
        }
    }

    /// The window this bucket is spent over.
    #[must_use]
    pub const fn window(self) -> Duration {
        RATE_LIMIT_WINDOW
    }
}

/// The identity a budget is spent by — a CALLER, never a connection.
///
/// The mount builds one from the address `middleware::caller_origin` resolved
/// under the listener's boot-selected trust profile. A caller whose address
/// could not be resolved shares one budget with every other such caller: v2's
/// `"unknown"`, which is fail-closed rather than a hole.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RateLimitCaller(String);

impl RateLimitCaller {
    /// A caller identified by its resolved client address.
    #[must_use]
    pub fn from_client_ip(client_ip: impl Into<String>) -> Self {
        Self(client_ip.into())
    }

    /// The identity a bucket is keyed by.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which limit refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitRefusalReason {
    /// The caller's own window budget is spent.
    Budget,
    /// The process-wide bucket table is full and could not be pruned.
    AtCapacity,
}

/// A refused request, and what the caller is told about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitRefusal {
    /// The budget that refused.
    pub bucket: RateLimitBucket,
    /// Which limit refused.
    pub reason: RateLimitRefusalReason,
    /// Whole seconds to wait, for `Retry-After`. Never longer than the window
    /// itself, so a rewound clock cannot advise a backoff that costs more than
    /// the budget being waited on.
    pub retry_after_seconds: u64,
    /// True for the first refusal of this window, so a blocked client that
    /// retries in a loop produces one `rate_limited` line, not one per request.
    pub first_in_window: bool,
}

/// One `(rpc, caller)` window and what is left of it.
#[derive(Debug)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
    rate: RateLimitBucket,
    refusal_reported: bool,
}

/// A caller plus the rpc it called, which is what a window belongs to.
type BucketKey = (String, String);

/// The bounded process table of live windows.
#[derive(Debug)]
struct BucketTable {
    buckets: HashMap<BucketKey, Bucket>,
    max_buckets: usize,
    /// When a full-table refusal was last reported, so a churning caller cannot
    /// turn the capacity refusal into a log flood.
    capacity_reported_at: Option<Instant>,
}

impl BucketTable {
    fn new(max_buckets: usize) -> Self {
        Self {
            buckets: HashMap::new(),
            max_buckets,
            capacity_reported_at: None,
        }
    }

    /// Spend one request from this caller's window, or refuse it.
    fn spend(
        &mut self,
        method: &str,
        caller: &str,
        rate: RateLimitBucket,
        now: Instant,
    ) -> Option<RateLimitRefusal> {
        let key = (method.to_owned(), caller.to_owned());
        if let Some(bucket) = self.buckets.get_mut(&key) {
            // A window that has not elapsed under the rate the rpc spends today
            // is the caller's to spend from. Otherwise the window ran out or the
            // rate changed: a new rate takes effect on the next request, not
            // after the old window runs out.
            if now < bucket.reset_at && bucket.rate == rate {
                return charge(bucket, rate.window(), now);
            }
        }
        if self.buckets.len() >= self.max_buckets {
            self.buckets.retain(|_, bucket| now < bucket.reset_at);
        }
        if self.buckets.len() >= self.max_buckets {
            let window = rate.window();
            let first_in_window = self
                .capacity_reported_at
                .is_none_or(|reported| now >= reported + window);
            return Some(RateLimitRefusal {
                bucket: rate,
                reason: RateLimitRefusalReason::AtCapacity,
                retry_after_seconds: seconds_in(window),
                first_in_window,
            });
        }
        // A window opens full and spends its first token on the request that
        // opened it, exactly as v2 does by falling through to the same charge.
        self.buckets.insert(
            key,
            Bucket {
                remaining: rate.tokens_per_window().saturating_sub(1),
                reset_at: now + rate.window(),
                rate,
                refusal_reported: false,
            },
        );
        None
    }

    fn note_capacity_report(&mut self, now: Instant) {
        self.capacity_reported_at = Some(now);
    }
}

/// Spend one request from a live window, or refuse it without spending.
///
/// A refusal leaves `remaining` where it was. Decrementing on a refusal, or
/// moving the window forward, would let a caller probe the boundary: spend the
/// last token, keep asking, and push the reset out as far as they care to wait.
fn charge(bucket: &mut Bucket, window: Duration, now: Instant) -> Option<RateLimitRefusal> {
    if bucket.remaining > 0 {
        bucket.remaining -= 1;
        return None;
    }
    let first_in_window = !bucket.refusal_reported;
    bucket.refusal_reported = true;
    Some(RateLimitRefusal {
        bucket: bucket.rate,
        reason: RateLimitRefusalReason::Budget,
        retry_after_seconds: seconds_in(bucket.reset_at.saturating_duration_since(now).min(window)),
        first_in_window,
    })
}

/// Whole seconds in a span, rounded up and never zero.
fn seconds_in(span: Duration) -> u64 {
    u64::try_from(span.as_millis().div_ceil(1_000).max(1)).unwrap_or(u64::MAX)
}

/// The request budget one coordinator process enforces.
///
/// Reached as `core.services.rate_limit`. `new()` takes nothing and must keep
/// taking nothing: an operator-configured rate is read at call time from
/// `core.services.boot`, not frozen into this constructor.
#[derive(Debug)]
pub struct RateLimiter {
    table: Mutex<BucketTable>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    /// A limiter over the process's default ceiling of live buckets.
    #[must_use]
    pub fn new() -> Self {
        let ceiling = NonZeroUsize::new(RATE_LIMIT_MAX_BUCKETS).unwrap_or(NonZeroUsize::MIN);
        Self::with_max_buckets(ceiling)
    }

    /// A limiter over a narrower ceiling, for a test that can reach capacity.
    #[must_use]
    pub fn with_max_buckets(max_buckets: NonZeroUsize) -> Self {
        Self {
            table: Mutex::new(BucketTable::new(max_buckets.get())),
        }
    }

    /// How many windows are live right now.
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.with_table(|table| table.buckets.len())
    }

    /// Admit one request as of the process's monotonic clock, refusing it with
    /// the `Retry-After` seconds to wait when the budget is spent.
    pub fn admit(&self, method: &str, caller: &RateLimitCaller) -> Option<RateLimitRefusal> {
        self.admit_at(method, caller, Instant::now())
    }

    /// Admit one request as of `now`. `None` means admitted.
    ///
    /// The instant is an argument so the window arithmetic is exercisable
    /// without sleeping, and so a wall-clock instant cannot be mistaken for the
    /// monotonic one the window is measured on.
    pub fn admit_at(
        &self,
        method: &str,
        caller: &RateLimitCaller,
        now: Instant,
    ) -> Option<RateLimitRefusal> {
        let rate = bucket_for_method(method)?;
        let identity = caller.as_str();
        let refusal = self.with_table(|table| table.spend(method, identity, rate, now));
        let refusal = refusal?;
        if refusal.first_in_window {
            // The budget is gone or the table is full: both change what the
            // caller is told, so both say so out loud rather than dropping a
            // 429 with no trace of why.
            tracing::warn!(
                caller = identity,
                method,
                reason = ?refusal.reason,
                retry_after_seconds = refusal.retry_after_seconds,
                "rate_limited"
            );
            if refusal.reason == RateLimitRefusalReason::AtCapacity {
                self.with_table(|table| table.note_capacity_report(now));
            }
        }
        Some(refusal)
    }

    /// The lock is never held across an await, and a poisoned one is rebuilt:
    /// the table is plain process state with no invariant a panic mid-spend
    /// could leave broken, and propagating that panic into every later request
    /// would turn one bad thread into an outage.
    fn with_table<R>(&self, body: impl FnOnce(&mut BucketTable) -> R) -> R {
        let mut guard = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        body(&mut guard)
    }
}

/// The budget `method` spends, or `None` when it is not rate limited.
///
/// Matched by exact name, never by prefix: a prefix that catches `Workspaces`
/// also catches the `WorkspacesList` that every SPA bootstrap and every
/// visibilitychange refresh calls, and that read then spends the budget of the
/// create, update and delete behind it.
#[must_use]
pub fn bucket_for_method(method: &str) -> Option<RateLimitBucket> {
    RATE_LIMITED_METHODS
        .iter()
        .find(|listed| **listed == method)
        .map(|listed| {
            if *listed == PAIR_POLL_METHOD {
                RateLimitBucket::PairPoll
            } else {
                RateLimitBucket::Sensitive
            }
        })
}
