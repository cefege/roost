//! Session tasks: the claimable work queue, the five `Tasks*` Connect methods
//! that move a row through it, and the bus delta every mutation publishes. Called
//! by `rpc/service_impl.rs` through the `METHOD_HANDLERS` table below; depends on
//! the `tasks` table, `task_bus` and `core.services.boot`. A task IS a row with a
//! status: v2 has no workflow and no scheduler, and one would be a second model.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::TaskState;
use serde_json::Value;
use sqlx::AssertSqlSafe;

use crate::auth::principal::require_account_device;
use crate::coord_core::ids::new_task_id;
use crate::coord_core::{Caller, CoordCore};
use crate::db::CoordDb;
use crate::events::bus_messages::{TaskBusMsg, TaskBusMsgKind};
use crate::rpc::service::{now_ms, ok_response};
use crate::write_gate::SharedLease;

/// The Connect method each handler answers, and the function that answers it.
pub const METHOD_HANDLERS: [(&str, &str); 5] = [
    ("TasksList", "sessions::tasks::handle_tasks_list"),
    ("TasksEnqueue", "sessions::tasks::handle_tasks_enqueue"),
    (
        "TasksNextPending",
        "sessions::tasks::handle_tasks_next_pending",
    ),
    ("TasksSetState", "sessions::tasks::handle_tasks_set_state"),
    ("TasksCancel", "sessions::tasks::handle_tasks_cancel"),
];

/// The oldest rows a list read returns: no DELETE path, so history only grows.
const TASKS_LIST_MAX_ROWS: i64 = 500;

/// The claim ttl a task gets when the enqueue names none (`handlers-tasks.ts:80`).
const DEFAULT_CLAIM_TTL_MS: i64 = 15 * 60 * 1000;

/// The ttl asked for, in the width the column holds. A `u64` past `i64::MAX` is
/// refused, not wrapped: a negative ttl reads as "already expired".
fn claim_ttl_ms(requested: Option<u64>) -> Result<i64, ConnectError> {
    let reason = "claimTtlMs exceeds a 64-bit millisecond timestamp";
    requested
        .map_or(Ok(DEFAULT_CLAIM_TTL_MS), i64::try_from)
        .map_err(|_| refusal(ErrorCode::InvalidArgument, reason))
}

/// One stored `tasks` row: the whole model, and the only place column names and
/// wire names meet.
#[derive(Debug, Clone, sqlx::FromRow)]
struct StoredTaskRow {
    id: String,
    state: String,
    payload_json: String,
    enqueued_at_ms: i64,
    claimed_at_ms: Option<i64>,
    claimed_by: Option<String>,
    finished_at_ms: Option<i64>,
    result_json: Option<String>,
    completion_check: Option<String>,
    completion_check_last_attempt_ms: Option<i64>,
    claim_ttl_ms: i64,
}

const TASK_COLUMNS: &str = "id, state, payload_json, enqueued_at_ms, claimed_at_ms, \
    claimed_by, finished_at_ms, result_json, completion_check, \
    completion_check_last_attempt_ms, claim_ttl_ms";

/// `TasksList` -- the queue, oldest first.
pub async fn handle_tasks_list(
    core: &CoordCore,
    caller: &Caller,
    request: proto::TasksListRequest,
) -> ServiceResult<proto::TasksListResponse> {
    require_account_device(caller)?;
    // An absent filter and an empty one ask the same question: v2's
    // `if (req.state)` read `""` as "every state" too.
    let asked = request.state.as_deref().filter(|raw| !raw.is_empty());
    let rows = read_tasks(&core.services.db, asked.map(task_state_of).transpose()?)
        .await
        .map_err(internal)?;
    let tasks: Vec<proto::Task> = rows.iter().map(task_row_to_proto).collect();
    // No event, deliberately: a read changes nothing, and a line per poll is a
    // log that teaches a reader to skip logs. Only a mutation is worth that.
    ok_response(proto::TasksListResponse {
        tasks,
        ..Default::default()
    })
}

/// `TasksEnqueue` -- one pending row, and its `created` delta.
pub async fn handle_tasks_enqueue(
    core: &CoordCore,
    caller: &Caller,
    request: proto::TasksEnqueueRequest,
) -> ServiceResult<proto::TasksEnqueueResponse> {
    require_account_device(caller)?;
    // A bare string with no JSON shape: accepting garbage would store a payload
    // every downstream `Task` consumer reads as absent. That it must be an OBJECT
    // is the other half, and is NOT checked; see the report.
    serde_json::from_str::<Value>(&request.payload_json).map_err(|error| {
        refusal(
            ErrorCode::InvalidArgument,
            format!("invalid payloadJson: {error}"),
        )
    })?;
    let dashboard_id = core.services.boot.require_tenant()?.dashboard_id.clone();
    let id = new_task_id(
        core.services.boot.process_epoch(),
        core.services.boot.boot_ms(),
    );
    let _lease = lease(core)?;
    let claim_ttl_ms = claim_ttl_ms(request.claim_ttl_ms)?;
    let row = insert_task(&core.services.db, &id, &dashboard_id, &request, claim_ttl_ms)
        .await
        .map_err(internal)?;
    let task = publish(core, TaskBusMsgKind::Created, &row);
    tracing::info!(task = %id, "task enqueued");
    ok_response(proto::TasksEnqueueResponse {
        task: proto::buffa::MessageField::some(task),
        ..Default::default()
    })
}

