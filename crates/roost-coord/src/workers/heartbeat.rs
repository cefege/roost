//! The heartbeat write, and the choice between the two presence frames a beat
//! can publish.
//!
//! Ported from `apps/coord/src/workers/handlers-workers-heartbeat.ts`.
//!
//! EVERY FIELD A BEAT RE-ASSERTS SELF-HEALS A ROW. `git_sha`, `reachable_addr`,
//! `os` and the host identity are re-sent every beat precisely so a machine that
//! was renamed, rebuilt or moved converges within one heartbeat instead of
//! keeping its enrollment-time value forever. `reachable_addr` is the exception
//! that proves the rule: an EMPTY value this beat means the worker could not
//! resolve it, so the prior good address is kept -- a null would show an
//! operator a machine it can no longer reach.
//!
//! EVERY OPTIONAL COLUMN IS RESOLVED AGAINST THE PRIOR ROW BEFORE THE WRITE,
//! never with a SQL `COALESCE`. Three of these fields are three-state -- absent
//! keeps the stored value, present-and-empty clears it -- and a `COALESCE`
//! cannot tell the last two apart, so the statement would silently keep a value
//! the worker asked to have cleared.
//!
//! A STATIC FIELD CHANGE RIDES THE FULL ROW, NOT THE LIGHT BEAT. The SPA's
//! worker record is only replaced by a `registered` frame, so a heartbeat that
//! changed `os` and published a light delta would leave the browser showing the
//! platform of a machine that no longer holds the key
//! (`handlers-workers-heartbeat.ts:199-221`).

use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::wire::{HostIdentity, HostMetrics, TerminalCoreCapacityReport, WorkerOs};
use sqlx::AssertSqlSafe;

use crate::db::CoordDb;

use super::register::WorkerWriteError;
use super::rows::{StoredWorkerRow, worker_projection};

/// Which of a beat's two proof-carrying claims failed to validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedClaim {
    /// The authenticated keeper proof.
    KeeperRuntime,
    /// The terminal-core admission report.
    TerminalCoreCapacity,
}

impl MalformedClaim {
    /// What the refusal says.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::KeeperRuntime => "keeper runtime observation is malformed",
            Self::TerminalCoreCapacity => "terminal core capacity report is malformed",
        }
    }
}

/// Which of the two proof-carrying claims did not validate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MalformedClaims {
    /// The keeper proof did not validate.
    pub keeper_runtime: bool,
    /// The terminal-core report did not validate.
    pub terminal_core_capacity: bool,
}

impl MalformedClaims {
    /// The claim to report, preferring the keeper proof because it is the one an
    /// update admission acts on.
    #[must_use]
    pub fn first(self) -> Option<MalformedClaim> {
        if self.keeper_runtime {
            Some(MalformedClaim::KeeperRuntime)
        } else if self.terminal_core_capacity {
            Some(MalformedClaim::TerminalCoreCapacity)
        } else {
            None
        }
    }
}

/// What a beat claimed, after validation.
#[derive(Debug, Clone, Default)]
pub struct HeartbeatClaim {
    /// A load sample. Absent keeps the stored sample.
    pub host_metrics: Option<HostMetrics>,
    /// The build this process runs. Absent keeps the stored value.
    pub git_sha: Option<String>,
    /// The address resolved this beat. Absent -- including an empty string --
    /// keeps the stored value.
    pub reachable_addr: Option<String>,
    /// The platform this process runs on. Absent keeps the stored value.
    pub os: Option<WorkerOs>,
    /// The static machine identity, three-state: absent keeps the stored value
    /// and `Some(None)` clears it, which is what a worker reporting an empty
    /// identity is asking for.
    pub host_identity: Option<Option<HostIdentity>>,
    /// The authenticated keeper proof. Always written: a beat without one proves
    /// the keeper is gone, and keeping the previous proof would let an update
    /// admission believe otherwise.
    pub keeper_runtime: Option<KeeperRuntimeObservationV1>,
    /// The terminal-core admission report, always written for the same reason.
    pub terminal_core_capacity: Option<TerminalCoreCapacityReport>,
    /// Which proof-carrying claim did not validate, if either did.
    pub malformed: MalformedClaims,
}

/// Which static fields this beat changed, and therefore whether the presence
/// frame has to carry the whole row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StaticFieldChanges {
    /// The build changed.
    pub git_sha: bool,
    /// The keeper proof changed.
    pub keeper_runtime: bool,
    /// The reachable address changed.
    pub reachable_addr: bool,
    /// The platform changed.
    pub os: bool,
    /// The static machine identity changed.
    pub host_identity: bool,
}

impl StaticFieldChanges {
    /// Whether any static field moved, which forces the full presence frame.
    #[must_use]
    pub fn any(self) -> bool {
        self.git_sha || self.keeper_runtime || self.reachable_addr || self.os || self.host_identity
    }
}

