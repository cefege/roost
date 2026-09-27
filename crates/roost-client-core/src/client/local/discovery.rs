//! Which worker door, if any, this page can reach on the browser's own machine.
//! Pure decision logic over a DESCRIBED environment, so it is provable with no
//! browser and no network.
//!
//! A page a worker served knows the answer from its own origin and never asks
//! anything. A page the coordinator served asks the machine's default loopback
//! door once, the first time a terminal pane goes live, and adopts a worker only
//! if that origin actually ANSWERED with a usable bootstrap. Everything else —
//! a 404, an unreachable origin, a body that is not a bootstrap — leaves the page
//! with no door at all, which is the honest answer: Sync is still there.
//!
//! Deliberately separate from `bootstrap.rs`, which answers the different
//! question "was this page served BY a worker" — the coordinator RPCs route off
//! that fact, and a discovered door must never move them.
//!
//! Ported from `apps/web/src/client/carriers/localWorkerDiscovery.ts`.

use crate::client::local::bootstrap::{
    BootstrapOutcome, BootstrapRefusal, LOCAL_BOOTSTRAP_PATH, LocalBootstrap, read_serving_origin,
};
use crate::client::local::door::http_origin_authority;

/// The machine's default loopback door, re-exported because it is the default
/// this module's whole decision turns on and a caller should not have to reach
/// into `roost-protocol` to learn it.
pub use roost_protocol::local_ui_door::DEFAULT_WORKER_LOCAL_UI_ORIGIN;

/// The key an operator sets for a door on a non-default port. Nothing reports a
/// worker's local-UI port to the coordinator, so there is nothing to derive.
pub const LOCAL_WORKER_ORIGIN_KEY: &str = "roost.localWorkerOrigin";

/// How long the door probe may take before the page concludes there is no door.
pub const DOOR_PROBE_TIMEOUT_MS: u64 = 2_000;

/// A worker door this browser can reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkerDoor {
    /// The origin to dial. Always the origin that was ASKED, never one the body
    /// named — a body that could redirect the dial would be a body that could
    /// point a terminal socket at anything.
    pub origin: String,
    /// The worker that answered there.
    pub worker_fingerprint: String,
}

/// What this browser can reach, described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEnvironment {
    /// The origin serving this document.
    pub page_origin: String,
    /// The bootstrap answer from the SERVING origin, when the page was served by
    /// a worker. `None` for a coordinator-served page.
    pub served_by_worker: Option<LocalBootstrap>,
    /// The operator's override, verbatim from storage, if any.
    pub operator_origin: Option<String>,
}

/// What discovery decided, the first time it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoorPlan {
    /// The page WAS served by a worker, and that origin is the door. No probe.
    Adopting(LocalWorkerDoor),
    /// Probe this URL.
    Probe { origin: String, url: String },
    /// Ask nothing, and why not.
    NotProbed(DoorAbsence),
    /// A previous call already asked, and this page probes exactly once.
    AlreadyAttempted,
}

/// What one probe produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoorAdoption {
    /// A door was reached and adopted.
    Adopted(LocalWorkerDoor),
    /// No door, and this is why.
    Absent(DoorAbsence),
}

/// Why a page has no worker door. Every member is one thing that was true, and a
/// host's log says which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoorAbsence {
    /// The origin answered, and not with a bootstrap.
    Status { origin: String },
    /// The origin answered 2xx with a body that is not a usable bootstrap.
    UnusableBody { origin: String, refusal: BootstrapRefusal },
    /// The origin could not be reached at all.
    Unreachable { origin: String },
    /// The candidate is this page's own origin, which already answered 404 for
    /// this path during startup; asking again would only burn a request.
    SameOrigin { origin: String },
    /// A probe answered for an origin this page did not ask, so it is stale and
    /// its answer is not about the door this page is waiting on.
    StaleProbe { origin: String },
}

impl DoorAbsence {
    /// The string a host records.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Status { .. } => "local worker door answered without a bootstrap",
            Self::UnusableBody { .. } => "local worker door body is not a usable bootstrap",
            Self::Unreachable { .. } => "local worker door is unreachable",
            Self::SameOrigin { .. } => "the candidate door is this page's own origin",
            Self::StaleProbe { .. } => "a stale door probe answered",
        }
    }
}

