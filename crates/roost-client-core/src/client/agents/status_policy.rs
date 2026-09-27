//! Browser coding-agent identity and presentation policy.
//!
//! Identity comparisons are `roost_protocol::wire::agent_status::order`'s, not
//! this file's: a fence restated here is the fence the two ends disagree about.
//! What this file owns is the DERIVED vocabulary every surface shares — the
//! level, the label, the priority, the copy — so a sidebar, a session row, a
//! toast and a title all say the same thing about one agent.
//!
//! Ported from `apps/web/src/client/agents/agentStatus.ts`. Depends on
//! `roost_protocol::wire` and adds no state.
use roost_protocol::wire::agent_status::agent_status_identity;
use roost_protocol::wire::{
    AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusIdentity, SessionId,
};

/// The bucket a status with no identity triple is acknowledged under.
pub const LEGACY_IDENTITY_KEY: &str = "legacy";

/// An exact (session, occupant, revision) acknowledgement.
///
/// Deliberately not a branded `AgentStatusIdentity`: a legacy status has no
/// identity, and a type that can hold "no identity" is what lets the
/// acknowledgement ledger key a legacy row without inventing an occupant for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatusRevisionToken {
    /// Which session's occupant this acknowledges.
    pub session_id: SessionId,
    /// The highest revision this profile has acknowledged for that occupant.
    pub revision: i64,
    /// Absent exactly when the status came from a legacy deployment.
    pub identity: Option<AgentStatusIdentity>,
}

impl AgentStatusRevisionToken {
    /// The storage key for this token's occupant: `epoch:occupant`, or the one
    /// legacy bucket.
    #[must_use]
    pub fn identity_key(&self) -> String {
        match &self.identity {
            Some(identity) => format!(
                "{}:{}",
                identity.status_epoch.as_str(),
                identity.occupant_id.as_str()
            ),
            None => LEGACY_IDENTITY_KEY.to_owned(),
        }
    }
}

/// The stable key of a status's occupant, or `None` for a legacy status.
#[must_use]
pub fn agent_status_occupant_key(status: &AgentStatusFields) -> Option<String> {
    agent_status_identity(status).map(|identity| {
        format!(
            "{}:{}",
            identity.status_epoch.as_str(),
            identity.occupant_id.as_str()
        )
    })
}

/// The token a status acknowledges, whether or not it carries an identity.
#[must_use]
pub fn agent_status_revision_token(status: &AgentStatus) -> AgentStatusRevisionToken {
    AgentStatusRevisionToken {
        session_id: status.common.session_id.clone(),
        revision: status.common.revision,
        identity: agent_status_identity(&status.common),
    }
}

/// Whether a status is the exact revision some earlier reader captured.
///
/// The token carries the epoch and occupant, not just the revision, because a
/// replacement occupant numbers its own revisions from 1: a bare revision would
/// match the previous agent's first report and re-show a completion the new
/// agent never earned.
#[must_use]
pub fn matches_agent_status_revision_token(
    status: &AgentStatus,
    token: &AgentStatusRevisionToken,
) -> bool {
    status.common.session_id == token.session_id
        && status.common.revision == token.revision
        && same_agent_identity_occupant(
            agent_status_identity(&status.common).as_ref(),
            token.identity.as_ref(),
        )
}

/// What a viewer is told an agent is doing, once acknowledgement is applied.
///
/// `Default` is `Unknown`, which is also the level an EMPTY rollup reports: a
/// count of nothing is not a count of idle sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum AgentStatusLevel {
    /// Waiting for a human.
    Blocked,
    /// Finished, and this profile had not seen it finish.
    Done,
    /// Running.
    Working,
    /// Present and not running.
    Idle,
    /// No status at all, or a state this build does not know.
    #[default]
    Unknown,
}

/// The stable spelling of a level, for a store that projects a string rather
/// than this enum.
///
/// A plain lower-case word, so a projection that cannot name a Rust variant
/// still says the same thing a projection that can.
#[must_use]
pub const fn agent_status_level_token(level: AgentStatusLevel) -> &'static str {
    match level {
        AgentStatusLevel::Blocked => "blocked",
        AgentStatusLevel::Done => "done",
        AgentStatusLevel::Working => "working",
        AgentStatusLevel::Idle => "idle",
        AgentStatusLevel::Unknown => "unknown",
    }
}