/// `TasksNextPending` -- claim the oldest pending task. An empty queue is not a
pub async fn handle_tasks_next_pending(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::TasksNextPendingRequest,
) -> ServiceResult<proto::TasksNextPendingResponse> {
    let fingerprint = require_account_device(caller)?;
    let _lease = lease(core)?;
    let claimed_at_ms = now_ms();
    // THE CLAIM IS ONE STATEMENT: the row is chosen inside the write that claims
    // it, where a SELECT then an UPDATE would let two devices have one.
    let sql = format!(
        "UPDATE tasks SET state = 'claimed', claimed_at_ms = ?, claimed_by = ? WHERE id = \
         (SELECT id FROM tasks WHERE state = 'pending' ORDER BY enqueued_at_ms LIMIT 1) \
         RETURNING {TASK_COLUMNS}"
    );
    let row = sqlx::query_as::<_, StoredTaskRow>(AssertSqlSafe(sql))
        .bind(claimed_at_ms)
        .bind(fingerprint)
        .fetch_optional(core.services.db.pool())
        .await
        .map_err(internal)?;
    let Some(row) = row else {
        return ok_response(proto::TasksNextPendingResponse::default());
    };
    let task = publish(core, TaskBusMsgKind::State, &row);
    tracing::info!(task = %row.id, claimed_by = %fingerprint, "task claimed");
    ok_response(proto::TasksNextPendingResponse {
        task: proto::buffa::MessageField::some(task),
        ..Default::default()
    })
}

/// `TasksSetState` -- move a task, and report the new row.
pub async fn handle_tasks_set_state(
    core: &CoordCore,
    caller: &Caller,
    request: proto::TasksSetStateRequest,
) -> ServiceResult<proto::TasksSetStateResponse> {
    let fingerprint = require_account_device(caller)?;
    let _lease = lease(core)?;
    let state = task_state_of(&request.state)?;
    let existing = read_task(&core.services.db, &request.id)
        .await
        .map_err(internal)?
        .ok_or_else(|| refusal(ErrorCode::NotFound, "task not found"))?;
    // The claim fences the ANSWER, not the row: a device that did not claim this
    // task may not report an outcome. A PENDING task has no claim at all.
    if existing
        .claimed_by
        .as_deref()
        .is_some_and(|claimed_by| claimed_by != fingerprint)
    {
        return Err(ConnectError::new(
            ErrorCode::PermissionDenied,
            "task claimed by different worker",
        ));
    }
    let terminal = matches!(
        state,
        TaskState::Done | TaskState::Failed | TaskState::Cancelled
    );
    let finished_at_ms = terminal.then_some(now_ms());
    let row = update_task_state(
        &core.services.db,
        &request.id,
        state,
        finished_at_ms,
        request.result_json.as_deref(),
    )
    .await
    .map_err(internal)?
    .ok_or_else(|| refusal(ErrorCode::NotFound, "task not found"))?;
    let task = publish(core, TaskBusMsgKind::State, &row);
    tracing::info!(task = %row.id, state = state.as_str(), "task state set");
    ok_response(proto::TasksSetStateResponse {
        task: proto::buffa::MessageField::some(task),
        ..Default::default()
    })
}

/// `TasksCancel` -- stop a task that has not finished. One refusal covers
pub async fn handle_tasks_cancel(
    core: &CoordCore,
    caller: &Caller,
    request: proto::TasksCancelRequest,
) -> ServiceResult<proto::TasksCancelResponse> {
    require_account_device(caller)?;
    let _lease = lease(core)?;
    let sql = format!(
        "UPDATE tasks SET state = 'cancelled', finished_at_ms = ? WHERE id = ? \
         AND state NOT IN ('done', 'failed', 'cancelled') RETURNING {TASK_COLUMNS}"
    );
    let row = sqlx::query_as::<_, StoredTaskRow>(AssertSqlSafe(sql))
        .bind(now_ms())
        .bind(&request.id)
        .fetch_optional(core.services.db.pool())
        .await
        .map_err(internal)?
        .ok_or_else(|| refusal(ErrorCode::NotFound, "task not found or already terminal"))?;
    let task = publish(core, TaskBusMsgKind::State, &row);
    tracing::info!(task = %row.id, "task cancelled");
    ok_response(proto::TasksCancelResponse {
        task: proto::buffa::MessageField::some(task),
        ..Default::default()
    })
}

/// Announce one row change, and hand the caller the wire row it announced. One
/// function rather than four inline publishes, because the incident this file
/// exists not to repeat (`docs/FAILURE-INDEX.md`, "a mutation commits without
/// publishing its bus delta") is a write that tells no one, and Sync backfill
/// cannot repair it. The published row is the one the write returned.
fn publish(core: &CoordCore, kind: TaskBusMsgKind, row: &StoredTaskRow) -> proto::Task {
    let task = task_row_to_proto(row);
    core.services.buses.task_bus.publish(TaskBusMsg {
        kind,
        task: task.clone(),
    });
    task
}