/// One page's door discovery.
///
/// Memoized because the caller is on a terminal pane's publish path and must not
/// wait on a network probe: a page that asked twice probes once. Interest in
/// adoption is expressed by draining [`take_adoptions`](Self::take_adoptions),
/// which is the only way a host learns a door appeared after it started.
#[derive(Debug, Default)]
pub struct DoorDiscovery {
    attempted: bool,
    planned_origin: Option<String>,
    door: Option<LocalWorkerDoor>,
    adoptions: Vec<LocalWorkerDoor>,
}

impl DoorDiscovery {
    /// A discovery that has not been asked yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The reachable door, or `None` while none is known.
    pub fn door(&self) -> Option<&LocalWorkerDoor> {
        self.door.as_ref()
    }

    /// Decide, once, what to do about the door for this page.
    pub fn start(&mut self, environment: &BrowserEnvironment) -> DoorPlan {
        if self.attempted {
            return DoorPlan::AlreadyAttempted;
        }
        self.attempted = true;
        if let Some(bootstrap) = &environment.served_by_worker {
            let door = LocalWorkerDoor {
                origin: environment.page_origin.clone(),
                worker_fingerprint: bootstrap.worker_fingerprint.clone(),
            };
            self.planned_origin = Some(door.origin.clone());
            self.adopt(door.clone());
            return DoorPlan::Adopting(door);
        }
        let origin = candidate_origin(environment.operator_origin.as_deref());
        if origin == environment.page_origin {
            return DoorPlan::NotProbed(DoorAbsence::SameOrigin { origin });
        }
        self.planned_origin = Some(origin.clone());
        DoorPlan::Probe {
            url: format!("{origin}{LOCAL_BOOTSTRAP_PATH}"),
            origin,
        }
    }

    /// Report one probe's answer, and adopt a door only if it answers the probe
    /// this page actually made.
    ///
    /// `status` is `None` when the request never completed, which is the only way
    /// a caller learns the origin was unreachable rather than unhelpful.
    pub fn complete_probe(
        &mut self,
        origin: &str,
        status: Option<u16>,
        payload: &str,
    ) -> DoorAdoption {
        if self.planned_origin.as_deref() != Some(origin) {
            return DoorAdoption::Absent(DoorAbsence::StaleProbe {
                origin: origin.to_string(),
            });
        }
        let adoption = match read_serving_origin(status, payload) {
            BootstrapOutcome::Served(answer) => {
                DoorAdoption::Adopted(LocalWorkerDoor {
                    origin: origin.to_string(),
                    worker_fingerprint: answer.worker_fingerprint,
                })
            }
            BootstrapOutcome::NotWorkerServed(refusal) => {
                let absence = match refusal {
                    BootstrapRefusal::Unreachable => DoorAbsence::Unreachable {
                        origin: origin.to_string(),
                    },
                    BootstrapRefusal::Status => DoorAbsence::Status {
                        origin: origin.to_string(),
                    },
                    other => DoorAbsence::UnusableBody {
                        origin: origin.to_string(),
                        refusal: other,
                    },
                };
                DoorAdoption::Absent(absence)
            }
        };
        if let DoorAdoption::Adopted(door) = &adoption {
            self.adopt(door.clone());
        }
        adoption
    }

    /// Take the doors adopted since the last drain.
    pub fn take_adoptions(&mut self) -> Vec<LocalWorkerDoor> {
        std::mem::take(&mut self.adoptions)
    }

    /// Forget everything, so a test can ask a fresh question of the same page.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn adopt(&mut self, door: LocalWorkerDoor) {
        self.door = Some(door.clone());
        self.adoptions.push(door);
    }
}

/// The origin a probe should go to: the operator's override when it is a bare
/// `http`/`https` origin, and the machine's default loopback door otherwise.
///
/// A malformed override is IGNORED rather than dialed, which is the whole point
/// of the bare-origin rule: a stored value that would resolve somewhere other
/// than the operator meant is not a door.
pub fn candidate_origin(operator_origin: Option<&str>) -> String {
    operator_origin
        .filter(|stored| http_origin_authority(stored).is_some())
        .unwrap_or(DEFAULT_WORKER_LOCAL_UI_ORIGIN)
        .to_string()
}
