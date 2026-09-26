//! Agent occupancy: the per-process candidates a session offers, the occupant
//! published from them, and what happens when a candidate goes away. Owned by
//! the worker.
//!
//! Process identifiers never leave this module. They are how a candidate is
//! recognised as the SAME process, and publishing one would make a worker's
//! pid layout readable by every browser attached to it.
//!
//! Three rules carry the whole model, and each is a place where the obvious
//! implementation is wrong.
//!
//! AN UNINTERRUPTED (kind, pid) INCARNATION OWNS ONE OCCUPANT. Restart the
//! same agent and it is a new occupant even though the pid may be identical —
//! a new incarnation is a new thing, and merging them would show a restart as
//! one continuous run.
//!
//! A DEAD OCCUPANT IS NOT RECLAIMABLE BY THE SAME NUMERIC PID. Process ids are
//! recycled, and a retired occupant resurrected by a new process that happens
//! to draw the same number would be a stale completion attributed to an agent
//! that never ran. This is the single most important rule here.
//!
//! AN EXIT IS NOT A WITHDRAWAL. An agent that finishes and then leaves is still
//! DONE, and its completion is held until a viewer acknowledges it — otherwise
//! a viewer arriving a moment later sees nothing and the run looks like it
//! never happened. An integration's explicit `active: false` is the withdrawal
//! verb, and it retires the row outright. The two are not interchangeable.

use std::collections::HashMap;

/// Where a status observation came from, and what it is allowed to prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// The agent's own integration reporting over its own protocol. For a
    /// full-lifecycle agent this is the whole truth and no screen signal may
    /// correct it.
    Integration,
    /// A screen observation. An on-screen blocker prompt is direct evidence a
    /// human is being waited on, so it outranks an integration that is not a
    /// full-lifecycle authority.
    Screen,
}

/// An agent's runtime state, as reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeState {
    Working,
    /// Waiting on a human. The state that matters most: a session sitting on a
    /// prompt is a session the user has to answer.
    Blocked,
    Idle,
}

impl RuntimeState {
    /// The state a state forces when it is WITHDRAWN.
    ///
    /// Idle, and not a disappearance: the agent is done, which is a fact about
    /// it, and a viewer must be able to see that fact after the process is
    /// gone. Inventing an `unknown` would lose the only thing worth keeping.
    pub fn forced_idle(self) -> Self {
        let _ = self;
        RuntimeState::Idle
    }
}

/// One process a session believes is an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub state: RuntimeState,
    /// A message from the agent, if it gave one.
    pub message: Option<String>,
    /// Whether a screen observation matched a rule marked `visible_blocker`.
    ///
    /// Direct evidence a human is being waited on, and it may CORRECT an
    /// integration that is not a full-lifecycle state authority.
    pub visible_blocker: bool,
}

/// The identity of an agent process: its kind and its process id.
///
/// The pair is the key because the same pid can be a different agent after a
/// restart, and the same agent can move to a different pid.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessKey {
    pub agent_kind: String,
    pub process_id: u32,
}

impl ProcessKey {
    pub fn new(agent_kind: impl Into<String>, process_id: u32) -> Self {
        Self {
            agent_kind: agent_kind.into(),
            process_id,
        }
    }
}

/// One occupant token: one uninterrupted incarnation of one agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    pub key: ProcessKey,
    /// Opaque, worker-private identity. Never the process id, which is
    /// exactly why the token exists.
    pub occupant_id: u64,
    pub state: RuntimeState,
    pub message: Option<String>,
    pub source: Source,
    /// Bumped on every published change, so a viewer can order what it sees.
    pub revision: u64,
    /// The revision at which this occupant finished.
    pub completed_revision: u64,
    /// False once the occupant's last candidate disappeared. A dead occupant
    /// can neither back a prompt proof nor be reclaimed.
    pub live: bool,
}

impl Occupant {
    /// Whether a viewer still has to acknowledge this occupant finishing.
    ///
    /// True for a finished occupant nobody has acknowledged. False once a
    /// viewer has seen the completion, or while the occupant is still running —
    /// a running agent has not finished anything.
    pub fn awaits_acknowledgement(&self) -> bool {
        !self.live && self.completed_revision != 0
    }
}

/// What happened to a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loss {
    /// The process went away. The occupant keeps its completion, forced idle,
    /// until a viewer acknowledges it.
    Exited,
    /// An integration explicitly reported `active: false`. This is the
    /// WITHDRAWAL verb, and it retires the row outright rather than leaving a
    /// completion nobody will ever come to read.
    Withdrawn,
}

