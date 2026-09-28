//! The browser half of the tab identity claim: `sessionStorage`, a Web Lock
//! held for the document lifetime, and the `BroadcastChannel` fallback kept
//! open to answer later probes.
//!
//! Ported from `apps/web/src/client/auth/tab-id.ts`; the decision table is
//! `roost_client_core::client::auth::tab_id::TabIdentity`, which this file only
//! performs steps for. Called once per document by `pump::boot` before the
//! first transport. `navigator.locks` is reached through `Reflect` because
//! web-sys gates `LockManager` behind `web_sys_unstable_apis`.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Object, Promise, Reflect};
use roost_client_core::client::auth::tab_id::message::ClaimMessage;
use roost_client_core::client::auth::tab_id::{
    ArbitrationPrimitives, BROADCAST_PROBE_WAIT_MS, ClaimStep, LockAttempt, TAB_ID_CLAIM_CHANNEL,
    TabIdentity,
};
use roost_client_core::client::auth::{CeremonyError, RandomSource};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{BroadcastChannel, MessageEvent};

use crate::platform::clock::BrowserClock;
use crate::platform::storage::SessionStorageKeyValueStore;

/// The document's claimed tab id. Owns the claim channel when the broadcast
/// fallback claimed it; keep it for the document lifetime so later
/// duplicated documents are told this one still owns the id.
#[derive(Debug)]
pub struct ClaimedTabId {
    id: String,
    _channel: Option<ClaimChannel>,
}

impl ClaimedTabId {
    /// The id every transport presents.
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Claim this document's tab id (v2 `claimTabIdentity`).
pub async fn claim_document_tab_id() -> ClaimedTabId {
    let identity = Rc::new(RefCell::new(TabIdentity::new(
        Rc::new(SessionStorageKeyValueStore::new()),
        Rc::new(CryptoRandomSource),
        Rc::new(BrowserClock::new()),
    )));
    let locks = web_locks();
    let primitives = ArbitrationPrimitives {
        web_locks: locks.is_some(),
        broadcast_channel: Reflect::get(&js_sys::global(), &"BroadcastChannel".into())
            .is_ok_and(|constructor| constructor.is_function()),
    };
    let mut channel: Option<ClaimChannel> = None;
    let mut step = identity.borrow_mut().begin_claim(primitives);
    loop {
        let next = match step {
            ClaimStep::RequestLock { name } => {
                let attempt = match &locks {
                    Some(locks) => request_lock(locks, &name).await,
                    None => LockAttempt::Unavailable,
                };
                identity.borrow_mut().lock_answered(attempt)
            }
            ClaimStep::Probe { message } => {
                if channel.is_none() {
                    channel = ClaimChannel::open(&identity);
                }
                match &channel {
                    Some(open) => open.probe(&identity, &message).await,
                    None => identity.borrow_mut().probe_unavailable(),
                }
            }
            ClaimStep::Claimed { id, keep_channel } => {
                return ClaimedTabId {
                    id,
                    _channel: channel.filter(|_| keep_channel),
                };
            }
        };
        step = match next {
            Some(next) => next,
            None => {
                // Only a host that answered a step it was not asked can land
                // here; keep the current id rather than stall startup.
                tracing::error!(target: "auth", "tab identity claim lost its step; keeping the current id");
                let id = identity.borrow_mut().tab_id();
                return ClaimedTabId { id, _channel: None };
            }
        };
    }
}

/// `navigator.locks`, when this document exposes it.
fn web_locks() -> Option<JsValue> {
    let navigator = web_sys::window()?.navigator();
    Reflect::get(&navigator, &"locks".into())
        .ok()
        .filter(|locks| locks.is_object())
}

/// One `ifAvailable` exclusive request. A granted callback returns a promise
/// that never settles, which holds the lock until the document goes away.
async fn request_lock(locks: &JsValue, name: &str) -> LockAttempt {
    let Some(request) = Reflect::get(locks, &"request".into())
        .ok()
        .and_then(|request| request.dyn_into::<Function>().ok())
    else {
        return LockAttempt::Unavailable;
    };
    let options = Object::new();
    if Reflect::set(&options, &"mode".into(), &"exclusive".into()).is_err()
        || Reflect::set(&options, &"ifAvailable".into(), &JsValue::TRUE).is_err()
    {
        return LockAttempt::Unavailable;
    }
    let mut resolver: Option<Function> = None;
    let answered = Promise::new(&mut |resolve: Function, _reject: Function| {
        resolver = Some(resolve);
    });
    let Some(resolve) = resolver else {
        return LockAttempt::Unavailable;
    };
    let granted = resolve.clone();
    let callback = Closure::once_into_js(move |lock: JsValue| -> JsValue {
        if lock.is_null() || lock.is_undefined() {
            let _ = granted.call1(&JsValue::NULL, &"occupied".into());
            return JsValue::UNDEFINED;
        }
        let _ = granted.call1(&JsValue::NULL, &"acquired".into());
        Promise::new(&mut |_resolve: Function, _reject: Function| {}).into()
    });
    let pending = match request.call3(locks, &name.into(), &options, &callback) {
        Ok(pending) => pending,
        Err(_) => return LockAttempt::Unavailable,
    };
    // A rejection before the callback ran (SecurityError, unsupported
    // options) is an unavailable API; after it, `resolve` has settled already.
    if let Some(catch) = Reflect::get(&pending, &"catch".into())
        .ok()
        .and_then(|catch| catch.dyn_into::<Function>().ok())
    {
        let refused = Closure::once_into_js(move |_error: JsValue| {
            let _ = resolve.call1(&JsValue::NULL, &"unavailable".into());
        });
        let _ = catch.call1(&pending, &refused);
    }
    match JsFuture::from(answered)
        .await
        .ok()
        .and_then(|answer| answer.as_string())
        .as_deref()
    {
        Some("acquired") => LockAttempt::Acquired,
        Some("occupied") => LockAttempt::Occupied,
        _ => LockAttempt::Unavailable,
    }
}

/// The open claim channel, its listener, and the pending probe's wake.
struct ClaimChannel {
    channel: BroadcastChannel,
    wake: Rc<ProbeWake>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl std::fmt::Debug for ClaimChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClaimChannel")
            .finish_non_exhaustive()
    }
}