/// What a beat decided.
#[derive(Debug)]
pub enum HeartbeatOutcome {
    /// The beat committed.
    Committed {
        /// The row as it now stands.
        updated: StoredWorkerRow,
        /// The static fields that moved, which decide the presence frame.
        changed: StaticFieldChanges,
    },
    /// A proof-carrying claim did not validate: the column was cleared, the
    /// beat still recorded that the worker is alive, and the beat is refused.
    Refused {
        /// The row as it now stands, with the bad column cleared.
        cleared: StoredWorkerRow,
        /// Which claim was refused.
        malformed: MalformedClaim,
    },
    /// There is no live row for this fingerprint: absent, or tombstoned.
    Tombstoned,
}

/// Commit one beat against a worker whose live row the caller already read.
pub async fn apply_worker_heartbeat(
    database: &CoordDb,
    prior: &StoredWorkerRow,
    claim: &HeartbeatClaim,
    now_ms: i64,
) -> Result<HeartbeatOutcome, WorkerWriteError> {
    if let Some(malformed) = claim.malformed.first() {
        return clear_malformed(database, prior, malformed, now_ms).await;
    }
    let fp = &prior.fp;
    let keeper_runtime_json = encode_optional(fp, &claim.keeper_runtime)?;
    let terminal_core_capacity_json = encode_optional(fp, &claim.terminal_core_capacity)?;
    let host_metrics_json = match &claim.host_metrics {
        Some(metrics) => Some(encode(fp, metrics)?),
        None => prior.host_metrics_json.clone(),
    };
    let git_sha = claim.git_sha.clone().or_else(|| prior.git_sha.clone());
    let reachable_addr = claim
        .reachable_addr
        .clone()
        .or_else(|| prior.reachable_addr.clone());
    let os = claim
        .os
        .map_or_else(|| prior.os.clone(), |os| os.as_str().to_owned());
    let host_identity_json = match &claim.host_identity {
        Some(Some(identity)) => Some(encode(fp, identity)?),
        Some(None) => None,
        None => prior.host_identity_json.clone(),
    };
    let sql = format!(
        "UPDATE workers SET last_seen_ms = ?, keeper_runtime_json = ?, \
         terminal_core_capacity_json = ?, host_metrics_json = ?, git_sha = ?, \
         reachable_addr = ?, os = ?, host_identity_json = ? \
         WHERE fp = ? AND deleted_at_ms IS NULL RETURNING {}",
        worker_projection()
    );
    let updated = sqlx::query_as::<_, StoredWorkerRow>(AssertSqlSafe(sql))
        .bind(now_ms)
        .bind(&keeper_runtime_json)
        .bind(&terminal_core_capacity_json)
        .bind(&host_metrics_json)
        .bind(&git_sha)
        .bind(&reachable_addr)
        .bind(&os)
        .bind(&host_identity_json)
        .bind(fp)
        .fetch_optional(database.pool())
        .await?;
    let Some(updated) = updated else {
        return Ok(HeartbeatOutcome::Tombstoned);
    };
    let changed = StaticFieldChanges {
        git_sha: prior.git_sha != git_sha,
        keeper_runtime: prior.keeper_runtime_json != keeper_runtime_json,
        reachable_addr: prior.reachable_addr != reachable_addr,
        os: prior.os != os,
        host_identity: prior.host_identity_json != host_identity_json,
    };
    Ok(HeartbeatOutcome::Committed { updated, changed })
}

/// Clear a claim that did not validate, and record the beat anyway.
///
/// A malformed proof is a fact about the worker, and refusing the beat outright
/// would make this coordinator look like one where the worker stopped reporting.
async fn clear_malformed(
    database: &CoordDb,
    prior: &StoredWorkerRow,
    malformed: MalformedClaim,
    now_ms: i64,
) -> Result<HeartbeatOutcome, WorkerWriteError> {
    let column = match malformed {
        MalformedClaim::KeeperRuntime => "keeper_runtime_json",
        MalformedClaim::TerminalCoreCapacity => "terminal_core_capacity_json",
    };
    let sql = format!(
        "UPDATE workers SET last_seen_ms = ?, {column} = NULL \
         WHERE fp = ? AND deleted_at_ms IS NULL RETURNING {}",
        worker_projection()
    );
    let cleared = sqlx::query_as::<_, StoredWorkerRow>(AssertSqlSafe(sql))
        .bind(now_ms)
        .bind(&prior.fp)
        .fetch_optional(database.pool())
        .await?;
    match cleared {
        Some(cleared) => Ok(HeartbeatOutcome::Refused { cleared, malformed }),
        None => Ok(HeartbeatOutcome::Tombstoned),
    }
}

/// Encode a value for the JSON column that stores it.
fn encode<T: serde::Serialize>(worker_fp: &str, value: &T) -> Result<String, WorkerWriteError> {
    serde_json::to_string(value).map_err(|error| WorkerWriteError::Encoding {
        fp: worker_fp.to_owned(),
        reason: error.to_string(),
    })
}

/// Encode a value for a column that stores SQL `NULL` when it is absent.
///
/// Not `encode(&None)`: that is the four characters `null`, which every read
/// decodes as absent and which would make an absent claim look like a changed
/// one on every single beat.
fn encode_optional<T: serde::Serialize>(
    worker_fp: &str,
    value: &Option<T>,
) -> Result<Option<String>, WorkerWriteError> {
    value
        .as_ref()
        .map(|value| encode(worker_fp, value))
        .transpose()
}
