//! Pure projection math over terminal view records: what a record asked for,
//! and the ONE predicate that decides which records still bind the PTY.
//!
//! Ported from `packages/protocol/src/terminal-view/terminal-view-registry-state.ts`
//! and the wire guards of `terminal-view-protocol.ts`. No I/O, no socket access,
//! no clock of its own: every function is handed `now_ms`.
//!
//! WHY THE PREDICATE LIVES ALONE. `docs/FAILURE-INDEX.md`, "A session stays
//! clipped to a viewer that is no longer looking", is the incident: two
//! membership predicates over the same records, and the permissive one decided
//! the PTY size. Every aggregation of viewer geometry in this crate goes
//! through [`view_constrains`], and the minimum itself is
//! `roost_protocol::viewport::minimum_terminal_geometry`.
use std::collections::{BTreeMap, BTreeSet, HashMap};

use roost_proto::TerminalViewCommand;
use roost_protocol::viewport::{
    TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS, TERMINAL_VIEW_LEASE_MS, TERMINAL_VIEW_PARK_GRACE_MS,
    TerminalGeometry, is_terminal_geometry, is_terminal_uuid, minimum_terminal_geometry,
};
use roost_protocol::wire::SessionId;

pub use super::tombstone::{PROCESS_TOMBSTONE_CAP, Tombstone, TombstoneStore, VIEWER_TOMBSTONE_CAP};

/// Distinct view records one session may hold, across every socket and device.
pub const SESSION_VIEW_CAP: usize = 256;

/// What a client declared about one view: the session it watches and the size
/// it is painting at. Every field here is part of the same-revision equality
/// check, so a new field must be added to [`intents_equal`] as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewIntent {
    /// The session this view watches.
    pub session_id: String,
    /// The columns the viewer is painting.
    pub cols: u32,
    /// The rows the viewer is painting.
    pub rows: u32,
    /// Whether the viewer still wants frames.
    pub active: bool,
}

/// The composite key one view record is stored under: a view id is only
/// meaningful inside the tab that minted it.
#[must_use]
pub fn view_key(viewer_key: &str, view_id: &str) -> String {
    format!("{viewer_key}\0{view_id}")
}

/// The intent a wire command carries.
#[must_use]
pub fn intent_of(command: &TerminalViewCommand) -> ViewIntent {
    ViewIntent {
        session_id: command.session_id.clone(),
        cols: command.cols,
        rows: command.rows,
        active: command.active,
    }
}

/// Whether two intents are the same declaration, which is what a
/// same-revision replay is checked against.
#[must_use]
pub fn intents_equal(left: &ViewIntent, right: &ViewIntent) -> bool {
    left.session_id == right.session_id
        && left.cols == right.cols
        && left.rows == right.rows
        && left.active == right.active
}

/// The trust boundary for one view command, or the reason it is refused.
///
/// The viewer key always comes from the authenticated socket, never from the
/// command, which is why it is a parameter rather than a field.
#[must_use]
pub fn validate_view_command(
    viewer_key: Option<&str>,
    command: &TerminalViewCommand,
) -> Option<&'static str> {
    if viewer_key.is_none() {
        return Some("terminal views require a tab-bound Sync socket");
    }
    if !is_terminal_uuid(&command.view_id) {
        return Some("invalid terminal view id");
    }
    if !is_terminal_uuid(&command.session_id) {
        return Some("invalid terminal session id");
    }
    if command.revision < 1 {
        return Some("invalid terminal view revision");
    }
    let claimed = TerminalGeometry {
        cols: command.cols,
        rows: command.rows,
    };
    if command.active && !is_terminal_geometry(&claimed) {
        return Some("terminal geometry is outside 1..256");
    }
    if !command.active
        && (command.cols > TERMINAL_MAX_COLS || command.rows > TERMINAL_MAX_ROWS)
    {
        return Some("inactive terminal geometry is outside 0..256");
    }
    None
}

/// One socket-bound view record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRecord {
    /// The composite key this record is stored under.
    pub key: String,
    /// The view id the client minted.
    pub view_id: String,
    /// The `${fingerprint}:${tab}` key that owns the handle.
    pub viewer_key: String,
    /// The device this record's socket belonged to.
    pub fingerprint: String,
    /// The socket that last declared it.
    pub socket_id: String,
    /// What the client declared.
    pub intent: ViewIntent,
    /// The highest revision accepted for this record.
    pub revision: u64,
    /// When the lease lapses, unless a heartbeat renews it.
    pub deadline_ms: u64,
    /// Whether the owning socket is gone.
    pub parked: bool,
    /// When the owning socket dropped; zero while the record is live.
    pub parked_at_ms: u64,
    /// The last observed [`view_constrains`] value. Written only from that
    /// predicate, so the sweep re-minimizes on exactly the tick a grace lapses
    /// instead of once a second for as long as the record stays claimable.
    pub constrains: bool,
}

impl ViewRecord {
    /// A lease deadline `TERMINAL_VIEW_LEASE_MS` from `now_ms`.
    #[must_use]
    pub fn lease_deadline(now_ms: u64) -> u64 {
        now_ms.saturating_add(TERMINAL_VIEW_LEASE_MS)
    }
}

