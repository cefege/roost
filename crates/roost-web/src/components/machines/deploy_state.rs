//! The deploy dialog's own state machine: what it is showing, and which
//! operation owns the right to change it.
//!
//! Split from `deploy_dialog` so the rules that actually matter are provable
//! without a browser, a coordinator or a clock — a declared-but-unusable
//! address is a configuration error rather than a fallback, a second Generate
//! mints nothing, and a coordinator answer that arrives after the reader walked
//! away is dropped. Every operation is started with a generation and every
//! answer is fenced by it, which is the whole fence.

use super::enrollment_origin::EnrollmentDecision;

/// What the dialog is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployPhase {
    /// An identity read is in flight and nothing has been decided yet.
    Checking,
    /// Nothing is dialled from another machine, so the guide is the answer.
    LocalOnly,
    /// A remote origin is known and a join command can be minted.
    Ready,
    /// A grant is being minted. The action that started it owns the dialog
    /// until it lands, and no second one may start.
    Minting,
    /// The command is here and the reader can copy it.
    Generated,
    /// Something was refused; `error` says what, and the reader can re-check.
    Failed,
}

/// Why the dialog is refusing, and which reader action fits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployErrorKind {
    /// A declared enrollment address exists and cannot be dialled.
    Configuration,
    /// The coordinator would not answer, or would not mint.
    Coordinator,
}

impl DeployErrorKind {
    /// The value a reader — or a spec — reads the refusal by. It names the KIND
    /// rather than the wording, because the wording is what changes.
    pub const fn attribute(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Coordinator => "coordinator",
        }
    }
}

/// One refusal, with the words the reader reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployError {
    /// Which refusal this is.
    pub kind: DeployErrorKind,
    /// The headline.
    pub title: String,
    /// The sentence under it that says what to change.
    pub detail: String,
}

impl DeployError {
    /// The refusal for a declared address no other machine can dial.
    ///
    /// It names the declaration because "your Roost is not reachable" sends an
    /// operator looking at a proxy, while "you declared THIS" sends them
    /// looking at the setting that produced it.
    pub fn invalid_declaration(declared_url: &str) -> Self {
        Self {
            kind: DeployErrorKind::Configuration,
            title: "The configured enrollment address is invalid.".to_owned(),
            detail: format!(
                "This Roost declares {declared_url:?}, which a machine on another network cannot \
                 reach. Declare a reachable HTTPS address for it, then check again. Roost does \
                 not substitute an address of its own."
            ),
        }
    }

    /// The refusal for a coordinator that would not answer.
    pub fn coordinator(refused: &str) -> Self {
        Self {
            kind: DeployErrorKind::Coordinator,
            title: "Roost could not complete the request.".to_owned(),
            detail: refused.to_owned(),
        }
    }
}

/// The dialog's state, and the generation counter that fences late answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployModel {
    phase: DeployPhase,
    generation: u64,
    open: bool,
    coordinator_url: Option<String>,
    deploy_command: Option<String>,
    error: Option<DeployError>,
    copied: bool,
}

impl DeployModel {
    /// A dialog that has just been mounted: open, mid-check, nothing decided.
    pub fn opened() -> Self {
        Self {
            phase: DeployPhase::Checking,
            generation: 0,
            open: true,
            coordinator_url: None,
            deploy_command: None,
            error: None,
            copied: false,
        }
    }

    /// What this render draws.
    pub fn phase(&self) -> DeployPhase {
        self.phase
    }

    /// The origin a join command will send the worker to, once one is known.
    pub fn coordinator_url(&self) -> Option<&str> {
        self.coordinator_url.as_deref()
    }

    /// The minted command, once one exists.
    pub fn deploy_command(&self) -> Option<&str> {
        self.deploy_command.as_deref()
    }

    /// The refusal, when there is one.
    pub fn error(&self) -> Option<&DeployError> {
        self.error.as_ref()
    }

    /// Whether the last copy attempt was accepted.
    pub fn copied(&self) -> bool {
        self.copied
    }

    /// Whether the guide, rather than the enrollment form, is the answer.
    pub fn shows_local_access_guide(&self) -> bool {
        matches!(self.phase, DeployPhase::Checking | DeployPhase::LocalOnly)
    }

    /// The operation the dialog is currently waiting on. A caller that starts
    /// work outside the model — a clipboard write — fences its answer with this.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Start a fresh identity read and return the generation that owns it.
    ///
    /// Every read is a NEW generation, so "Check again" is a new question to the
    /// coordinator rather than a re-read of an answer this dialog already holds:
    /// the operator may have changed the declaration since the last one.
    pub fn begin_check(&mut self) -> u64 {
        self.generation += 1;
        self.phase = DeployPhase::Checking;
        self.coordinator_url = None;
        self.deploy_command = None;
        self.error = None;
        self.copied = false;
        self.generation
    }

    /// Start a mint, or `None` when there is nothing to mint or one is already
    /// in flight.
    ///
    /// This is the single answer to "can this press mint a grant": only a dialog
    /// that is Ready and not already minting may, so pressing Generate twice
    /// mints one grant rather than two.
    pub fn begin_mint(&mut self) -> Option<u64> {
        if self.phase != DeployPhase::Ready {
            return None;
        }
        self.generation += 1;
        self.phase = DeployPhase::Minting;
        self.error = None;
        self.copied = false;
        Some(self.generation)
    }

    /// Whether `generation` is still the operation this dialog is waiting on.
    pub fn owns(&self, generation: u64) -> bool {
        self.open && generation == self.generation
    }

    /// Apply an identity read, if it is still the one being waited on.
    pub fn accept_identity(&mut self, generation: u64, decision: EnrollmentDecision) -> bool {
        if !self.owns(generation) {
            return false;
        }
        self.copied = false;
        match decision {
            EnrollmentDecision::Ready { coordinator_url } => {
                self.coordinator_url = Some(coordinator_url);
                self.error = None;
                // An identity read inside a mint does not un-mint it: that mint
                // already re-checked the door, and handing the action back
                // mid-flight is exactly how a double press mints twice.
                if self.phase != DeployPhase::Minting {
                    self.phase = DeployPhase::Ready;
                }
            }
            EnrollmentDecision::LocalOnly => {
                self.coordinator_url = None;
                self.deploy_command = None;
                self.error = None;
                self.phase = DeployPhase::LocalOnly;
            }
            EnrollmentDecision::ConfigurationError { declared_url } => {
                self.coordinator_url = None;
                self.deploy_command = None;
                self.error = Some(DeployError::invalid_declaration(&declared_url));
                self.phase = DeployPhase::Failed;
            }
        }
        true
    }

    /// Record the minted command, if that mint still owns the dialog.
    pub fn accept_mint(&mut self, generation: u64, command: String) -> bool {
        if !self.owns(generation) {
            return false;
        }
        self.phase = DeployPhase::Generated;
        self.deploy_command = Some(command);
        self.error = None;
        true
    }

    /// Record a refusal, if the operation that hit it still owns the dialog.
    pub fn accept_failure(&mut self, generation: u64, error: DeployError) -> bool {
        if !self.owns(generation) {
            return false;
        }
        self.phase = DeployPhase::Failed;
        self.deploy_command = None;
        self.error = Some(error);
        true
    }

    /// Record a clipboard write, if the dialog that asked for it is still open.
    pub fn accept_copy(&mut self, generation: u64, copied: bool) -> bool {
        if !self.owns(generation) {
            return false;
        }
        self.copied = copied;
        true
    }

    /// The reader walked away: nothing already in flight may land here again.
    pub fn close(&mut self) {
        self.open = false;
        self.generation += 1;
    }
}
