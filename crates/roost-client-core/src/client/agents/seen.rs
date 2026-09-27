//! Browser-profile acknowledgement state for exact coding-agent occupants.
//!
//! A revision advances only within one epoch/occupant pair, so this ledger is
//! keyed by BOTH: acknowledging the agent that just finished must not
//! acknowledge the replacement that took its process. Every merge is a maximum
//! per identity, so another tab's acknowledgement survives this one's write.
//!
//! The SOURCE is stored but is NOT part of the key: the same occupant reported
//! by the integration and by the screen is one occupant, and two sources
//! acknowledging it must not be two buckets.
//!
//! Ported from `apps/web/src/lib/agentSeen.ts`. Storage is a host concern — the
//! ledger encodes to a line format and decodes from one, and
//! `platform::KeyValueStore` holds the string. Depends on `status_policy` for
//! the token and `roost_protocol::wire` for the brands.

use roost_protocol::wire::agent_status::agent_status_identity;
use roost_protocol::wire::{
    AgentOccupantId, AgentStatus, AgentStatusIdentity, AgentStatusSource, SessionId, StatusEpoch,
};

use crate::client::agents::status_policy::{LEGACY_IDENTITY_KEY, AgentStatusRevisionToken};

/// Where a host persists the ledger. One key, so a second tab finds it.
pub const AGENT_SEEN_STORAGE_KEY: &str = "roost.agentSeen.v2";

/// Field separator inside one record. Not a printable character, and a session
/// id, an epoch and an occupant are all UUIDs, so no field can contain it.
const FIELD: char = '\u{1f}';

/// One record's field count. Fixed for every token, legacy included: a legacy
/// token writes three empty identity fields rather than a shorter record, so a
/// truncated write is a parse failure and never a silently shorter token.
const FIELDS: usize = 5;

/// What one occupant of one session has had acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Acknowledged {
    revision: i64,
    /// Empty for a legacy status, which has no occupant to name.
    epoch: String,
    occupant: String,
    /// Empty for a legacy status.
    source: Option<AgentStatusSource>,
}

impl Acknowledged {
    fn from_token(token: &AgentStatusRevisionToken) -> Self {
        match &token.identity {
            Some(identity) => Self {
                revision: token.revision,
                epoch: identity.status_epoch.as_str().to_owned(),
                occupant: identity.occupant_id.as_str().to_owned(),
                source: Some(identity.source),
            },
            None => Self {
                revision: token.revision,
                epoch: String::new(),
                occupant: String::new(),
                source: None,
            },
        }
    }

    fn to_token(&self, session_id: &SessionId) -> AgentStatusRevisionToken {
        let identity = match (&self.epoch, &self.occupant, self.source) {
            (epoch, occupant, Some(source)) => {
                match (
                    StatusEpoch::try_from(epoch.as_str()),
                    AgentOccupantId::try_from(occupant.as_str()),
                ) {
                    (Ok(status_epoch), Ok(occupant_id)) => Some(AgentStatusIdentity {
                        status_epoch,
                        occupant_id,
                        source,
                    }),
                    // A record that was written from a valid token decodes back
                    // to a valid one; a hand-edited one does not, and an
                    // identity that cannot be rebuilt is a legacy token rather
                    // than a guessed occupant.
                    _ => None,
                }
            }
            _ => None,
        };
        AgentStatusRevisionToken {
            session_id: session_id.clone(),
            revision: self.revision,
            identity,
        }
    }

    /// The bucket this occupant is acknowledged under.
    fn identity_key(&self) -> String {
        if self.epoch.is_empty() {
            LEGACY_IDENTITY_KEY.to_owned()
        } else {
            format!("{}:{}", self.epoch, self.occupant)
        }
    }
}

/// Acknowledged revisions, per session, per occupant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSeenLedger {
    /// `session -> (occupant key -> acknowledgement)`. Sorted, so the encoded
    /// form is a deterministic dump: two tabs that merged the same set write
    /// byte-identical storage, and a diff of that key is noise rather than a
    /// lost acknowledgement.
    by_session: BTreeMap<SessionId, BTreeMap<String, Acknowledged>>,
}