/// How one level reads. Every field is a token or a literal, never a raw colour:
/// the web host resolves `color` against the design system, so a second palette
/// cannot appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStatusPresentation {
    /// The word a row shows.
    pub label: &'static str,
    /// The plural noun a rollup counts, so "1 needs input" reads as English.
    pub count_label: &'static str,
    /// The sentence a hover explains.
    pub tooltip: &'static str,
    /// A design-system colour role.
    pub color: &'static str,
    /// Attention order, highest first. `Done` outranks `Working` because a
    /// finished agent is the one thing a viewer opened the tab to find.
    pub priority: u8,
    /// The `StatusDot` status a chip shows.
    pub dot_status: AgentDotStatus,
}

/// The dot a `StatusDot` primitive takes. Named here so no host invents its own
/// mapping and a fourth surface cannot disagree with the other three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDotStatus {
    /// Needs attention.
    Warn,
    /// Finished.
    Ok,
    /// Running.
    Info,
    /// Present, nothing to say.
    Idle,
}

/// The one presentation table, keyed by level. A `match` and not a lookup
/// table: the arms are checked at compile time, so a sixth level cannot ship
/// without a presentation.
#[must_use]
pub const fn agent_status_presentation(level: AgentStatusLevel) -> AgentStatusPresentation {
    match level {
        AgentStatusLevel::Blocked => AgentStatusPresentation {
            label: "Needs input",
            count_label: "needs input",
            tooltip: "The agent is waiting for your input",
            color: "var(--md-warning)",
            priority: 4,
            dot_status: AgentDotStatus::Warn,
        },
        AgentStatusLevel::Done => AgentStatusPresentation {
            label: "Done",
            count_label: "done",
            tooltip: "The agent finished since you last viewed this terminal",
            color: "var(--md-secondary)",
            priority: 3,
            dot_status: AgentDotStatus::Ok,
        },
        AgentStatusLevel::Working => AgentStatusPresentation {
            label: "Working",
            count_label: "working",
            tooltip: "The agent is working",
            color: "var(--md-primary)",
            priority: 2,
            dot_status: AgentDotStatus::Info,
        },
        AgentStatusLevel::Idle => AgentStatusPresentation {
            label: "Idle",
            count_label: "idle",
            tooltip: "The agent is idle",
            color: "var(--md-success)",
            priority: 1,
            dot_status: AgentDotStatus::Idle,
        },
        AgentStatusLevel::Unknown => AgentStatusPresentation {
            label: "Unknown",
            count_label: "unknown",
            tooltip: "Agent status is unavailable",
            color: "var(--md-on-surface-variant)",
            priority: 0,
            dot_status: AgentDotStatus::Idle,
        },
    }
}

/// The level a status reads as, given what this profile has already acknowledged.
///
/// Idle becomes Done only while a real completion revision remains unseen, and
/// an identified occupant's first completion reads `done` while a legacy
/// status's first completion does not — because an identified occupant's
/// completions are the only ones this profile can have missed.
#[must_use]
pub fn derive_agent_status_level(
    status: Option<&AgentStatus>,
    acknowledged_revision: Option<i64>,
) -> AgentStatusLevel {
    let Some(status) = status else {
        return AgentStatusLevel::Unknown;
    };
    if status.common.state == AgentRuntimeState::Blocked {
        return AgentStatusLevel::Blocked;
    }
    if status.common.state == AgentRuntimeState::Working {
        return AgentStatusLevel::Working;
    }
    let seen = acknowledged_revision.unwrap_or_else(|| {
        if agent_status_occupant_key(&status.common).is_none() {
            0
        } else {
            -1
        }
    });
    if status.common.completed_revision > 0 && status.common.completed_revision > seen {
        return AgentStatusLevel::Done;
    }
    AgentStatusLevel::Idle
}

