//! Whether a Sync WebSocket upgrade is allowed, decided before any socket state
//! exists.
//!
//! Owned by the coordinator's Sync link. Pure: the shape of the request goes
//! in, a decision comes out, and the I/O layer applies it. The validation ORDER
//! is the security property, and an order that lives in a socket handler is an
//! order nobody reviews.
//!
//! THE ORDER IS THE CONTRACT (`apps/coord/src/sync/sync-ws-upgrade.ts:124-213`):
//!
//! 1. the `Origin`, when present, must be allowed -- checked **first**, before
//!    the credential is even read;
//! 2. exactly two subprotocols, `roost-auth` then a non-empty credential;
//! 3. the credential must verify;
//! 4. the principal must be an account device or a worker -- a legacy
//!    self-hosted key is `404 not found`, not `401`;
//! 5. the `tab` query value, when present, must fit the byte bound.
//!
//! WHY ORIGIN IS CHECKED FIRST. A browser can be made to open a WebSocket to a
//! loopback address from any page it visits, and the cookie-free credential
//! rides in the subprotocol rather than in a header the page cannot set. So the
//! origin check is the only thing standing between a web page and a socket that
//! accepts commands, and it has to run before the credential is examined -- after
//! the credential, a rejected origin and a rejected credential look identical
//! from the page's point of view and the page cannot tell which to fix.
//!
//! WHY A LEGACY KEY IS 404 AND NOT 401. `legacy-self-hosted` is a real,
//! authorized browser key that predates accounts. It authenticates. What it does
//! not have is a **scope**: a Sync feed must know which resources a socket may
//! observe, and a key with no account has none. Answering 404 says "there is
//! nothing here", which is true; answering 401 would say "your key is not
//! valid", which is false and would send an operator to re-pair a key that
//! works everywhere else (`sync-ws-upgrade.ts:160-170`).

use roost_protocol::fingerprint::FINGERPRINT_HEX_LEN;

/// The subprotocol marker every Sync client offers first.
pub const SYNC_AUTH_SUBPROTOCOL: &str = "roost-auth";

/// The path the coordinator upgrades for the Sync stream.
pub const SYNC_WS_PATH: &str = "/ws/coord-sync";

/// The query value that enables the cumulative-ACK window.
pub const SYNC_QUERY_FLOW_V1: &str = "1";

/// The query value that additionally selects domain generations and socket
/// identity.
pub const SYNC_QUERY_V2: &str = "2";

/// The close code for a connection rejection, and the reason string beside it
/// (`apps/coord/src/sync/sync-ws-upgrade.ts:33-34`).
pub const CONNECTION_REJECTION_CLOSE_CODE: u16 = 1013;
pub const CONNECTION_REJECTION_REASON: &str = "connection rejected";

/// The origins an operator may declare. Compared exactly; a prefix or a regex
/// would admit an attacker's page served from a neighbouring port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicy {
    /// The coordinator's own operator-declared identity origin.
    pub public_url: Option<String>,
    /// The operator-declared browser front door.
    pub web_public_url: Option<String>,
    /// Extra bare origins allowed to make cross-origin calls.
    pub cors_allowed_origins: Vec<String>,
    /// The worker's loopback SPA origin, which dials this socket cross-origin.
    pub worker_local_ui_origin: String,
    /// The loopback bind, as `host:port`, when it is loopback. A local-first
    /// coordinator serves its canonical origin over HTTP, and the `http://`
    /// twin of the `Host` header is admitted only when the bind itself is
    /// loopback.
    pub loopback_bind: Option<String>,
    /// Whether to accept the plain-HTTP twin of the `Host` origin.
    pub relaxed_csp: bool,
}

/// What a Sync upgrade request presents. Query values are pre-split; the
/// endpoint legitimately takes a query, unlike the worker link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncUpgradeRequest {
    /// The request path.
    pub path: String,
    /// The `Origin` header, or `None` when the client sent none. A non-browser
    /// client sends none and is not thereby trusted; it simply is not the thing
    /// this check is against.
    pub origin: Option<String>,
    /// The `Host` header, which pairs with `origin` for the loopback rule.
    pub host: String,
    /// The `sec-websocket-protocol` entries in the order offered.
    pub offered_protocols: Vec<String>,
    /// The verified caller, or `None` when the credential did not verify.
    pub caller: Option<VerifiedSyncCaller>,
    /// The `tab` query value, if any.
    pub tab: Option<String>,
    /// The `since` query value, if any.
    pub since: Option<String>,
    /// The `flow` query value, if any.
    pub flow: Option<String>,
    /// The `sync_v` query value, if any.
    pub sync_v: Option<String>,
}

