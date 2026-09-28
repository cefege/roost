//! The identity and the shape of a toast card.
//!
//! Split from the mutations because identity is the RULE and the mutations are
//! what it rules: `ToastId` is what makes "the same event produced two cards"
//! unrepresentable, and it changes for a different reason than a dismissal
//! deadline does. Nothing here mutates anything.

/// How a card reads, and how long it gets before it removes itself.
///
/// Errors get the longest window and still expire: the card's Copy button is
/// the durable path to the text, and a sticky `ttl: None` is the explicit
/// opt-out for the rare must-not-vanish notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToastKind {
    /// A completed action.
    Ok,
    /// Something the user should look at but need not act on.
    Warn,
    /// A failure, with the detail a user may need to quote.
    Err,
}

impl ToastKind {
    /// The wire spelling, for a host that renders the kind as a data attribute.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Err => "err",
        }
    }

    /// The default lifetime in milliseconds, or `None` to stay until dismissed.
    pub const fn default_ttl_ms(self) -> Option<u64> {
        match self {
            Self::Ok => Some(3_000),
            Self::Warn => Some(5_000),
            Self::Err => Some(8_000),
        }
    }
}

/// The event a toast was raised by.
///
/// Every variant carries the event's OWN discriminator, so an id cannot be built
/// by counting: a Sync toast cannot exist without the delivery sequence the
/// socket assigned its frame, and a Connect toast cannot exist without the call
/// id this store minted for the request it answers. That is what makes "the
/// same event produced two cards" unrepresentable rather than merely avoided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToastSource {
    /// A Sync frame: the domain it belongs to, its kind, and the cumulative
    /// delivery sequence the socket gave it. A frame redelivered after a
    /// reconnect keeps its sequence, so it re-raises the same id.
    Sync {
        /// The Sync domain the frame belonged to, as `SyncDomain`'s name.
        domain: &'static str,
        /// The frame's kind name, as `SyncFrame::kind_name` spells it.
        kind: &'static str,
        /// The cumulative delivery sequence, zero for a control frame.
        delivery_seq: u64,
    },
    /// A Connect answer, correlated by the call id `Store::next_call_id` minted.
    Rpc {
        /// The call this answer settles.
        call_id: u64,
    },
    /// An optimistic spawn's settlement, correlated by the attempt the ledger
    /// minted for it. A superseded attempt's settlement never becomes a toast,
    /// because [`crate::store::optimistic_spawn::settle_spawn`] refuses it
    /// before it reaches here.
    Spawn {
        /// The attempt the ledger minted when the spawn began.
        attempt: u64,
    },
    /// A fact the host itself observed — a preference write, a paste the user
    /// performed, a capture the user asked for. There is no frame to take a
    /// discriminator from, so the SUBJECT must be the identity of the fact: two
    /// host facts that want the same card share one id and replace each other,
    /// which is the safe reading when a host cannot name them apart.
    Host {
        /// What observed it, e.g. `"capture"`.
        name: &'static str,
    },
}

/// One card's identity: the event, and what it is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ToastId {
    /// The event that raised it.
    pub source: ToastSource,
    /// What the card is about — a session id, a worker fingerprint, a relay id.
    /// Empty only for a subject-less host fact, which is then keyed by source
    /// alone.
    pub subject: String,
}

impl ToastId {
    /// The id of a card about `subject` raised by `source`.
    pub fn new(source: ToastSource, subject: impl Into<String>) -> Self {
        Self {
            source,
            subject: subject.into(),
        }
    }

    /// Whether this card is about `session_id`, directly or by its action.
    pub fn names_session(&self, session_id: &str) -> bool {
        self.subject == session_id
    }
}

/// What a card's button does.
///
/// Data, not a closure: a store that held a callback could not be `Debug`, and
/// a host would have to keep the closure's captures alive past the credential
/// that authorised them. [`take_toast_action`] hands the intent back to the host
/// and removes the card in the same step, which is the order
/// `AgentNotificationBridge.tsx:110-119` performs by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastIntent {
    /// Reveal a session: navigate to it and ring its surface.
    RevealSession {
        /// The session to reveal.
        session_id: String,
    },
}

/// The button a card offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastAction {
    /// The button's label.
    pub label: String,
    /// What pressing it does.
    pub intent: ToastIntent,
}

/// One card.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    /// Its identity, from the event that raised it.
    pub id: ToastId,
    /// The one-line message.
    pub msg: String,
    /// How it reads, and how long it lives by default.
    pub kind: ToastKind,
    /// A multi-line follow-up, rendered below the message in a selectable
    /// block.
    pub details: Option<String>,
    /// The inline button, when the card offers one.
    pub action: Option<ToastAction>,
    /// The session this card's action navigates to, so hovering it can ring
    /// that session's surface.
    pub target_session_id: Option<String>,
    /// The instant the card is removed, or `None` when it stays until it is
    /// dismissed by hand. While a card is HELD this is `None` and
    /// `remaining_ms` holds what was left.
    pub expires_at_ms: Option<u64>,
    /// How much of the card's window is left. Zero once it is due.
    pub remaining_ms: u64,
    /// Whether the pointer or focus is on the card and the window is frozen.
    pub held: bool,
}

impl Toast {
    /// Whether the card is due for removal at `now_ms`.
    pub fn is_due(&self, now_ms: u64) -> bool {
        !self.held
            && self
                .expires_at_ms
                .is_some_and(|deadline| now_ms >= deadline)
    }

    /// The instant the card would be removed, ignoring a hold. A held card
    /// reports `None`: it has no deadline until it is released.
    pub fn armed_deadline(&self) -> Option<u64> {
        if self.held { None } else { self.expires_at_ms }
    }
}

/// What a caller may add beyond the message and the kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToastOptions {
    /// A multi-line follow-up.
    pub details: Option<String>,
    /// The card's window, or `None` to keep it until dismissed. Absent means
    /// the kind's default.
    pub ttl_ms: Option<Option<u64>>,
    /// The inline button.
    pub action: Option<ToastAction>,
    /// The session the button navigates to.
    pub target_session_id: Option<String>,
}

impl ToastOptions {
    /// A card with nothing but its message.
    pub fn plain() -> Self {
        Self::default()
    }

    /// A card whose window is set explicitly. `None` keeps it until dismissed.
    pub fn with_ttl(ttl_ms: Option<u64>) -> Self {
        Self {
            ttl_ms: Some(ttl_ms),
            ..Self::default()
        }
    }

    /// Attach the multi-line follow-up.
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Attach the inline button and the session it reveals.
    pub fn with_action(mut self, label: impl Into<String>, session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        self.target_session_id = Some(session_id.clone());
        self.action = Some(ToastAction {
            label: label.into(),
            intent: ToastIntent::RevealSession { session_id },
        });
        self
    }
}