/// How a received message wakes the probe it settled.
#[derive(Default)]
struct ProbeWake {
    resolve: RefCell<Option<Function>>,
    step: RefCell<Option<ClaimStep>>,
}

impl ClaimChannel {
    fn open(identity: &Rc<RefCell<TabIdentity>>) -> Option<Self> {
        let channel = BroadcastChannel::new(TAB_ID_CLAIM_CHANNEL).ok()?;
        let wake = Rc::new(ProbeWake::default());
        let on_message = {
            let (identity, wake, channel) =
                (Rc::clone(identity), Rc::clone(&wake), channel.clone());
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let Some(message) = read_message(&event.data()) else {
                    return;
                };
                let Ok(mut claim) = identity.try_borrow_mut() else {
                    tracing::warn!(target: "auth", "claim message arrived mid-step; dropped");
                    return;
                };
                let outcome = claim.message_received(message);
                drop(claim);
                if let Some(reply) = outcome.reply {
                    // A closed channel answers nothing; the document is going.
                    let _ = post(&channel, &reply);
                }
                if let Some(step) = outcome.step {
                    *wake.step.borrow_mut() = Some(step);
                    if let Some(resolve) = wake.resolve.borrow_mut().take() {
                        let _ = resolve.call0(&JsValue::NULL);
                    }
                }
            })
        };
        channel.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        Some(Self {
            channel,
            wake,
            _on_message: on_message,
        })
    }

    /// Post one probe and wait for a reply that settles it or the window.
    async fn probe(
        &self,
        identity: &Rc<RefCell<TabIdentity>>,
        message: &ClaimMessage,
    ) -> Option<ClaimStep> {
        let mut armed = false;
        let settled = Promise::new(&mut |resolve: Function, _reject: Function| {
            armed = web_sys::window().is_some_and(|window| {
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        &resolve,
                        i32::try_from(BROADCAST_PROBE_WAIT_MS).unwrap_or(i32::MAX),
                    )
                    .is_ok()
            });
            *self.wake.resolve.borrow_mut() = Some(resolve);
        });
        if !armed || post(&self.channel, message).is_err() {
            self.wake.resolve.borrow_mut().take();
            return identity.borrow_mut().probe_unavailable();
        }
        let _ = JsFuture::from(settled).await;
        self.wake.resolve.borrow_mut().take();
        let delivered = self.wake.step.borrow_mut().take();
        delivered.or_else(|| identity.borrow_mut().probe_timed_out(message.nonce()))
    }
}

impl Drop for ClaimChannel {
    fn drop(&mut self) {
        self.channel.set_onmessage(None);
        self.channel.close();
    }
}

/// `{ type, id, nonce }` read off a message's data; anything else is ignored.
fn read_message(data: &JsValue) -> Option<ClaimMessage> {
    if !data.is_object() {
        return None;
    }
    let field = |name: &str| Reflect::get(data, &name.into()).ok()?.as_string();
    ClaimMessage::parse(field("type").as_deref(), field("id"), field("nonce"))
}

fn post(channel: &BroadcastChannel, message: &ClaimMessage) -> Result<(), JsValue> {
    let body = Object::new();
    Reflect::set(&body, &"type".into(), &message.kind().into())?;
    Reflect::set(&body, &"id".into(), &message.id().into())?;
    Reflect::set(&body, &"nonce".into(), &message.nonce().into())?;
    channel.post_message(&body)
}

/// `crypto.getRandomValues`, or v2's `Math.random` fill where it is missing.
#[derive(Debug)]
struct CryptoRandomSource;

impl RandomSource for CryptoRandomSource {
    fn fill_bytes(&self, out: &mut [u8]) -> Result<(), CeremonyError> {
        let filled = web_sys::window()
            .and_then(|window| window.crypto().ok())
            .is_some_and(|crypto| crypto.get_random_values_with_u8_array(out).is_ok());
        if !filled {
            for byte in out.iter_mut() {
                *byte = (js_sys::Math::random() * 256.0) as u8;
            }
        }
        Ok(())
    }
}