impl AgentSeenLedger {
    /// A ledger that has acknowledged nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a ledger from what a host has stored, or an empty one.
    ///
    /// A malformed record is DROPPED, never fatal: browser storage is not a
    /// place a viewer reads an error from, and dropping one acknowledgement
    /// costs at worst one extra "Done" that the next acknowledgement clears.
    #[must_use]
    pub fn decode(raw: Option<&str>) -> Self {
        let mut ledger = Self::new();
        let Some(raw) = raw else {
            return ledger;
        };
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(FIELD).collect();
            if parts.len() != FIELDS {
                continue;
            }
            let (Ok(session_id), Ok(revision)) = (SessionId::try_from(parts[0]), parts[1].parse())
            else {
                continue;
            };
            if revision < 0 {
                continue;
            }
            let record = Acknowledged {
                revision,
                epoch: parts[2].to_owned(),
                occupant: parts[3].to_owned(),
                source: source_from_str(parts[4]),
            };
            // A half-present identity is neither a legacy record nor a complete
            // one, so it is neither: `to_token` collapses it to legacy, which
            // acknowledges less than the row claims rather than more.
            ledger.merge(&[record.to_token(&session_id)]);
        }
        ledger
    }

    /// Encode for storage: one record per line, sorted.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for (session_id, occupants) in &self.by_session {
            for record in occupants.values() {
                let source = record
                    .source
                    .map_or(String::new(), |source| source.as_str().to_owned());
                lines.push(format!(
                    "{session_id}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{source}",
                    record.revision, record.epoch, record.occupant
                ));
            }
        }
        lines.join("\n")
    }

    /// Fold a set of tokens in, keeping the highest revision per identity.
    ///
    /// Returns whether anything moved, so a host can skip a write for a merge
    /// that was entirely older — the common case when a second tab has already
    /// acknowledged what this one just acknowledged.
    pub fn merge(&mut self, tokens: &[AgentStatusRevisionToken]) -> bool {
        let mut changed = false;
        for token in tokens {
            let record = Acknowledged::from_token(token);
            let occupants = self.by_session.entry(token.session_id.clone()).or_default();
            let key = record.identity_key();
            match occupants.get(&key) {
                Some(current) if current.revision >= record.revision => {}
                _ => {
                    occupants.insert(key, record);
                    changed = true;
                }
            }
        }
        changed
    }

    /// The highest revision this profile has acknowledged for a status's
    /// occupant.
    ///
    /// `-1` for an identified occupant nothing has acknowledged, `0` for a
    /// legacy status, and the difference is load-bearing: an identified
    /// occupant's first completion is genuinely unseen, while a legacy
    /// deployment cannot have completed anything this profile missed.
    #[must_use]
    pub fn acknowledged_revision(&self, status: &AgentStatus) -> i64 {
        self.by_session
            .get(&status.common.session_id)
            .and_then(|occupants| occupants.get(&occupant_key(status)))
            .map_or(default_acknowledged(status), |record| record.revision)
    }

    /// Acknowledge a status, returning whether this moved the ledger.
    ///
    /// A no-op at or below what is already acknowledged, so acknowledging twice
    /// neither rewrites storage nor passes over something newer.
    pub fn mark_seen(&mut self, status: &AgentStatus) -> bool {
        if status.common.revision <= self.acknowledged_revision(status) {
            return false;
        }
        self.merge(&[crate::client::agents::status_policy::agent_status_revision_token(
            status,
        )])
    }

    /// Forget every acknowledgement for one session.
    ///
    /// Called when a session is removed: an acknowledgement for a session that
    /// no longer exists is storage nobody will read again, and this ledger is
    /// the one structure in the agent surface a long-lived tab would otherwise
    /// grow forever.
    pub fn forget_session(&mut self, session_id: &SessionId) -> bool {
        self.by_session.remove(session_id).is_some()
    }

    /// How many sessions carry at least one acknowledgement.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_session.len()
    }

    /// Whether nothing has been acknowledged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_session.is_empty()
    }

    /// The acknowledged tokens, for a host that must merge another tab's write.
    #[must_use]
    pub fn tokens(&self) -> Vec<AgentStatusRevisionToken> {
        self.by_session
            .iter()
            .flat_map(|(session_id, occupants)| {
                occupants.values().map(|record| record.to_token(session_id))
            })
            .collect()
    }
}

fn occupant_key(status: &AgentStatus) -> String {
    match agent_status_identity(&status.common) {
        Some(identity) => format!(
            "{}:{}",
            identity.status_epoch.as_str(),
            identity.occupant_id.as_str()
        ),
        None => LEGACY_IDENTITY_KEY.to_owned(),
    }
}

fn default_acknowledged(status: &AgentStatus) -> i64 {
    if agent_status_identity(&status.common).is_some() {
        -1
    } else {
        0
    }
}

fn source_from_str(value: &str) -> Option<AgentStatusSource> {
    match value {
        "integration" => Some(AgentStatusSource::Integration),
        "screen" => Some(AgentStatusSource::Screen),
        _ => None,
    }
}