/// The facts a successful credential check established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSyncCaller {
    /// The key's fingerprint.
    pub fingerprint: String,
    /// The key's authorized label.
    pub label: String,
}

/// Why a Sync upgrade was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SyncUpgradeRefusal {
    /// The `Origin` was present and not allowed. `403 forbidden origin`.
    #[error("origin_rejected")]
    OriginRejected,
    /// The subprotocol envelope was not exactly `[roost-auth, credential]`.
    /// `401 unauthorized`.
    #[error("malformed_subprotocol")]
    MalformedSubprotocol,
    /// The credential did not verify. `401 unauthorized`.
    #[error("jwt_invalid")]
    JwtInvalid,
    /// The principal is `legacy-self-hosted`, which has no scope. `404 not
    /// found`.
    #[error("legacy_self_hosted")]
    LegacySelfHosted,
    /// The `tab` value exceeded its byte bound. `400` with
    /// [`CONNECTION_REJECTION_REASON`].
    #[error("tab_too_long")]
    TabTooLong,
}

impl SyncUpgradeRefusal {
    /// The status this refusal answers with.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            SyncUpgradeRefusal::OriginRejected => 403,
            SyncUpgradeRefusal::MalformedSubprotocol | SyncUpgradeRefusal::JwtInvalid => 401,
            SyncUpgradeRefusal::LegacySelfHosted => 404,
            SyncUpgradeRefusal::TabTooLong => 400,
        }
    }

    /// The body this refusal answers with.
    #[must_use]
    pub const fn body(self) -> &'static str {
        match self {
            SyncUpgradeRefusal::OriginRejected => "forbidden origin",
            SyncUpgradeRefusal::MalformedSubprotocol | SyncUpgradeRefusal::JwtInvalid => {
                "unauthorized"
            }
            SyncUpgradeRefusal::LegacySelfHosted => "not found",
            SyncUpgradeRefusal::TabTooLong => CONNECTION_REJECTION_REASON,
        }
    }
}

/// What a Sync socket may see and do once admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncScope {
    /// A worker's own fingerprint when the caller is a worker, or `None` for a
    /// browser, which sees the whole install.
    pub owner_worker_fp: Option<String>,
    /// Whether the socket is read-only. A worker principal is a firehose
    /// consumer over its own resources: it may ACK and subscribe, but it cannot
    /// issue a terminal view or input command
    /// (`apps/coord/src/sync/sync-ws-upgrade.ts:171-172,78-83`).
    pub read_only: bool,
    /// The exact browser tab id, or `None`. A v2 socket without `tab` is
    /// read-only by construction.
    pub tab_id: Option<String>,
    /// The `${fingerprint}:${tab}` key that owns socket-bound terminal view
    /// handles, or `None`.
    pub viewer_key: Option<String>,
    /// Whether the cumulative-ACK window is on: only for the exact `flow=1`.
    pub flow_control: bool,
    /// Whether domain generations are negotiated: only for the exact
    /// `flow=1&sync_v=2`.
    pub domain_generations: bool,
    /// The last durable event id this socket already holds, or zero.
    pub since_event_id: u64,
}

/// A Sync upgrade decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncUpgradeDecision {
    /// Admitted with this scope.
    Admitted {
        /// The verified caller.
        caller: VerifiedSyncCaller,
        /// What the socket may see and do.
        scope: SyncScope,
    },
    /// Refused, with the status and body the transport writes.
    Refused(SyncUpgradeRefusal),
}

/// The maximum UTF-8 bytes in a `tab` value.
///
/// 256 (`packages/protocol/src/ui-state.ts`, `UI_TAB_ID_MAX_UTF8_BYTES`). The
/// tab id is persisted as UI state keyed on it, so an unbounded value is an
/// unbounded primary key written by an anonymous-ish peer.
pub const TAB_ID_MAX_UTF8_BYTES: usize = 256;

