//! One authenticated tab id per browser document, and the arbitration that
//! keeps a duplicated tab from sharing it.
//!
//! Ported from `apps/web/src/client/auth/tab-id.ts`. Browser tab duplication
//! copies `sessionStorage`, so the id a document finds under [`TAB_ID_KEY`] may
//! belong to a live sibling. The host claims it before any transport: a Web
//! Lock named [`TAB_ID_LOCK_PREFIX`]`+id`, else a probe on the
//! [`TAB_ID_CLAIM_CHANNEL`] `BroadcastChannel` that waits
//! [`BROADCAST_PROBE_WAIT_MS`] for an owner to answer. An occupied id is
//! re-minted and claimed again; an id no primitive can arbitrate is kept.
//!
//! [`TabIdentity`] is the decision table with no browser in it: it returns a
//! [`ClaimStep`] the host performs and takes the answer back. The host owns the
//! lock callback (held pending for the document lifetime) and the channel,
//! which stays open after a broadcast claim so it can answer later probes.
//! Called by `roost-web`'s `platform::tab_id`.

use std::rc::Rc;

use crate::client::auth::ceremony::RandomSource;
use crate::platform::{Clock, KeyValueStore};

/// The `sessionStorage` key the document's tab id is kept under.
pub const TAB_ID_KEY: &str = "roost.tabId";
/// The Web Lock name prefix; the lock is `roost.tab-id:<id>`.
pub const TAB_ID_LOCK_PREFIX: &str = "roost.tab-id:";
/// The `BroadcastChannel` the fallback probes and answers on.
pub const TAB_ID_CLAIM_CHANNEL: &str = "roost.tab-id-claim-v1";
/// How long a broadcast probe waits for an owner before claiming the id.
pub const BROADCAST_PROBE_WAIT_MS: u32 = 80;

/// Which primitive found the conflict that rotated an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arbitration {
    WebLocks,
    BroadcastChannel,
}

impl Arbitration {
    fn as_str(self) -> &'static str {
        match self {
            Self::WebLocks => "web-locks",
            Self::BroadcastChannel => "broadcast-channel",
        }
    }
}

/// How a probe settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BroadcastAttempt {
    Acquired,
    Occupied,
    Unavailable,
}

/// What an `ifAvailable` exclusive Web Lock request answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockAttempt {
    /// The callback ran with a lock; the host now holds it for the document.
    Acquired,
    /// The callback ran with `null`: another document holds this id.
    Occupied,
    /// `request` threw or rejected before its callback ran.
    Unavailable,
}

/// Which arbitration primitives this document exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArbitrationPrimitives {
    /// `navigator.locks` is present.
    pub web_locks: bool,
    /// `BroadcastChannel` is constructible.
    pub broadcast_channel: bool,
}

/// One message on the claim channel (v2 `{ type, id, nonce }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimMessage {
    /// "Does any document own `id`?"
    Probe { id: String, nonce: String },
    /// "I own `id`", answering the probe that carried `nonce`.
    Occupied { id: String, nonce: String },
}

impl ClaimMessage {
    /// The message whose `type`, `id` and `nonce` properties read as given; a
    /// missing or non-string property, or an unknown `type`, is not a message.
    pub fn parse(kind: Option<&str>, id: Option<String>, nonce: Option<String>) -> Option<Self> {
        let (id, nonce) = (id?, nonce?);
        match kind? {
            "probe" => Some(Self::Probe { id, nonce }),
            "occupied" => Some(Self::Occupied { id, nonce }),
            _ => None,
        }
    }

    /// The `type` property.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Probe { .. } => "probe",
            Self::Occupied { .. } => "occupied",
        }
    }

    /// The `id` property.
    pub fn id(&self) -> &str {
        match self {
            Self::Probe { id, .. } | Self::Occupied { id, .. } => id,
        }
    }

    /// The `nonce` property.
    pub fn nonce(&self) -> &str {
        match self {
            Self::Probe { nonce, .. } | Self::Occupied { nonce, .. } => nonce,
        }
    }
}