/// THE membership rule for effective geometry: a record binds the PTY while
/// its lease holds and its socket is either live or inside the park grace.
/// Park absorbs reconnect wobble for reclaim, a stream-continuity question; it
/// must not pin everyone else's PTY to a viewer whose socket is gone.
#[must_use]
pub fn view_constrains(record: &ViewRecord, now_ms: u64) -> bool {
    if record.deadline_ms <= now_ms {
        return false;
    }
    !record.parked || now_ms < record.parked_at_ms.saturating_add(TERMINAL_VIEW_PARK_GRACE_MS)
}

/// The geometry decision input for one session: `live` are the records that
/// currently bind the PTY, `retained` counts every record still in membership.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeometrySet {
    /// The records the live predicate admitted.
    pub live: Vec<TerminalGeometry>,
    /// How many records membership still holds, parked ones included.
    pub retained: usize,
}

/// The live and retained record counts for one session's membership.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewStats {
    /// Records whose socket is still attached.
    pub active: usize,
    /// Records whose socket is gone but whose claim is still reclaimable.
    pub parked: usize,
}

/// One viewer's row in the diagnostic projection: what it asked for and whether
/// it is part of the set that produced the session's effective geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewInput {
    /// The device this record's socket belonged to.
    pub fingerprint: String,
    /// The view id the client minted.
    pub view_id: String,
    /// The columns the viewer is painting.
    pub cols: u32,
    /// The rows the viewer is painting.
    pub rows: u32,
    /// Whether the owning socket is gone.
    pub parked: bool,
    /// Whether this record currently binds the PTY.
    pub constrains: bool,
}

impl From<&ViewRecord> for ViewInput {
    fn from(record: &ViewRecord) -> Self {
        Self {
            fingerprint: record.fingerprint.clone(),
            view_id: record.view_id.clone(),
            cols: record.intent.cols,
            rows: record.intent.rows,
            parked: record.parked,
            constrains: record.constrains,
        }
    }
}

/// Split one session's records into the ones that bind the PTY and the count
/// that stay in membership.
#[must_use]
pub fn geometry_set<'a>(
    views: &'a HashMap<String, ViewRecord>,
    keys: Option<&'a BTreeSet<String>>,
    now_ms: u64,
) -> GeometrySet {
    let empty = BTreeSet::new();
    let mut set = GeometrySet::default();
    for key in keys.unwrap_or(&empty) {
        let Some(record) = views.get(key) else {
            continue;
        };
        set.retained += 1;
        if view_constrains(record, now_ms) {
            set.live.push(TerminalGeometry {
                cols: record.intent.cols,
                rows: record.intent.rows,
            });
        }
    }
    set
}

/// The per-viewer diagnostic rows for one session.
#[must_use]
pub fn project_inputs(
    views: &HashMap<String, ViewRecord>,
    keys: Option<&BTreeSet<String>>,
    now_ms: u64,
) -> Vec<ViewInput> {
    let empty = BTreeSet::new();
    let mut inputs = Vec::new();
    for key in keys.unwrap_or(&empty) {
        let Some(record) = views.get(key) else {
            continue;
        };
        inputs.push(ViewInput {
            constrains: view_constrains(record, now_ms),
            ..ViewInput::from(record)
        });
    }
    inputs
}

/// The devices with a record on this session, in a stable order.
#[must_use]
pub fn active_fingerprints(
    views: &HashMap<String, ViewRecord>,
    keys: Option<&BTreeSet<String>>,
) -> BTreeSet<String> {
    let empty = BTreeSet::new();
    let mut fingerprints = BTreeSet::new();
    for key in keys.unwrap_or(&empty) {
        if let Some(record) = views.get(key) {
            fingerprints.insert(record.fingerprint.clone());
        }
    }
    fingerprints
}

/// The per-device geometry this session's viewers project, minimized per
/// device through the one shared primitive. Parked records contribute:
/// presence answers who has the session open, which is a different question
/// from who binds the PTY.
#[must_use]
pub fn project_viewers(
    session_views: &BTreeMap<SessionId, BTreeSet<String>>,
    views: &HashMap<String, ViewRecord>,
) -> BTreeMap<SessionId, BTreeMap<String, TerminalGeometry>> {
    let mut projection = BTreeMap::new();
    for (session_id, keys) in session_views {
        let mut grouped: BTreeMap<&str, Vec<TerminalGeometry>> = BTreeMap::new();
        for key in keys {
            if let Some(record) = views.get(key) {
                let geometry = TerminalGeometry {
                    cols: record.intent.cols,
                    rows: record.intent.rows,
                };
                grouped
                    .entry(record.fingerprint.as_str())
                    .or_default()
                    .push(geometry);
            }
        }
        let mut viewers = BTreeMap::new();
        for (fingerprint, geometries) in grouped {
            if let Ok(Some(geometry)) = minimum_terminal_geometry(&geometries) {
                viewers.insert(fingerprint.to_owned(), geometry);
            }
        }
        if !viewers.is_empty() {
            projection.insert(session_id.clone(), viewers);
        }
    }
    projection
}