/// The queue, optionally narrowed to one state.
async fn read_tasks(
    db: &CoordDb,
    state: Option<TaskState>,
) -> Result<Vec<StoredTaskRow>, sqlx::Error> {
    let filter = if state.is_some() { " WHERE state = ?" } else { "" };
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM tasks{filter} \
         ORDER BY enqueued_at_ms LIMIT {TASKS_LIST_MAX_ROWS}"
    );
    let query = sqlx::query_as::<_, StoredTaskRow>(AssertSqlSafe(sql));
    match state {
        Some(state) => query.bind(state.as_str()).fetch_all(db.pool()).await,
        None => query.fetch_all(db.pool()).await,
    }
}

/// One task row by id, or `None` for absent.
async fn read_task(database: &CoordDb, id: &str) -> Result<Option<StoredTaskRow>, sqlx::Error> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?");
    sqlx::query_as::<_, StoredTaskRow>(AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(database.pool())
        .await
}

/// Insert one pending task, returning the row as stored. `RETURNING` beats
/// v2's insert-then-reselect: no window where the read row is not the new one.
async fn insert_task(
    database: &CoordDb,
    id: &str,
    dashboard_id: &str,
    request: &proto::TasksEnqueueRequest,
    claim_ttl_ms: i64,
) -> Result<StoredTaskRow, sqlx::Error> {
    let sql = format!(
        "INSERT INTO tasks (id, dashboard_id, state, payload_json, enqueued_at_ms, \
         claimed_at_ms, claimed_by, finished_at_ms, result_json, completion_check, \
         completion_check_last_attempt_ms, claim_ttl_ms) \
         VALUES (?, ?, 'pending', ?, ?, NULL, NULL, NULL, NULL, ?, NULL, ?) \
         RETURNING {TASK_COLUMNS}"
    );
    sqlx::query_as::<_, StoredTaskRow>(AssertSqlSafe(sql))
        .bind(id)
        .bind(dashboard_id)
        .bind(&request.payload_json)
        .bind(now_ms())
        .bind(&request.completion_check)
        .bind(claim_ttl_ms)
        .fetch_one(database.pool())
        .await
}

/// Move a task to a state, stamping a finish only for a terminal one.
async fn update_task_state(
    database: &CoordDb,
    id: &str,
    state: TaskState,
    finished_at_ms: Option<i64>,
    result_json: Option<&str>,
) -> Result<Option<StoredTaskRow>, sqlx::Error> {
    let sql = format!(
        "UPDATE tasks SET state = ?, finished_at_ms = COALESCE(?, finished_at_ms), \
         result_json = COALESCE(?, result_json) WHERE id = ? RETURNING {TASK_COLUMNS}"
    );
    sqlx::query_as::<_, StoredTaskRow>(AssertSqlSafe(sql))
        .bind(state.as_str())
        .bind(finished_at_ms)
        .bind(result_json)
        .bind(id)
        .fetch_optional(database.pool())
        .await
}

/// A stored timestamp on the wire, which is unsigned; 0 beats a refusal.
fn epoch_ms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// The wire task for a stored row.
fn task_row_to_proto(row: &StoredTaskRow) -> proto::Task {
    proto::Task {
        id: row.id.clone(),
        state: row.state.clone(),
        payload_json: row.payload_json.clone(),
        enqueued_at_ms: epoch_ms(row.enqueued_at_ms),
        claimed_at_ms: row.claimed_at_ms.map(epoch_ms),
        claimed_by: row.claimed_by.clone(),
        finished_at_ms: row.finished_at_ms.map(epoch_ms),
        result_json: row.result_json.clone(),
        completion_check: row.completion_check.clone(),
        completion_check_last_attempt_ms: row.completion_check_last_attempt_ms.map(epoch_ms),
        claim_ttl_ms: epoch_ms(row.claim_ttl_ms),
        ..Default::default()
    }
}

/// The state a wire string names, or v2's `TaskState.safeParse` refusal.
fn task_state_of(raw: &str) -> Result<TaskState, ConnectError> {
    serde_json::from_value::<TaskState>(Value::String(raw.to_owned())).map_err(|_| {
        refusal(
            ErrorCode::InvalidArgument,
            format!("invalid task state {raw:?}"),
        )
    })
}


fn refusal(code: ErrorCode, reason: impl Into<String>) -> ConnectError {
    ConnectError::new(code, reason.into())
}

fn internal(error: sqlx::Error) -> ConnectError {
    refusal(ErrorCode::Internal, error.to_string())
}

/// A shared lease, or the gate's refusal: `method_holds_lease` names these four
/// methods and the gate has no other caller, so this is the fence.
fn lease(core: &CoordCore) -> Result<SharedLease, ConnectError> {
    let gate = core.services.write_gate();
    gate.acquire_shared()
        .map_err(|held| refusal(ErrorCode::Unavailable, held.to_string()))
}