/// What the host does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimStep {
    /// Request lock `name` exclusively with `ifAvailable`, keep a granted
    /// callback pending for the document lifetime, and report the answer to
    /// [`TabIdentity::lock_answered`].
    RequestLock { name: String },
    /// Open the claim channel if it is not open, arm a
    /// [`BROADCAST_PROBE_WAIT_MS`] timer that reports
    /// [`TabIdentity::probe_timed_out`] with this nonce, and post `message`.
    /// A channel that will not open or post reports
    /// [`TabIdentity::probe_unavailable`].
    Probe { message: ClaimMessage },
    /// The document owns `id`. `keep_channel` is whether the claim channel
    /// must stay open to answer later probes; when false the host closes it.
    Claimed { id: String, keep_channel: bool },
}

/// What one received claim message asks of the host.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageOutcome {
    /// A message to post back on the channel.
    pub reply: Option<ClaimMessage>,
    /// The step that follows a probe this message settled.
    pub step: Option<ClaimStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Unclaimed,
    Locking,
    Probing { id: String, nonce: String },
    Claimed,
}

/// The document's tab identity and its claim, one per document.
pub struct TabIdentity {
    storage: Rc<dyn KeyValueStore>,
    random: Rc<dyn RandomSource>,
    clock: Rc<dyn Clock>,
    cached: Option<String>,
    stage: Stage,
    broadcast_channel: bool,
    broadcast_claimed: Option<String>,
}

impl std::fmt::Debug for TabIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TabIdentity")
            .field("cached", &self.cached)
            .field("stage", &self.stage)
            .field("broadcast_claimed", &self.broadcast_claimed)
            .finish_non_exhaustive()
    }
}

impl TabIdentity {
    /// An unclaimed identity over the document's `sessionStorage`, minting from
    /// `random`. `clock` only disambiguates a mint that repeated the id it
    /// replaced.
    pub fn new(
        storage: Rc<dyn KeyValueStore>,
        random: Rc<dyn RandomSource>,
        clock: Rc<dyn Clock>,
    ) -> Self {
        Self {
            storage,
            random,
            clock,
            cached: None,
            stage: Stage::Unclaimed,
            broadcast_channel: false,
            broadcast_claimed: None,
        }
    }

    /// The document's current id: the cached one, else the stored one, else a
    /// fresh one written to storage (v2 `getTabId`). Unclaimed until
    /// [`ClaimStep::Claimed`].
    pub fn tab_id(&mut self) -> String {
        if let Some(cached) = &self.cached {
            return cached.clone();
        }
        if let Some(stored) = self.storage.get(TAB_ID_KEY)
            && !stored.is_empty()
        {
            self.cached = Some(stored.clone());
            return stored;
        }
        let fresh = self.mint();
        // Cached first so a re-entrant storage observes one document id.
        self.cached = Some(fresh.clone());
        self.storage.set(TAB_ID_KEY, &fresh);
        fresh
    }

    /// Start the claim (v2 `claimCurrentTabIdentity`): Web Locks first, the
    /// broadcast probe when locks are absent or refuse, else keep the stored
    /// id. Called once per document.
    pub fn begin_claim(&mut self, primitives: ArbitrationPrimitives) -> ClaimStep {
        self.broadcast_channel = primitives.broadcast_channel;
        if primitives.web_locks {
            self.stage = Stage::Locking;
            let id = self.tab_id();
            return ClaimStep::RequestLock {
                name: lock_name(&id),
            };
        }
        self.start_broadcast()
    }

    /// The answer to [`ClaimStep::RequestLock`]; `None` when no lock was asked.
    pub fn lock_answered(&mut self, attempt: LockAttempt) -> Option<ClaimStep> {
        if self.stage != Stage::Locking {
            return None;
        }
        Some(match attempt {
            LockAttempt::Acquired => self.claimed(false),
            LockAttempt::Unavailable => self.start_broadcast(),
            LockAttempt::Occupied => {
                let previous = self.tab_id();
                let fresh = self.rotate(&previous, Arbitration::WebLocks);
                ClaimStep::RequestLock {
                    name: lock_name(&fresh),
                }
            }
        })
    }

    /// The probe carrying `nonce` saw no owner within the wait: claim its id.
    /// `None` for a probe that already settled.
    pub fn probe_timed_out(&mut self, nonce: &str) -> Option<ClaimStep> {
        let Stage::Probing { nonce: pending, .. } = &self.stage else {
            return None;
        };
        (pending == nonce).then(|| self.settle(BroadcastAttempt::Acquired))
    }