/// Decide a Sync upgrade.
///
/// `principal` is the already-resolved principal kind, named explicitly so a
/// worker and a browser cannot be confused for one another here.
#[must_use]
pub fn admit_sync_upgrade(
    request: &SyncUpgradeRequest,
    policy: &OriginPolicy,
    principal: PrincipalKind,
) -> SyncUpgradeDecision {
    if request.path != SYNC_WS_PATH {
        return SyncUpgradeDecision::Refused(SyncUpgradeRefusal::MalformedSubprotocol);
    }

    // Rule 1: origin first, before the credential is read.
    if let Some(origin) = request.origin.as_deref()
        && !origin_is_allowed(origin, &request.host, policy)
    {
        return SyncUpgradeDecision::Refused(SyncUpgradeRefusal::OriginRejected);
    }

    // Rule 2.
    if request.offered_protocols.len() != 2
        || request.offered_protocols[0] != SYNC_AUTH_SUBPROTOCOL
        || request.offered_protocols[1].is_empty()
    {
        return SyncUpgradeDecision::Refused(SyncUpgradeRefusal::MalformedSubprotocol);
    }

    // Rule 3.
    let Some(caller) = request.caller.as_ref() else {
        return SyncUpgradeDecision::Refused(SyncUpgradeRefusal::JwtInvalid);
    };

    // Rule 4.
    let owner_worker_fp = match principal {
        PrincipalKind::AccountDevice => None,
        PrincipalKind::Worker(fingerprint) => Some(fingerprint),
        PrincipalKind::LegacySelfHosted => {
            return SyncUpgradeDecision::Refused(SyncUpgradeRefusal::LegacySelfHosted);
        }
    };

    // Rule 5. A blank `tab=` is treated as absent, matching v2's
    // `requestedTabId?.trim() ? requestedTabId : null`
    // (`sync-ws-upgrade.ts:173-174`).
    let tab_id = request
        .tab
        .as_deref()
        .map(str::trim)
        .filter(|tab| !tab.is_empty())
        .map(str::to_string);
    if tab_id
        .as_deref()
        .is_some_and(|tab| tab.len() > TAB_ID_MAX_UTF8_BYTES)
    {
        return SyncUpgradeDecision::Refused(SyncUpgradeRefusal::TabTooLong);
    }

    let read_only = owner_worker_fp.is_some();
    let flow_control = request.flow.as_deref() == Some(SYNC_QUERY_FLOW_V1);
    let domain_generations = flow_control && request.sync_v.as_deref() == Some(SYNC_QUERY_V2);

    SyncUpgradeDecision::Admitted {
        caller: caller.clone(),
        scope: SyncScope {
            owner_worker_fp,
            read_only,
            // A read-only socket keeps its `tab` for correlation but never
            // owns view handles: `tab_id: !readOnly ? tabId : null`
            // (`sync-ws-upgrade.ts:189`).
            tab_id: if read_only { None } else { tab_id.clone() },
            viewer_key: if !read_only && tab_id.is_some() {
                Some(format!(
                    "{}:{}",
                    caller.fingerprint,
                    tab_id.unwrap_or_default()
                ))
            } else {
                None
            },
            flow_control,
            domain_generations,
            since_event_id: request
                .since
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
        },
    }
}

/// Which kind of principal a verified key resolved to. The database answer,
/// named so this function can stay pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalKind {
    /// A browser key associated with an account device.
    AccountDevice,
    /// A machine: the worker fingerprint, which is also the key's.
    Worker(String),
    /// A pre-account browser key, which has no scope to admit a feed over.
    LegacySelfHosted,
}

fn origin_is_allowed(origin: &str, host: &str, policy: &OriginPolicy) -> bool {
    if policy.public_url.as_deref() == Some(origin)
        || policy.web_public_url.as_deref() == Some(origin)
        || policy
            .cors_allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
        || format!("https://{host}") == origin
        || origin == policy.worker_local_ui_origin
    {
        return true;
    }
    // A local-first coordinator serves its canonical browser origin over HTTP.
    // The `http://` twin of the `Host` header is admitted only when the bind
    // itself is loopback, so this cannot be used to open a socket to a
    // coordinator that is reachable from the network.
    if let Some(bind) = policy.loopback_bind.as_deref()
        && is_loopback_host_port(bind)
        && is_loopback_host_port(host)
        && format!("http://{host}") == origin
    {
        return true;
    }
    policy.relaxed_csp && format!("http://{host}") == origin
}

fn is_loopback_host_port(value: &str) -> bool {
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
        && !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// The viewer key a browser socket owns, for correlation with UI state.
#[must_use]
pub fn viewer_key(fingerprint: &str, tab_id: &str) -> String {
    format!("{fingerprint}:{tab_id}")
}

/// A fingerprint's rendered width, re-exported so the Sync layer's bound
/// arithmetic reads against the same constant the renderer uses.
#[must_use]
pub const fn fingerprint_hex_len() -> usize {
    FINGERPRINT_HEX_LEN
}
