//! The already-validated data the registry, presence and control `SyncFrame`
//! variants carry: the shapes v2 built in its proto→wire adapters and handed to
//! its `_handle*` projectors.
//!
//! Built only by `sync::decode`, read only by the folds under `handle_sync/`.
//! Ported from `apps/web/src/store/sync-frame.ts:102-371` and
//! `apps/web/src/lib/pairedBrowserNotice.ts`.

use crate::store::PairRequest;

/// One viewer of a session, from a `session_presence` frame of kind `viewers`.
///
/// v2 folds these into `rootStore.session_viewers[sid]` keyed by `fp`
/// (`sync-frame.ts:231-262`); the sidebar draws one dot per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionViewer {
    /// The viewing browser's key fingerprint.
    pub fp: String,
    /// The viewer's own key, which is the fingerprint when the payload named
    /// none (`viewerKey ?? fp`).
    pub viewer_key: String,
    /// The viewer's effective columns; `0` when the payload listed only fps.
    pub cols: u32,
    /// The viewer's effective rows; `0` when the payload listed only fps.
    pub rows: u32,
    /// When the coordinator last saw the viewer, when it said.
    pub last_ms: Option<i64>,
    /// The viewer's display label, when it has one.
    pub label: Option<String>,
}

/// One `audit_log` insert, in the legacy wire shape the audit pane renders
/// (`sync-frame.ts:161-176`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// The row id. Rows are deduplicated by it.
    pub id: u64,
    /// When the call happened, in milliseconds.
    pub ts: u64,
    /// The caller's key fingerprint, when the call was authenticated.
    pub caller_fp: Option<String>,
    /// The caller's label, when the coordinator knew one.
    pub caller_label: Option<String>,
    /// The HTTP method.
    pub method: String,
    /// The request path.
    pub path: String,
    /// The response status.
    pub status: u32,
    /// The request's trace id, when it carried one.
    pub trace_id: Option<String>,
}

/// Where one `worker_routable` frame sits inside a chunked retained seed.
///
/// Absent on a live full-set replacement (`snapshot_id` empty on the wire).
/// Present only after decode has checked the bounds v2 checks
/// (`sync-inbound.ts:150-163`): `1..=4096` chunks and an index inside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutableChunk {
    /// The seed this chunk belongs to. A chunk of a different seed discards
    /// every partial one.
    pub snapshot_id: String,
    /// Which chunk this is.
    pub chunk_index: u32,
    /// How many chunks the seed has.
    pub chunk_count: u32,
}

/// One pair-request change (`sync-frame.ts:285-350`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairRequestChange {
    /// A request to upsert, keyed by its ephemeral id.
    Pending(PairRequest),
    /// A request approved or denied elsewhere: drop it.
    Removed {
        /// The request's ephemeral id.
        ephemeral_id: String,
    },
    /// The whole pending set, seeded per Sync connect. REPLACES the set, so a
    /// removal missed while disconnected cannot linger.
    Snapshot(Vec<PairRequest>),
    /// A pairing finished: drop the request and announce the new browser once.
    Completed(PairedBrowser),
}

/// The browser a completed pairing admitted, as the notice describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedBrowser {
    /// The request that completed.
    pub ephemeral_id: String,
    /// The requester's own label.
    pub label: String,
    /// The browser the edge parsed.
    pub client_browser: String,
    /// The operating system the edge parsed.
    pub client_os: String,
    /// The city the edge resolved.
    pub city: String,
    /// The region the edge resolved.
    pub region: String,
    /// The country the edge resolved.
    pub country_code: String,
}

/// What the notice calls a browser neither the edge nor the requester named.
const UNKNOWN_BROWSER_LABEL: &str = "Unknown browser";

impl PairedBrowser {
    /// "Chrome on macOS · Berlin": the parsed browser and OS first, the
    /// requester's own label when neither was parsed, then the most specific
    /// known place. v2 `formatPairedBrowserLabel`.
    pub fn announcement_label(&self) -> String {
        let browser = self.client_browser.trim();
        let os = self.client_os.trim();
        let device = match (browser.is_empty(), os.is_empty()) {
            (false, false) => format!("{browser} on {os}"),
            (false, true) => browser.to_owned(),
            (true, false) => os.to_owned(),
            (true, true) => match self.label.trim() {
                "" => UNKNOWN_BROWSER_LABEL.to_owned(),
                label => label.to_owned(),
            },
        };
        let place = [&self.city, &self.region, &self.country_code]
            .into_iter()
            .map(|part| part.trim())
            .find(|part| !part.is_empty());
        match place {
            Some(place) => format!("{device} · {place}"),
            None => device,
        }
    }
}

/// The answer to one `input_route_claim` this socket sent
/// (`sync.proto` `TerminalInputRouteResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRouteResult {
    /// The claim this answers.
    pub request_id: String,
    /// The session the claim was for.
    pub session_id: String,
    /// The revision the claim used.
    pub revision: u64,
    /// Whether the worker admitted the route.
    pub accepted: bool,
    /// The newest revision the worker holds, for a refused claim to retry above.
    pub latest_revision: u64,
    /// The epoch to stamp on input, when accepted.
    pub input_route_epoch: String,
    /// The worker process epoch that answered.
    pub worker_epoch: String,
    /// Why the claim was refused, when it was.
    pub reason: String,
}

/// The answer to one content-free `terminal_transport_probe`.
///
/// An empty `worker_epoch` is the coordinator's "not here": the probe's wire
/// shape has no error field (`crates/roost-coord/src/sync_ws/control_frames.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportProbeResult {
    /// The probe this answers.
    pub request_id: String,
    /// The worker probed.
    pub worker_fp: String,
    /// The worker process epoch that answered, or empty for a refusal.
    pub worker_epoch: String,
}

/// A coordinator announcing it is handing this socket to another origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorRelocation {
    /// The handoff's identity.
    pub handoff_id: String,
    /// The origin being left.
    pub source_url: String,
    /// The origin to dial instead.
    pub target_url: String,
}
