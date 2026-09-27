//! Optimistic spawn: the row that appears before the coordinator has agreed to
//! it, and the rule for which answer is still allowed to act on it.
//!
//! A spawn has THREE outcomes, and the third is the one a naive port misses.
//! It can be **admitted**, it can be **rejected**, and it can be
//! **superseded** — because the user respawned, or closed the pending tab, or
//! the answer simply arrived after a newer attempt for the same session had
//! already begun. A late rejection from a superseded attempt must not delete the
//! placeholder the newer attempt established, and must not raise a failure card
//! over a terminal that is working.
//!
//! So the question "is this response still relevant?" is answered HERE, as a
//! property of the mutation:
//!
//! - every attempt carries an [`SpawnTicket`] the LEDGER minted, and the
//!   ledger is the only thing that knows which attempt owns a session id;
//! - [`settle_spawn_rejected`] and [`settle_spawn_admitted`] take that ticket
//!   and return a [`SpawnSettlement`]. They write only when the ticket names the
//!   live, still-pending attempt, and otherwise return
//!   [`SpawnSettlement::Superseded`] having touched nothing;
//! - no caller compares anything. A call site that wanted to check first would
//!   be a second place to forget, and the failure it would produce — a live
//!   terminal deleted by a request the user abandoned — is the worst one in the
//!   client.
//!
//! The placeholder is a [`ClientOnlySession`] in this ledger and NOT a row in
//! the authoritative `SessionPlane`. v2 inserted a forged `Session` into the
//! root store, which meant a bootstrap snapshot arriving mid-spawn could prune
//! it and the shared fold could overwrite it. Here the two cannot collide: the
//! ledger holds a client-only row until [`reconcile_spawn`] is told the
//! authoritative one arrived, and the fold is never handed a row it did not mint.
//!
//! Ported from `apps/web/src/store/optimisticSpawn.ts` (239 lines). Dropped: the
//! measurement deferred, because a synchronous core has no promise — the mounted
//! placeholder's real geometry reaches the spawn through `ViewOpened`, which is
//! the same measurement the view path already carries. The `aborted` set becomes
//! a tombstone with a reason; the client-only id set becomes the ledger's own
//! admitted entries; `MAX_RETAINED_CLIENT_ONLY_SPAWN_IDS` bounds the tombstones.
pub mod settle;

use std::collections::{BTreeMap, VecDeque};

pub use settle::{
    abort_optimistic_spawn, begin_optimistic_spawn, reconcile_spawn, respawn_optimistic_spawn,
    settle_spawn_admitted, settle_spawn_rejected,
};

/// How many settled attempts keep a tombstone. A refusal older than this is
/// reported as never having existed, which changes a log line and nothing else.
pub const MAX_RETAINED_SPAWN_TOMBSTONES: usize = 256;

/// A pending request, and the attempt the ledger minted for it.
///
/// The session id is the HOST's to mint: it needs `crypto.randomUUID`, and a
/// client core with no random source must not invent one. The ATTEMPT is the
/// store's, because it is store sequencing and nothing else may hold it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpawnTicket {
    /// The client-minted session id the spawn will be reported under.
    pub session_id: String,
    /// The attempt the ledger minted. Monotonic, so a slow answer is still
    /// correlatable and a newer attempt is always recognisable as newer.
    pub attempt: u64,
}

impl SpawnTicket {
    /// The subject a card about this spawn is keyed by.
    pub fn as_str(&self) -> &str {
        &self.session_id
    }
}

/// A session that exists only in this browser, until the coordinator admits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOnlySession {
    /// The client-minted session id.
    pub id: String,
    /// The machine the terminal is being spawned on.
    pub worker_fp: String,
    /// The folder it is being spawned in. Terminals follow their live cwd, so
    /// this starts at the anchor's folder and is not the spawn folder once the
    /// shell moves.
    pub cwd: String,
    /// The immutable spawn folder, which a `cd` does not change.
    pub spawn_cwd: String,
    /// The workspace the anchor belonged to, if any.
    pub workspace_id: Option<String>,
    /// When this browser minted the row. Ordering reads it, so a new terminal
    /// lands last in the list rather than first.
    pub created_at_ms: i64,
}

/// Where a ledger entry is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    /// The request is in flight. Only a pending entry can be settled.
    Pending,
    /// The coordinator admitted it. The row stays — it is the tab the user is
    /// looking at — until the authoritative row replaces it.
    Admitted,
}

/// One placeholder and the attempt that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpawnEntry {
    ticket: SpawnTicket,
    placeholder: ClientOnlySession,
    state: EntryState,
}

/// Why a settled attempt left the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TombstoneReason {
    /// The user closed the pending tab before the answer landed.
    Retracted,
    /// The coordinator admitted it.
    Admitted,
    /// The coordinator refused it.
    Rejected,
}