/// The level a status reads as, as a stable lower-case token, for a projection
/// that carries a string rather than this crate's enum.
#[must_use]
pub fn agent_status_level_token_for(
    status: Option<&AgentStatus>,
    acknowledged_revision: Option<i64>,
) -> &'static str {
    agent_status_level_token(derive_agent_status_level(
        status,
        acknowledged_revision,
    ))
}

/// Whether a status carries a completion this profile has not acknowledged.
///
/// The one predicate the "unseen" flag on a projected row must be built from:
/// a row that shows an agent as busy while its completion is unseen is the
/// small wrongness that makes a list stop being trusted.
#[must_use]
pub fn agent_status_completion_unseen(
    status: &AgentStatus,
    acknowledged_revision: Option<i64>,
) -> bool {
    derive_agent_status_level(Some(status), acknowledged_revision) == AgentStatusLevel::Done
}

/// The sentence a hover shows: the level's own words, plus the agent's message
/// when it sent one.
#[must_use]
pub fn agent_status_tooltip(status: &AgentStatus, acknowledged_revision: Option<i64>) -> String {
    let presentation = agent_status_presentation(derive_agent_status_level(
        Some(status),
        acknowledged_revision,
    ));
    match status.common.message.as_deref().map(str::trim) {
        Some(message) if !message.is_empty() => format!("{}: {message}", presentation.tooltip),
        _ => presentation.tooltip.to_owned(),
    }
}

/// How many sessions read at each level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentStatusCounts {
    /// Sessions waiting for a human.
    pub blocked: usize,
    /// Sessions finished and unacknowledged.
    pub done: usize,
    /// Sessions running.
    pub working: usize,
    /// Sessions present and idle.
    pub idle: usize,
    /// Sessions with no status.
    pub unknown: usize,
}

impl AgentStatusCounts {
    /// One count for one level.
    #[must_use]
    pub const fn get(&self, level: AgentStatusLevel) -> usize {
        match level {
            AgentStatusLevel::Blocked => self.blocked,
            AgentStatusLevel::Done => self.done,
            AgentStatusLevel::Working => self.working,
            AgentStatusLevel::Idle => self.idle,
            AgentStatusLevel::Unknown => self.unknown,
        }
    }
}

/// What a fleet-wide attention chip shows: the highest level present, its
/// counts, and how many sessions carry a level at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentStatusRollup {
    /// The highest-priority level present, or `Unknown` for an empty set.
    pub level: AgentStatusLevel,
    /// The per-level counts.
    pub counts: AgentStatusCounts,
    /// How many sessions carry a level other than `Unknown`.
    pub total: usize,
}

/// Fold a set of levels into one chip's worth of state.
#[must_use]
pub fn fold_agent_status_levels(
    levels: impl IntoIterator<Item = AgentStatusLevel>,
) -> AgentStatusRollup {
    let mut counts = AgentStatusCounts::default();
    let mut level = AgentStatusLevel::Unknown;
    let mut total = 0;
    for candidate in levels {
        match candidate {
            AgentStatusLevel::Blocked => counts.blocked += 1,
            AgentStatusLevel::Done => counts.done += 1,
            AgentStatusLevel::Working => counts.working += 1,
            AgentStatusLevel::Idle => counts.idle += 1,
            AgentStatusLevel::Unknown => counts.unknown += 1,
        }
        if candidate != AgentStatusLevel::Unknown {
            total += 1;
        }
        if agent_status_presentation(candidate).priority > agent_status_presentation(level).priority {
            level = candidate;
        }
    }
    AgentStatusRollup {
        level,
        counts,
        total,
    }
}

/// The counts as reader-facing text, highest attention first.
#[must_use]
pub fn format_agent_status_counts(counts: &AgentStatusCounts) -> String {
    [
        AgentStatusLevel::Blocked,
        AgentStatusLevel::Working,
        AgentStatusLevel::Done,
        AgentStatusLevel::Idle,
    ]
    .into_iter()
    .filter(|level| counts.get(*level) > 0)
    .map(|level| {
        format!(
            "{} {}",
            counts.get(level),
            agent_status_presentation(level).count_label
        )
    })
    .collect::<Vec<String>>()
    .join(" · ")
}
