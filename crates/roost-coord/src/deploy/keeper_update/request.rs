//! The request this RPC accepts, and the admission it is decided against.
//!
//! Owned by `deploy::keeper_update`. Depends on the shared
//! `roost_protocol::keeper_update` admission contract for every question about
//! whether an envelope proves anything, and on the write gate for the one
//! question only this coordinator can answer: was the fleet quiet while it
//! decided.

use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, PRESERVE, REPLACE_EMPTY, validate_keeper_coordinator_open_session_ids,
};
use roost_protocol::wire::WorkerFp;
use serde_json::Value;

use super::refusal::KeeperUpdateRefusal;
use crate::db::CoordDb;
use crate::write_gate::WriteGate;

/// The action the shared outcome comparator's maintenance branch keys on.
///
/// The shared module keeps its own copy private and exposes the comparator, so
/// this is the one label a caller must spell to reach that branch. It is not a
/// second classification: it is "shut the keeper down and admit no
/// replacement", which no journaled envelope ever carries.
pub const MAINTENANCE_ACTION: &str = "maintenance";

/// A journaled envelope past this is refused unread: the envelope holds two
/// keeper contracts and one admission, and anything larger is not one.
const MAX_JOURNALED_UPDATE_BYTES: usize = 32 * 1024;

/// What the caller is asking this coordinator to authorize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperUpdateAction {
    /// Restart the worker and keep the running keeper and its channels.
    Preserve,
    /// Shut the keeper down; only a provably empty one may be replaced.
    ReplaceEmpty,
    /// Shut the keeper down, admitting no replacement in this call.
    Maintenance,
}

impl KeeperUpdateAction {
    /// The label the shared outcome comparator matches against.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preserve => PRESERVE,
            Self::ReplaceEmpty => REPLACE_EMPTY,
            Self::Maintenance => MAINTENANCE_ACTION,
        }
    }

    /// Whether the running keeper must be provably empty before this proceeds.
    ///
    /// A `replace-empty` never crosses live sessions whatever the caller asked
    /// for: `force_live` is refused without the maintenance path before this is
    /// consulted, so no authorization reaches that arm. A maintenance shutdown
    /// is the one path an operator may authorize across live sessions, and the
    /// authorization is exactly the flag.
    #[must_use]
    pub fn requires_empty_keeper(self, force_live: bool) -> bool {
        match self {
            Self::ReplaceEmpty => true,
            Self::Maintenance => !force_live,
            Self::Preserve => false,
        }
    }
}

/// A parsed `WorkersPrepareKeeperUpdateRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperUpdateRequest {
    /// The path asked for: a maintenance shutdown or a journaled replacement.
    pub maintenance: bool,
    /// Operator authorization to cross live sessions, maintenance only.
    pub force_live: bool,
    /// The action this request is admitted or refused against.
    pub action: KeeperUpdateAction,
    /// The validated envelope, absent exactly on the maintenance path.
    pub journal: Option<JournaledKeeperUpdateV1>,
    /// Which machine of a two-host deploy this request names.
    pub direction: Option<&'static str>,
}

impl KeeperUpdateRequest {
    /// Validate the request, refusing in the order v2 asks its questions.
    pub fn parse(
        request: roost_proto::WorkersPrepareKeeperUpdateRequest,
    ) -> Result<Self, KeeperUpdateRefusal> {
        let (maintenance, force_live) = (request.maintenance, request.force_live);
        // First, and before anything is read: force_live authorizes destroying
        // live PTYs, so it must never arrive on the path that carries a
        // replayed or hand-edited journal.
        if force_live && !maintenance {
            return Err(KeeperUpdateRefusal::ForceLiveWithoutMaintenance);
        }
        if maintenance {
            if request.journaled_update_json.is_some() || !request.direction.is_empty() {
                return Err(KeeperUpdateRefusal::MaintenanceWithJournal);
            }
            return Ok(Self {
                maintenance,
                force_live,
                action: KeeperUpdateAction::Maintenance,
                journal: None,
                direction: None,
            });
        }
        let encoded = request.journaled_update_json.as_deref().unwrap_or_default();
        if encoded.is_empty() || encoded.len() > MAX_JOURNALED_UPDATE_BYTES {
            return Err(KeeperUpdateRefusal::JournaledUpdateRequired);
        }
        let direction = match request.direction.as_str() {
            "source" => Some("source"),
            "target" => Some("target"),
            _ => return Err(KeeperUpdateRefusal::InvalidDirection),
        };
        // The shared contract decides whether this envelope proves anything,
        // and every refinement it runs is a way a deploy could have hand-edited
        // a journal into claiming authority it does not hold.
        let value: Value = serde_json::from_str(encoded)
            .map_err(|_| KeeperUpdateRefusal::MalformedJournaledUpdate)?;
        let journal = JournaledKeeperUpdateV1::parse(&value).map_err(|error| {
            tracing::error!(%error, "a journaled keeper update did not satisfy the shared admission contract");
            KeeperUpdateRefusal::MalformedJournaledUpdate
        })?;
        let action = match journal.admission.required_action.as_str() {
            PRESERVE => KeeperUpdateAction::Preserve,
            REPLACE_EMPTY => KeeperUpdateAction::ReplaceEmpty,
            _ => return Err(KeeperUpdateRefusal::MalformedJournaledUpdate),
        };
        Ok(Self {
            maintenance,
            force_live,
            action,
            journal: Some(journal),
            direction,
        })
    }
}

