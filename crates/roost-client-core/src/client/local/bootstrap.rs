//! The worker-served bootstrap: which coordinator a page a WORKER served should
//! dial, and the rules an answer must satisfy before a single RPC is retargeted
//! at the machine that happened to answer it.
//!
//! Fail-closed by construction. Anything unexpected — a 404, an HTML body, a
//! missing field, a relative URL — leaves the page on its own origin, because
//! the alternative silently sends every coordinator call to the worker instead
//! of to the coordinator. One parser serves both the serving origin and a
//! probed door, because two parsers are how the two would disagree on what
//! counts as an answer.
//!
//! Ported from `apps/web/src/client/carriers/localBootstrap.ts` and the
//! `coordBase` it feeds (`apps/web/src/client/rpc/connect.ts:69-93`). Storage
//! and `location` are host inputs here, passed as arguments, so the rule is
//! provable without a document.

use serde_json::Value;

/// The path only a worker's local UI door answers.
pub const LOCAL_BOOTSTRAP_PATH: &str = "/api/local-bootstrap";

/// How long the serving origin may take before the page stops waiting for it.
///
/// Bounded so a hung origin cannot stall SPA startup; the answer is a routing
/// decision, not a dependency, and the page is already usable without it.
pub const BOOTSTRAP_TIMEOUT_MS: u64 = 2_000;

/// The key an operator sets to record that this page is self-hosted, and so that
/// a coordinator override is honoured at all.
pub const DEPLOYMENT_MODE_KEY: &str = "roost.deploymentMode";

/// The value of [`DEPLOYMENT_MODE_KEY`] that enables a stored coordinator
/// override.
pub const DEPLOYMENT_MODE_SELF_HOSTED: &str = "self-hosted";

/// The key a stored coordinator base is kept under.
pub const COORDINATOR_OVERRIDE_KEY: &str = "roost.coordinatorUrl";

/// What a worker serving this document says about the coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBootstrap {
    /// Where the coordinator is. Absolute `http` or `https` only.
    pub coordinator_url: String,
    /// The worker whose loopback door is on this origin.
    pub worker_fingerprint: String,
}

impl LocalBootstrap {
    /// Validate an answer from either the serving origin or a probed door.
    ///
    /// A relative or non-HTTP `coordinatorUrl` is refused rather than resolved
    /// against the page: a relative base would retarget every coordinator RPC
    /// at this page's own origin, which is the WORKER, and the failure would be
    /// silent.
    pub fn parse(payload: &str) -> Result<Self, BootstrapRefusal> {
        let value: Value = serde_json::from_str(payload).map_err(|_| BootstrapRefusal::NotJson)?;
        let Value::Object(fields) = value else {
            return Err(BootstrapRefusal::NotAnObject);
        };
        let coordinator_url = string_field(&fields, "coordinatorUrl")?;
        let worker_fingerprint = string_field(&fields, "workerFingerprint")?;
        if !is_http_url(&coordinator_url) {
            return Err(BootstrapRefusal::NotHttpUrl);
        }
        Ok(Self {
            coordinator_url,
            worker_fingerprint,
        })
    }
}

/// One required string field, trimmed, and non-empty.
fn string_field(
    fields: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<String, BootstrapRefusal> {
    let Some(Value::String(raw)) = fields.get(name) else {
        return Err(BootstrapRefusal::MissingField(name.to_string()));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BootstrapRefusal::EmptyField(name.to_string()));
    }
    Ok(trimmed.to_string())
}

/// Whether a value is an absolute `http`/`https` URL with a host.
///
/// Deliberately stricter than a prefix test: `http://` with nothing after it is
/// not a base a client can dial, and a value carrying a scheme this client does
/// not speak must not be "fixed" into one that looks like it does.
fn is_http_url(value: &str) -> bool {
    let Some(authority) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    !authority.is_empty() && !authority.contains(['/', '?', '#', ' '])
}

/// Why an answer is not a usable bootstrap. Each member is one thing the parser
/// checked, so a host's log says which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapRefusal {
    /// The body was not JSON at all.
    NotJson,
    /// The body was JSON, and not an object.
    NotAnObject,
    /// A required field was absent, or was not a string.
    MissingField(String),
    /// A required field was present and blank.
    EmptyField(String),
    /// `coordinatorUrl` was relative, or not `http`/`https`.
    NotHttpUrl,
    /// The request never completed — a refused connection, an aborted timeout.
    Unreachable,
    /// The origin answered, and not with a bootstrap.
    Status,
}

impl BootstrapRefusal {
    /// The string a host records.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotJson => "not json",
            Self::NotAnObject => "not an object",
            Self::MissingField(_) => "missing field",
            Self::EmptyField(_) => "empty field",
            Self::NotHttpUrl => "coordinator url is not absolute http",
            Self::Unreachable => "serving origin did not answer",
            Self::Status => "serving origin answered without a bootstrap",
        }
    }
}

/// What a probe of the serving origin produced.
///
/// There is no error arm a caller must handle and nothing here throws: this
/// runs before the SPA graph loads, and a probe that cannot answer leaves the
/// page exactly where it was rather than failing startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapOutcome {
    /// The page was served by a worker, and this is what it said.
    Served(LocalBootstrap),
    /// It was not, and this is why.
    NotWorkerServed(BootstrapRefusal),
}

/// Read one probe of the SERVING origin.
///
/// `status` is `None` when the request never completed; that is the only way a
/// caller learns the origin was unreachable, and it is deliberately not the
/// same answer as a status it did not like.
pub fn read_serving_origin(status: Option<u16>, payload: &str) -> BootstrapOutcome {
    let Some(status) = status else {
        return BootstrapOutcome::NotWorkerServed(BootstrapRefusal::Unreachable);
    };
    if !(200..300).contains(&status) {
        return BootstrapOutcome::NotWorkerServed(BootstrapRefusal::Status);
    }
    match LocalBootstrap::parse(payload) {
        Ok(bootstrap) => BootstrapOutcome::Served(bootstrap),
        Err(refusal) => BootstrapOutcome::NotWorkerServed(refusal),
    }
}

/// The coordinator base every Connect call goes to, as `connect.ts` resolves it.
///
/// A worker-served bootstrap wins outright: that page was handed to it BY a
/// worker, and the worker's own answer is the only one that can say where its
/// coordinator is. Failing that, a stored override is honoured only for a page
/// that recorded itself as self-hosted — which is what stops a stale override
/// on someone else's page from pointing their calls at a stranger's machine.
pub fn coordinator_base(
    served_by_worker: Option<&LocalBootstrap>,
    deployment_mode: Option<&str>,
    stored_override: Option<&str>,
) -> String {
    if let Some(bootstrap) = served_by_worker {
        return bootstrap.coordinator_url.clone();
    }
    if deployment_mode != Some(DEPLOYMENT_MODE_SELF_HOSTED) {
        return String::new();
    }
    stored_override.unwrap_or_default().to_string()
}

/// The origin Connect calls actually go to: the base, or the page's own origin
/// when there is no base at all.
pub fn coordinator_base_url(base: &str, page_origin: &str) -> String {
    if base.is_empty() {
        return page_origin.to_string();
    }
    base.to_string()
}