    /// The channel would not open or post: keep the id, close the channel.
    pub fn probe_unavailable(&mut self) -> Option<ClaimStep> {
        matches!(self.stage, Stage::Probing { .. })
            .then(|| self.settle(BroadcastAttempt::Unavailable))
    }

    /// A message another document posted on the claim channel.
    ///
    /// A probe for the id this document claimed is answered `occupied`. A probe
    /// for the id this document is still probing proves a second pending
    /// document exists (a channel never delivers a sender its own message), so
    /// this one yields; if probes cross, both yield and re-mint independently.
    pub fn message_received(&mut self, message: ClaimMessage) -> MessageOutcome {
        let pending = match &self.stage {
            Stage::Probing { id, nonce } => Some((id.as_str(), nonce.as_str())),
            _ => None,
        };
        match message {
            ClaimMessage::Probe { id, nonce } => {
                if self.broadcast_claimed.as_deref() == Some(id.as_str()) {
                    return MessageOutcome {
                        reply: Some(ClaimMessage::Occupied { id, nonce }),
                        step: None,
                    };
                }
                if pending.is_some_and(|(pending_id, _)| pending_id == id) {
                    return self.yield_probe();
                }
            }
            ClaimMessage::Occupied { id, nonce } => {
                if pending == Some((id.as_str(), nonce.as_str())) {
                    return self.yield_probe();
                }
            }
        }
        MessageOutcome::default()
    }

    fn yield_probe(&mut self) -> MessageOutcome {
        MessageOutcome {
            reply: None,
            step: Some(self.settle(BroadcastAttempt::Occupied)),
        }
    }

    fn start_broadcast(&mut self) -> ClaimStep {
        if !self.broadcast_channel {
            // Neither primitive: intentionally keep the sessionStorage id.
            return self.claimed(false);
        }
        let id = self.tab_id();
        let nonce = self.mint();
        self.stage = Stage::Probing {
            id: id.clone(),
            nonce: nonce.clone(),
        };
        ClaimStep::Probe {
            message: ClaimMessage::Probe { id, nonce },
        }
    }

    fn settle(&mut self, result: BroadcastAttempt) -> ClaimStep {
        let id = self.tab_id();
        match result {
            BroadcastAttempt::Acquired => {
                self.broadcast_claimed = Some(id);
                self.claimed(true)
            }
            BroadcastAttempt::Unavailable => {
                self.broadcast_claimed = None;
                self.claimed(false)
            }
            BroadcastAttempt::Occupied => {
                self.rotate(&id, Arbitration::BroadcastChannel);
                self.start_broadcast()
            }
        }
    }

    fn claimed(&mut self, keep_channel: bool) -> ClaimStep {
        self.stage = Stage::Claimed;
        let id = self.tab_id();
        tracing::info!(target: "auth", tab8 = prefix8(&id), keep_channel, "tab identity claimed");
        ClaimStep::Claimed { id, keep_channel }
    }

    /// Replace a conflicting id in one step: readers of this document switch at
    /// once and storage follows, and a storage failure cannot put the document
    /// back on the conflicting value.
    fn rotate(&mut self, previous: &str, arbitration: Arbitration) -> String {
        let mut fresh = self.mint();
        if fresh == previous {
            // A broken random source must not leave the known-conflicting id.
            fresh = format!("{fresh}-{}", base36(self.clock.now_ms()));
        }
        self.cached = Some(fresh.clone());
        self.storage.set(TAB_ID_KEY, &fresh);
        tracing::warn!(
            target: "auth",
            signal = "tab.duplicate_identity_rotated",
            arbitration = arbitration.as_str(),
            previous8 = prefix8(previous),
            next8 = prefix8(&fresh),
            "duplicated tab identity rotated"
        );
        fresh
    }

    /// A v4 UUID, the shape of v2's `crypto.randomUUID()`.
    fn mint(&self) -> String {
        let mut bytes = [0_u8; 16];
        if let Err(error) = self.random.fill_bytes(&mut bytes) {
            tracing::warn!(target: "auth", %error, "tab id entropy unavailable; minting from a partial draw");
        }
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

fn lock_name(id: &str) -> String {
    format!("{TAB_ID_LOCK_PREFIX}{id}")
}

fn prefix8(id: &str) -> String {
    id.chars().take(8).collect()
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.iter().rev().map(|&digit| char::from(digit)).collect()
}