/// What the coordinator proved, and the envelope the worker must now receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperUpdateAdmission {
    /// The action the journal, or the maintenance path, named.
    pub action: KeeperUpdateAction,
    /// The open-session proof this admission rests on, empty or not. Carried
    /// canonically so the worker can check it against the channel count it
    /// observes on the host, and logged when live sessions are crossed.
    pub open_session_ids: Vec<String>,
    /// The envelope, re-rendered rather than echoed: the worker parses this
    /// text, and a hand-edited one would be a second contract.
    pub journaled_update_json: Option<String>,
    /// `source` or `target`, empty on the maintenance path.
    pub direction: String,
    pub maintenance: bool,
    pub force_live: bool,
}

/// Decide whether the running keeper may be touched.
///
/// The drain is a parameter rather than an assumption because the emptiness
/// proof below is the only thing between this call and the destruction of every
/// live PTY on the machine, and it is only valid while nothing can create a
/// channel between the read and the worker's shutdown. This is the instant v2's
/// final-empty-recheck test hook fires, made structural: a decision reached
/// outside the drain is refused rather than trusted.
pub async fn decide_keeper_update(
    gate: &WriteGate,
    database: &CoordDb,
    worker_fp: &WorkerFp,
    request: &KeeperUpdateRequest,
) -> Result<KeeperUpdateAdmission, KeeperUpdateRefusal> {
    if !gate.exclusive_held() {
        return Err(KeeperUpdateRefusal::DrainNotHeld);
    }
    let open_session_ids = read_open_session_ids(database, worker_fp).await?;
    if request.action.requires_empty_keeper(request.force_live) && !open_session_ids.is_empty() {
        return Err(if request.maintenance {
            KeeperUpdateRefusal::MaintenanceBlockedByLiveSessions
        } else {
            KeeperUpdateRefusal::ReplacementBlockedByLiveSessions
        });
    }
    Ok(KeeperUpdateAdmission {
        action: request.action,
        journaled_update_json: request
            .journal
            .as_ref()
            .and_then(|journal| serde_json::to_string(journal).ok()),
        direction: request.direction.unwrap_or_default().to_owned(),
        maintenance: request.maintenance,
        force_live: request.force_live,
        open_session_ids,
    })
}

/// The coordinator's open sessions for one machine, in canonical order.
///
/// The order is part of the proof, not a convenience: the worker compares this
/// list against the channel count it observes on the host, so two coordinators
/// ordering differently would produce two proofs for one fleet.
pub async fn read_open_session_ids(
    database: &CoordDb,
    worker_fp: &WorkerFp,
) -> Result<Vec<String>, KeeperUpdateRefusal> {
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT id FROM sessions WHERE worker_fp = ?1 AND status = 'open' ORDER BY id ASC",
    )
    .bind(worker_fp.as_str())
    .fetch_all(database.pool())
    .await
    .map_err(|error| {
        tracing::error!(%worker_fp, %error, "the coordinator open-session proof could not be read");
        KeeperUpdateRefusal::CoordinatorReadFailed
    })?;
    validate_keeper_coordinator_open_session_ids("coordinator_open_session_ids", &ids).map_err(
        |error| {
            tracing::error!(%worker_fp, %error, "the coordinator open-session proof is not canonical");
            KeeperUpdateRefusal::MalformedOpenSessionProof
        },
    )?;
    Ok(ids)
}