/// One session's agent observations.
#[derive(Debug, Default)]
pub struct Occupancy {
    occupants: HashMap<ProcessKey, Occupant>,
    /// Keys retired by a withdrawal. A later sighting of the same numeric pid
    /// is a NEW occupant rather than a resurrection, and this is where that is
    /// remembered — because process ids are recycled and the number alone says
    /// nothing about which process holds it.
    retired: Vec<ProcessKey>,
    next_occupant_id: u64,
    revision: u64,
}

impl Occupancy {
    pub fn new() -> Self {
        Self {
            occupants: HashMap::new(),
            retired: Vec::new(),
            next_occupant_id: 1,
            revision: 0,
        }
    }

    /// The occupants this session currently publishes, in a stable order.
    pub fn occupants(&self) -> Vec<&Occupant> {
        let mut all: Vec<&Occupant> = self.occupants.values().collect();
        all.sort_by_key(|occupant| occupant.occupant_id);
        all
    }

    pub fn get(&self, key: &ProcessKey) -> Option<&Occupant> {
        self.occupants.get(key)
    }

    /// Offer a candidate for a process.
    ///
    /// A key that was WITHDRAWN gets a new occupant rather than resurrecting
    /// the old one, because the same numeric pid after a withdrawal is a
    /// different process. A key that merely EXITED keeps its occupant, so an
    /// agent that finished and left is still reported as done.
    pub fn observe(&mut self, key: ProcessKey, source: Source, candidate: Candidate) -> u64 {
        if !self.occupants.contains_key(&key) && self.retired.contains(&key) {
            // A withdrawn incarnation is never revived by a matching pid. The
            // key is cleared so the NEXT sighting is a genuinely new occupant.
            self.retired.retain(|retired| retired != &key);
        }

        let occupant = self.occupants.entry(key.clone()).or_insert_with(|| {
            let occupant_id = self.next_occupant_id;
            self.next_occupant_id += 1;
            Occupant {
                key: key.clone(),
                occupant_id,
                state: RuntimeState::Idle,
                message: None,
                source,
                revision: 0,
                completed_revision: 0,
                live: true,
            }
        });

        // A dead occupant is not brought back to life by a new observation of
        // the same pid; it is a different process wearing the same number.
        if !occupant.live && source == Source::Screen {
            return self.revision;
        }

        // What a source is ALLOWED to prove.
        //
        // A visible blocker prompt corrects an integration that is not a
        // full-lifecycle authority: a prompt on screen is direct evidence a
        // human is being waited on. Every other screen observation proves
        // IDENTITY and ACTIVITY only — that there is a screen for this process
        // at all — and must not move the state, or a terminal that merely has
        // text on it would report an agent as working. An integration's own
        // report is not correctable by any screen signal: for a full-lifecycle
        // agent it IS the whole truth.
        let state = if source == Source::Screen {
            if candidate.visible_blocker {
                RuntimeState::Blocked
            } else {
                occupant.state
            }
        } else {
            candidate.state
        };
        let message = candidate.message.or_else(|| occupant.message.clone());

        if occupant.state != state || occupant.message != message {
            occupant.state = state;
            occupant.message = message;
            occupant.source = source;
            self.revision += 1;
            occupant.revision = self.revision;
        }
        occupant.live = true;
        self.revision
    }

    /// A candidate went away.
    ///
    /// An exit holds the completion; a withdrawal retires the row. The
    /// difference is the whole point of having two verbs.
    pub fn lose(&mut self, key: &ProcessKey, loss: Loss) -> Option<&Occupant> {
        let occupant = self.occupants.get_mut(key)?;
        match loss {
            Loss::Exited => {
                // Still DONE, and still owed to a viewer. Forced idle rather
                // than a disappearance, because a viewer arriving a moment
                // later must see that the agent finished.
                if occupant.state != RuntimeState::Idle || occupant.completed_revision == 0 {
                    occupant.state = occupant.state.forced_idle();
                    self.revision += 1;
                    occupant.revision = self.revision;
                    occupant.completed_revision = self.revision;
                }
                occupant.live = false;
                self.occupants.get(key)
            }
            Loss::Withdrawn => {
                // The integration said so outright. Retiring leaves nothing to
                // acknowledge, which is correct: nobody will come to read a
                // completion the agent itself withdrew.
                self.occupants.remove(key);
                if !self.retired.contains(key) {
                    self.retired.push(key.clone());
                }
                None
            }
        }
    }

    /// A viewer has seen the completion. This is what releases a dead
    /// occupant.
    pub fn acknowledge(&mut self, key: &ProcessKey) -> Option<&Occupant> {
        let occupant = self.occupants.get_mut(key)?;
        if !occupant.live {
            self.occupants.remove(key);
            return None;
        }
        None
    }

    /// Dead occupants still waiting for a viewer to see their completion.
    pub fn awaiting_acknowledgement(&self) -> Vec<&Occupant> {
        self.occupants
            .values()
            .filter(|o| o.awaits_acknowledgement())
            .collect()
    }
}