impl TombstoneReason {
    /// Why a late answer for this attempt is now stale.
    const fn superseded(self) -> SupersededReason {
        match self {
            Self::Retracted => SupersededReason::Retracted,
            Self::Admitted | Self::Rejected => SupersededReason::AlreadySettled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Tombstone {
    session_id: String,
    attempt: u64,
    reason: TombstoneReason,
}

/// Why a response was not allowed to act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupersededReason {
    /// The user retracted the attempt — they closed the pending tab. An expected
    /// removal, and the reason v2's `aborted` set existed for.
    Retracted,
    /// A newer attempt for the same session has begun. The newer one owns this
    /// session id and its state, full stop.
    ReplacedByNewerAttempt,
    /// An earlier answer already settled this attempt.
    AlreadySettled,
    /// No attempt with this number was ever minted, or its tombstone has aged
    /// out. A caller bug or a very late answer.
    NeverExisted,
}

/// What a settlement did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnSettlement {
    /// The answer belonged to the attempt that owns this session, and the ledger
    /// acted on it.
    Applied,
    /// The answer is about an attempt that no longer decides anything. Nothing
    /// was written, and nothing was raised.
    Superseded(SupersededReason),
}

impl SpawnSettlement {
    /// Whether the ledger acted.
    pub const fn is_applied(self) -> bool {
        matches!(self, Self::Applied)
    }
}

/// Why a spawn could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnRefusal {
    /// The host's id is not a UUID. The coordinator's own `SessionId` is one, so
    /// a row keyed by anything else could never be replaced by the authoritative
    /// row — the tab would sit client-only forever.
    NotAUuid {
        /// What the host proposed.
        session_id: String,
    },
    /// Nothing is spawning for this id, so there is nothing to respawn: the
    /// caller holds a ticket the ledger has already released.
    NoPendingSpawn {
        /// The session the ticket named.
        session_id: String,
    },
}

/// The in-flight spawns, the placeholders, and the tombstones of the attempts
/// that have been answered or retracted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpawnLedger {
    entries: BTreeMap<String, SpawnEntry>,
    tombstones: VecDeque<Tombstone>,
    next_attempt: u64,
}

impl SpawnLedger {
    /// Nothing pending.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            tombstones: VecDeque::new(),
            next_attempt: 1,
        }
    }

    /// How many placeholders this browser is holding.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this browser holds no placeholder.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The placeholders, oldest first by the browser's own mint instant.
    pub fn client_only_sessions(&self) -> Vec<ClientOnlySession> {
        let mut placeholders: Vec<ClientOnlySession> = self
            .entries
            .values()
            .map(|entry| entry.placeholder.clone())
            .collect();
        placeholders.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        placeholders
    }

    /// Whether this browser is holding a placeholder for `session_id`.
    ///
    /// The membership test a sidebar row and a palette row need: such a row is
    /// NOT an authoritative session and must not be counted as one.
    pub fn is_client_only(&self, session_id: &str) -> bool {
        self.entries.contains_key(session_id)
    }

    /// Whether `session_id`'s spawn is still in flight.
    pub fn is_pending(&self, session_id: &str) -> bool {
        self.entries
            .get(session_id)
            .is_some_and(|entry| entry.state == EntryState::Pending)
    }

    /// The ticket that currently owns `session_id`, if any.
    pub fn ticket_for(&self, session_id: &str) -> Option<SpawnTicket> {
        self.entries
            .get(session_id)
            .map(|entry| entry.ticket.clone())
    }

    /// Drop everything, at a credential boundary.
    ///
    /// A suspended pre-switch spawn continuation must not be able to navigate or
    /// issue follow-up work after the new scope is hydrated, so its answers are
    /// refused rather than acted on.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.tombstones.clear();
        self.next_attempt = 1;
    }

    /// Whether `ticket` is the attempt that decides this session's state.
    fn relevance(&self, ticket: &SpawnTicket) -> Option<SupersededReason> {
        match self.entries.get(&ticket.session_id) {
            Some(entry) if entry.ticket.attempt == ticket.attempt => None,
            Some(entry) if entry.ticket.attempt > ticket.attempt => {
                Some(SupersededReason::ReplacedByNewerAttempt)
            }
            // A ticket older than the live attempt is a replaced attempt; a
            // ticket newer than anything this ledger holds was never minted.
            Some(_) => Some(SupersededReason::NeverExisted),
            None => Some(
                self.tombstone_reason(&ticket.session_id, ticket.attempt)
                    .map_or(SupersededReason::NeverExisted, TombstoneReason::superseded),
            ),
        }
    }

    fn tombstone_reason(&self, session_id: &str, attempt: u64) -> Option<TombstoneReason> {
        self.tombstones
            .iter()
            .find(|stone| stone.session_id == session_id && stone.attempt == attempt)
            .map(|stone| stone.reason)
    }

    fn mint_attempt(&mut self) -> u64 {
        let attempt = self.next_attempt;
        self.next_attempt = self.next_attempt.saturating_add(1);
        attempt
    }

    fn remember(&mut self, session_id: String, attempt: u64, reason: TombstoneReason) {
        self.tombstones.push_back(Tombstone {
            session_id,
            attempt,
            reason,
        });
        while self.tombstones.len() > MAX_RETAINED_SPAWN_TOMBSTONES {
            self.tombstones.pop_front();
        }
    }
}
