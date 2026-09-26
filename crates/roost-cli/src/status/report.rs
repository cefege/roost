//! The `roost status` report's data shapes, and the two JSON columns a
//! coordinator database hands back that still need validating. Called by
//! status/collect.rs (which fills it), status/render.rs (which prints it) and
//! the output-shape tests. Depends on roost-protocol for the two column
//! validators, because a keeper runtime and a capacity report arrive on the
//! wire and their rules are not the CLI's to restate.

use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::wire::TerminalCoreCapacityReport;
use serde_json::Value;

/// How long a worker may go without a heartbeat before the readout calls it
/// stale. 90s is three times the coordinator's 30s heartbeat cadence, so a
/// worker that missed one round trip is still fresh and one that missed three
/// is not. A smaller value marks a busy machine stale during an ordinary GC
/// pause; a larger one shows a machine that died two minutes ago as online.
pub const WORKER_STALE_MS: i64 = 90_000;

#[derive(Debug, Clone, PartialEq)]
pub struct CoordStatus {
    /// Did the unauthenticated identity RPC answer on the coordinator's own
    /// listener (or, off a coordinator host, through the front door)?
    pub reachable: bool,
    /// The build the coordinator reported, when it reported one. A reachable
    /// coordinator with no SHA is still reachable; the SHA only positions it
    /// against the fleet.
    pub git_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerStatus {
    pub fingerprint: String,
    pub label: String,
    pub os: String,
    pub reachable_addr: Option<String>,
    pub git_sha: Option<String>,
    /// None renders as "update admission unproven" — the coordinator has no
    /// observation, which is a different statement from "no channels".
    pub keeper_runtime: Option<KeeperRuntimeObservationV1>,
    pub terminal_core_capacity: Option<TerminalCoreCapacityReport>,
    pub coordinator_open_session_ids: Vec<String>,
    pub last_seen_ms: i64,
    pub age_ms: i64,
    pub stale: bool,
}

impl WorkerStatus {
    /// One machine's row, with staleness derived from the same clock the
    /// renderer uses. The caller passes `now_ms` so the age a test pins and
    /// the age an operator reads come from one value.
    pub fn with_derived_age(mut self, now_ms: i64) -> Self {
        self.age_ms = now_ms - self.last_seen_ms;
        self.stale = self.age_ms > WORKER_STALE_MS;
        self
    }
}

/// The operator-declared front door. Roost installs no proxy, tunnel, or
/// certificate, so the only claim this can make is whether that URL answers.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointStatus {
    pub public_url: Option<String>,
    pub answers: bool,
}

/// What the installed coordinator does with a page request.
///
/// `serves` and `web_dist_present` are separate fields because a deploy points
/// `ROOST_WEB_DIST_PATH` INTO a release directory a later settlement deletes:
/// the path existing and the path being served are independent facts, and
/// collapsing them into one "spa ok" is how a release retirement breaks every
/// URL while the readout still says the dist is there.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaStatus {
    /// HEAD `/` on the coordinator's own listener answered 200. None when
    /// there was no listener on this host to ask.
    pub serves: Option<bool>,
    pub web_dist_path: Option<String>,
    pub web_dist_present: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
    pub coord_agent_loaded: bool,
    pub worker_agent_loaded: bool,
    pub coord: CoordStatus,
    pub workers: Vec<WorkerStatus>,
    pub endpoint: EndpointStatus,
    pub spa: SpaStatus,
}

/// The one RPC every listener answers, whether reached directly or through a
/// front door. POSTed, not GETted, because a Connect service has no GET
/// surface — a GET 404 would make a perfectly healthy coordinator read as
/// silent, which is the failure this probe exists to avoid.
pub const COORD_IDENTITY_PATH: &str = "/roost.v1.CoordinatorService/AuthCoordIdentity";

/// A keeper runtime column that does not parse is a column to report as
/// missing, not a status failure. The coordinator is the authority on keeper
/// state; this process cannot re-derive it, and refusing to print the readout
/// because one projection is malformed would hide every other row.
pub fn parse_keeper_runtime(serialized: Option<&str>) -> Option<KeeperRuntimeObservationV1> {
    let value: Value = serde_json::from_str(serialized?).ok()?;
    KeeperRuntimeObservationV1::parse(&value).ok()
}

/// Same reasoning as [`parse_keeper_runtime`]: a coordinator predating
/// capacity reporting has no such column, and that is a normal state.
pub fn parse_terminal_core_capacity(
    serialized: Option<&str>,
) -> Option<TerminalCoreCapacityReport> {
    let value: Value = serde_json::from_str(serialized?).ok()?;
    TerminalCoreCapacityReport::parse(value).ok()
}
