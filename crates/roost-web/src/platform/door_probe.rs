//! Asking a machine's own worker door whether it is there, and what it is.
//!
//! Owned by `platform`, called by `pump::carrier_dial` before a minted grant is
//! spent. It is the only file in this tree that performs the bootstrap GET, and
//! it moves NO decision: whether an answer adopts a door is
//! `client::local::discovery::DoorDiscovery`, and re-reading the status here
//! would be a second answer to "did that origin serve a worker".
//!
//! The probe is bounded by `client::local::discovery::DOOR_PROBE_TIMEOUT_MS` and
//! reports an unreachable origin as `status: None` rather than as a refusal,
//! because those are different facts: a 404 means something answered and is not
//! a worker, while `None` means nothing answered at all, and a caller that
//! conflated them would report "no worker on this machine" for a machine whose
//! worker was merely asleep.
//!
//! Target-total in the way every platform file here is: a build with no browser
//! reports every origin as unreachable, which is the truth — it has no network
//! stack of its own to have asked.

use roost_client_core::client::local::bootstrap::LocalBootstrap;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::local::bootstrap::{
    BootstrapOutcome, COORDINATOR_OVERRIDE_KEY, DEPLOYMENT_MODE_KEY, LOCAL_BOOTSTRAP_PATH,
    read_serving_origin,
};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::local::discovery::DOOR_PROBE_TIMEOUT_MS;
use roost_client_core::client::local::discovery::LOCAL_WORKER_ORIGIN_KEY;
use std::cell::RefCell;

/// What one bootstrap request produced.
///
/// `status` is `None` when the request never completed, which is the only way a
/// caller learns the origin was unreachable rather than unhelpful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAnswer {
    /// The HTTP status, or `None` when the request did not complete.
    pub status: Option<u16>,
    /// The body as text, empty when there was none.
    pub body: String,
}

impl BootstrapAnswer {
    /// The answer for an origin this build could not reach.
    ///
    /// A named constructor rather than a literal at each call site: three of
    /// them exist, and an arm that built `{status: Some(0), ..}` instead would
    /// be a machine that answered.
    pub fn unreachable() -> Self {
        Self {
            status: None,
            body: String::new(),
        }
    }
}

/// The origin serving this document.
#[cfg(target_arch = "wasm32")]
pub fn page_origin() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

/// The origin serving this document.
#[cfg(not(target_arch = "wasm32"))]
pub fn page_origin() -> String {
    String::new()
}

/// The operator's door override, verbatim from storage, if they set one.
///
/// Read rather than validated: `candidate_origin` is what decides whether a
/// stored value is a bare origin this client will dial, and reading it here
/// without judging it keeps that decision in one place.
pub fn stored_operator_origin() -> Option<String> {
    let storage = web_sys::window()?.local_storage().ok()??;
    storage.get_item(LOCAL_WORKER_ORIGIN_KEY).ok().flatten()
}

/// One GET against a bootstrap path, bounded by the discovery deadline.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_bootstrap(url: &str) -> BootstrapAnswer {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen_futures::JsFuture;

    let unreachable = BootstrapAnswer::unreachable;
    let Some(window) = web_sys::window() else {
        return unreachable();
    };
    // Bounded by the discovery deadline rather than left open: a machine whose
    // worker is stopped has a loopback port that accepts nothing, and a request
    // that never completes leaves the page with no door AND no answer for as
    // long as the operator waited. `AbortSignal::timeout` rather than a timer
    // this crate owns, because a timer that fires into a dropped `Closure` is a
    // throw inside a browser callback.
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_signal(Some(&web_sys::AbortSignal::timeout_with_f64(
        DOOR_PROBE_TIMEOUT_MS as f64,
    )));
    let Ok(request) = web_sys::Request::new_with_str_and_init(url, &init) else {
        return unreachable();
    };
    let Ok(value) = JsFuture::from(window.fetch_with_request(&request)).await else {
        return unreachable();
    };
    let Ok(response) = value.dyn_into::<web_sys::Response>() else {
        return unreachable();
    };
    let status = response.status();
    let body = match response.text() {
        Ok(text) => JsFuture::from(text)
            .await
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    BootstrapAnswer {
        status: Some(status),
        body,
    }
}

/// One GET against a bootstrap path, bounded by the discovery deadline.
#[cfg(not(target_arch = "wasm32"))]
pub async fn fetch_bootstrap(_url: &str) -> BootstrapAnswer {
    BootstrapAnswer::unreachable()
}

thread_local! {
    /// The answer the SERVING origin gave before the application graph loaded.
    ///
    /// v2 holds the same fact in a module-level variable that `connect.ts` reads
    /// synchronously at module scope (`localBootstrap.ts:18-29`), because the
    /// graph may not start before the routing decision is final: a page a worker
    /// served would otherwise point every coordinator RPC at that worker.
    static PRIMED: RefCell<Option<LocalBootstrap>> = const { RefCell::new(None) };
}

/// Ask the serving origin who it is, once, before the graph loads.
///
/// Never throws and never outlasts the discovery deadline: the coordinator's own
/// 404 on this path is the ordinary "a coordinator served me" answer, and a page
/// left on its own origin is the fail-closed result, not an error.
#[cfg(target_arch = "wasm32")]
pub async fn prime_serving_bootstrap() {
    let origin = page_origin();
    if origin.is_empty() {
        return;
    }
    let answer = fetch_bootstrap(&format!("{origin}{LOCAL_BOOTSTRAP_PATH}")).await;
    let bootstrap = match read_serving_origin(answer.status, &answer.body) {
        BootstrapOutcome::Served(bootstrap) => Some(bootstrap),
        BootstrapOutcome::NotWorkerServed(refusal) => {
            tracing::info!(
                target: "door",
                reason = refusal.reason(),
                "the serving origin is not a worker door; the page keeps its own origin"
            );
            None
        }
    };
    let worker_fp = bootstrap
        .as_ref()
        .map(|bootstrap| bootstrap.worker_fingerprint.clone());
    PRIMED.with(|primed| *primed.borrow_mut() = bootstrap);
    tracing::info!(
        target: "door",
        worker_fingerprint = ?worker_fp,
        "serving origin asked whether it is a worker door"
    );
}

/// What the serving origin said, when it said a worker served this document.
pub fn primed_serving_bootstrap() -> Option<LocalBootstrap> {
    PRIMED.with(|primed| primed.borrow().clone())
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn prime_serving_bootstrap() {}

/// The coordinator every Connect call goes to, resolved as `connect.ts` does.
///
/// Three rungs, in the order that file names them: a worker-served bootstrap
/// wins outright, then a stored override for a page that recorded itself
/// self-hosted, then the page's own origin. The override rung is gated on the
/// deployment mode there and here because a stale override on someone else's
/// page would otherwise aim their calls at a stranger's machine.
#[cfg(target_arch = "wasm32")]
pub fn coordinator_base_url_for_page() -> String {
    let stored = |key: &str| {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(key).ok().flatten())
    };
    let base = roost_client_core::client::local::coordinator_base(
        primed_serving_bootstrap().as_ref(),
        stored(DEPLOYMENT_MODE_KEY).as_deref(),
        stored(COORDINATOR_OVERRIDE_KEY).as_deref(),
    );
    roost_client_core::client::local::coordinator_base_url(&base, &page_origin())
}

/// The coordinator base for a build with no document, which has no origin and so
/// no coordinator to dial.
#[cfg(not(target_arch = "wasm32"))]
pub fn coordinator_base_url_for_page() -> String {
    String::new()
}
